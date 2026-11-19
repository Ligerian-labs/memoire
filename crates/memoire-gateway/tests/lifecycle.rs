//! What happens without a request: the sweeper expires burstable
//! databases, persistent ones are snapshotted, restored and pruned, export
//! and import move a database, and the startup reconciler repairs drift.
//! The intervals are seconds here so each can be watched.
mod common;

use common::*;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

const SETTINGS: &str = r#"data_dir = "data"
min_idle_ttl_seconds = 1
max_database_bytes = 1048576
idle_close_seconds = 1
last_used_flush_seconds = 1
sweep_interval_seconds = 1
snapshot_interval_seconds = 2
snapshot_retention_seconds = 8"#;

const JOBS: &str = r#"[jobs]
poll_interval_ms = 200"#;

struct Team<'a> {
    gateway: &'a Gateway,
    auth: String,
    team: String,
}

impl Team<'_> {
    fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.gateway.request(
            method,
            &format!("/v1/memoire{path}"),
            &self.headers(),
            body.as_ref(),
        )
    }

    fn headers(&self) -> Vec<(&str, &str)> {
        vec![
            ("authorization", self.auth.as_str()),
            ("merezha-organization", self.team.as_str()),
        ]
    }

    fn create(&self, body: Value) -> Value {
        let (status, body) = self.call("POST", "/databases", Some(body));
        assert_eq!(status, 201, "{body}");
        body
    }

    fn sql(&self, name: &str, mode: &str, sql: &str) -> (u16, Value) {
        self.call(
            "POST",
            &format!("/databases/{name}/query"),
            Some(json!({"mode": mode, "statements": [{"sql": sql}]})),
        )
    }

    fn rows(&self, name: &str, sql: &str) -> Value {
        let (status, body) = self.sql(name, "read", sql);
        assert_eq!(status, 200, "{body}");
        body["results"][0]["rows"].clone()
    }
}

fn code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("")
}

fn file(dir: &Path, team: &str, id: &str) -> PathBuf {
    dir.join("data/teams")
        .join(team)
        .join(format!("{id}.sqlite"))
}

/// Waits until `done` holds, at most `seconds`.
fn eventually(seconds: u64, what: &str, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(
            started.elapsed() < Duration::from_secs(seconds),
            "never: {what}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// A scheduled snapshot that surely holds the writes made before the call:
/// the newest of two taken since. Snapshot runs are serialized, so the
/// second one started after the first finished, which was after the call;
/// the first alone may have been copying while the writes committed.
fn after_writes(list: &dyn Fn() -> Vec<Value>) -> Value {
    let before: Vec<Value> = list().iter().map(|s| s["id"].clone()).collect();
    let mut newest = Value::Null;
    eventually(30, "two scheduled snapshots after the writes", || {
        let fresh: Vec<Value> = list()
            .into_iter()
            .filter(|s| s["reason"] == "scheduled" && !before.contains(&s["id"]))
            .collect();
        // The list is newest first.
        newest = fresh.first().cloned().unwrap_or(Value::Null);
        fresh.len() >= 2
    });
    newest
}

fn start(dir: &Path, db: &merezha_postgres::TestDatabase) -> (Gateway, PathBuf) {
    let config = config_with(dir, SETTINGS, JOBS);
    (Gateway::start(&config, db), config)
}

#[test]
fn burstable_databases_expire_unless_used() {
    let Some(db) = database() else { return };
    let dir = tempfile::tempdir().unwrap();
    let (gateway, _) = start(dir.path(), &db);
    let owner = register(&gateway, "sweep@memoire.test");
    let team_id = create_team(&gateway, &owner, "sweep-team");
    let team = Team {
        gateway: &gateway,
        auth: owner.bearer(),
        team: team_id.clone(),
    };

    let scratch =
        team.create(json!({"name": "scratch", "kind": "burstable", "idle_ttl_seconds": 2}));
    let kept = team.create(json!({"name": "kept", "kind": "burstable", "idle_ttl_seconds": 3}));
    let notes = team.create(json!({"name": "notes", "kind": "persistent"}));
    assert_eq!(team.sql("scratch", "write", "CREATE TABLE t (x)").0, 200);
    let path = file(dir.path(), &team_id, scratch["id"].as_str().unwrap());
    assert!(path.is_file());

    // `kept` is queried every second: its idle clock keeps restarting.
    let started = Instant::now();
    eventually(20, "scratch expired", || {
        if started.elapsed() < Duration::from_secs(8) {
            assert_eq!(team.sql("kept", "read", "SELECT 1").0, 200);
        }
        let (_, body) = team.call("GET", "/databases/scratch", None);
        body["state"] == "expired"
    });
    // The sweeper removes the file right after the catalog accepts the
    // expiry.
    eventually(5, "the expired file removed", || !path.exists());
    std::thread::sleep(Duration::from_secs(2));
    assert!(!path.exists(), "and it stays removed across sweeps");
    let (_, body) = team.call("GET", "/databases/scratch", None);
    assert_eq!(body["size_bytes"], Value::Null);
    let (status, body) = team.sql("scratch", "read", "SELECT 1");
    assert_eq!((status, code(&body)), (410, "database_expired"), "{body}");
    let (status, body) = team.call("POST", "/databases/scratch/extend", Some(json!({})));
    assert_eq!((status, code(&body)), (410, "database_expired"));
    let (_, kept_now) = team.call("GET", "/databases/kept", None);
    assert_eq!(kept_now["state"], "active", "{kept_now} {kept}");

    // The list hides expired entries unless asked.
    let names = |query: &str| -> Vec<String> {
        let (_, page) = team.call("GET", &format!("/databases{query}"), None);
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert!(!names("").contains(&"scratch".to_owned()));
    assert!(names("?state=all").contains(&"scratch".to_owned()));

    // Persistent databases never expire; an expired entry can be deleted
    // and its name taken again.
    let (_, notes_now) = team.call("GET", "/databases/notes", None);
    assert_eq!(
        (notes_now["state"].clone(), notes_now["id"].clone()),
        (json!("active"), notes["id"].clone())
    );
    assert_eq!(team.call("DELETE", "/databases/scratch", None).0, 204);
    let again = team.create(json!({"name": "scratch", "kind": "burstable"}));
    assert_ne!(again["id"], scratch["id"]);
}

#[test]
fn persistent_databases_are_snapshotted_restored_and_pruned() {
    let Some(db) = database() else { return };
    let dir = tempfile::tempdir().unwrap();
    let (gateway, _) = start(dir.path(), &db);
    let owner = register(&gateway, "snap@memoire.test");
    let team_id = create_team(&gateway, &owner, "snap-team");
    let team = Team {
        gateway: &gateway,
        auth: owner.bearer(),
        team: team_id.clone(),
    };
    team.create(json!({"name": "notes", "kind": "persistent"}));
    team.create(json!({"name": "scratch", "kind": "burstable"}));
    team.sql("notes", "write", "CREATE TABLE t (v TEXT)");
    team.sql("notes", "write", "INSERT INTO t VALUES ('v1')");

    let list = || {
        let (status, body) = team.call("GET", "/databases/notes/snapshots", None);
        assert_eq!(status, 200, "{body}");
        body["items"].as_array().unwrap().clone()
    };
    let first = after_writes(&list);
    assert!(first["id"].as_str().unwrap().starts_with("snap_"));
    assert!(first["size_bytes"].as_u64().unwrap() > 0);
    team.sql("notes", "write", "UPDATE t SET v = 'v2'");
    assert_eq!(team.rows("notes", "SELECT v FROM t"), json!([["v2"]]));

    // Restore needs `snapshots`; a key without it is refused.
    let (_, key) = api_key(
        &gateway,
        &owner,
        &team_id,
        &["memoire.databases.read", "memoire.databases.write"],
        None,
    );
    let writer = Team {
        gateway: &gateway,
        auth: format!("Bearer {}", key["key"].as_str().unwrap()),
        team: team_id.clone(),
    };
    let restore = format!(
        "/databases/notes/snapshots/{}/restore",
        first["id"].as_str().unwrap()
    );
    let (status, body) = writer.call("POST", &restore, None);
    assert_eq!((status, code(&body)), (403, "unauthorized"), "{body}");
    let (status, body) = team.call("POST", &restore, None);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["name"], "notes");
    assert_eq!(team.rows("notes", "SELECT v FROM t"), json!([["v1"]]));
    let reasons: Vec<Value> = list().iter().map(|s| s["reason"].clone()).collect();
    assert!(reasons.contains(&json!("pre_restore")), "{reasons:?}");
    // The pre-restore snapshot holds v2.
    let pre = list()
        .into_iter()
        .find(|s| s["reason"] == "pre_restore")
        .unwrap();
    team.call(
        "POST",
        &format!(
            "/databases/notes/snapshots/{}/restore",
            pre["id"].as_str().unwrap()
        ),
        None,
    );
    assert_eq!(team.rows("notes", "SELECT v FROM t"), json!([["v2"]]));

    let (status, body) = team.call(
        "POST",
        &format!("/databases/notes/snapshots/snap_{:032x}/restore", 7),
        None,
    );
    assert_eq!((status, code(&body)), (404, "snapshot_not_found"));
    let (status, body) = team.call("GET", "/databases/scratch/snapshots", None);
    assert_eq!((status, code(&body)), (409, "not_persistent"));

    // Older than the retention, the first snapshot is pruned with its file.
    let first_id = first["id"].as_str().unwrap().to_owned();
    let notes_id = {
        let (_, body) = team.call("GET", "/databases/notes", None);
        body["id"].as_str().unwrap().to_owned()
    };
    let first_file = dir
        .path()
        .join("data/snapshots")
        .join(&team_id)
        .join(&notes_id)
        .join(format!("{first_id}.sqlite"));
    assert!(first_file.is_file(), "{first_file:?}");
    eventually(30, "the first snapshot pruned", || {
        !list().iter().any(|s| s["id"] == first_id.as_str())
    });
    assert!(!first_file.exists(), "pruning removes the file");

    // Deleting the database removes its snapshots.
    let dir_of = dir
        .path()
        .join("data/snapshots")
        .join(&team_id)
        .join(&notes_id);
    assert!(dir_of.is_dir());
    assert_eq!(team.call("DELETE", "/databases/notes", None).0, 204);
    assert!(!dir_of.exists());
}

#[test]
fn export_and_import_move_a_database() {
    let Some(db) = database() else { return };
    let dir = tempfile::tempdir().unwrap();
    let (gateway, _) = start(dir.path(), &db);
    let owner = register(&gateway, "move@memoire.test");
    let team_id = create_team(&gateway, &owner, "move-team");
    let team = Team {
        gateway: &gateway,
        auth: owner.bearer(),
        team: team_id.clone(),
    };
    team.create(json!({"name": "source", "kind": "persistent"}));
    team.sql(
        "source",
        "write",
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)",
    );
    team.sql(
        "source",
        "write",
        "INSERT INTO notes (body) VALUES ('hello'), ('world')",
    );

    let (status, head, bytes) = gateway.raw(
        "GET",
        "/v1/memoire/databases/source/export",
        &team.headers(),
        None,
    );
    assert_eq!(status, 200, "{head}");
    let head = head.to_ascii_lowercase();
    assert!(
        head.contains("content-type: application/vnd.sqlite3"),
        "{head}"
    );
    assert!(
        head.contains("content-disposition: attachment; filename=\"source.sqlite\""),
        "{head}"
    );
    assert_eq!(&bytes[..16], b"SQLite format 3\0");
    let exported = dir.path().join("exported.sqlite");
    std::fs::write(&exported, &bytes).unwrap();
    let copy = rusqlite::Connection::open(&exported).unwrap();
    let count: i64 = copy
        .query_row("SELECT count(*) FROM notes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
    drop(copy);

    let put = |name: &str, content_type: &str, body: &[u8]| {
        let (status, _, raw) = gateway.raw(
            "PUT",
            &format!("/v1/memoire/databases/{name}/import"),
            &team.headers(),
            Some((content_type, body)),
        );
        (
            status,
            serde_json::from_slice::<Value>(&raw).unwrap_or(Value::Null),
        )
    };
    team.create(json!({"name": "target", "kind": "burstable"}));
    let (status, body) = put("target", "application/vnd.sqlite3", &bytes);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["name"], "target");
    assert!(
        body["last_used_at"].is_string(),
        "an import is a use: {body}"
    );
    assert_eq!(
        team.rows("target", "SELECT body FROM notes ORDER BY id"),
        json!([["hello"], ["world"]])
    );
    assert_eq!(
        team.sql(
            "target",
            "write",
            "INSERT INTO notes (body) VALUES ('more')"
        )
        .0,
        200
    );

    let (status, body) = put("target", "application/vnd.sqlite3", &bytes);
    assert_eq!((status, code(&body)), (409, "database_not_empty"), "{body}");
    team.create(json!({"name": "empty", "kind": "burstable"}));
    let (status, body) = put(
        "empty",
        "application/vnd.sqlite3",
        b"not a database at all, really",
    );
    assert_eq!((status, code(&body)), (422, "invalid_import"), "{body}");
    let mut corrupt = bytes.clone();
    corrupt[bytes.len() / 2..].fill(0x5a);
    let (status, body) = put("empty", "application/vnd.sqlite3", &corrupt);
    assert_eq!((status, code(&body)), (422, "invalid_import"), "{body}");
    let (status, body) = put("empty", "application/octet-stream", &bytes);
    assert_eq!(
        (status, code(&body)),
        (415, "unsupported_media_type"),
        "{body}"
    );

    // Past this gateway's 1 MiB limit.
    let big = dir.path().join("big.sqlite");
    let connection = rusqlite::Connection::open(&big).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE b (x); INSERT INTO b WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 600) SELECT randomblob(4000) FROM c;",
        )
        .unwrap();
    drop(connection);
    let (status, body) = put(
        "empty",
        "application/vnd.sqlite3",
        &std::fs::read(&big).unwrap(),
    );
    assert_eq!((status, code(&body)), (413, "database_too_large"), "{body}");
    // The refused imports left it empty and usable.
    assert_eq!(
        team.rows("empty", "SELECT count(*) FROM sqlite_schema"),
        json!([[0]])
    );
}

#[test]
fn the_reconciler_repairs_drift_at_startup() {
    let Some(db) = database() else { return };
    let dir = tempfile::tempdir().unwrap();
    let (gateway, _) = start(dir.path(), &db);
    let owner = register(&gateway, "drift@memoire.test");
    let team_id = create_team(&gateway, &owner, "drift-team");
    let team = Team {
        gateway: &gateway,
        auth: owner.bearer(),
        team: team_id.clone(),
    };
    let kept = team.create(json!({"name": "kept", "kind": "persistent"}));
    let blank = team.create(json!({"name": "blank", "kind": "burstable"}));
    team.sql("kept", "write", "CREATE TABLE t (v)");
    team.sql("kept", "write", "INSERT INTO t VALUES ('saved')");
    after_writes(&|| {
        let (_, body) = team.call("GET", "/databases/kept/snapshots", None);
        body["items"].as_array().unwrap().clone()
    });
    assert!(gateway.stop().success());

    // Drift: an old orphan, a fresh one, and two active databases without
    // their file.
    let kept_file = file(dir.path(), &team_id, kept["id"].as_str().unwrap());
    let blank_file = file(dir.path(), &team_id, blank["id"].as_str().unwrap());
    let old_orphan = file(dir.path(), &team_id, &format!("db_{:032x}", 1));
    let new_orphan = file(dir.path(), &team_id, &format!("db_{:032x}", 2));
    std::fs::copy(&kept_file, &old_orphan).unwrap();
    std::fs::copy(&kept_file, &new_orphan).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&old_orphan)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
    for gone in [&kept_file, &blank_file] {
        std::fs::remove_file(gone).unwrap();
        for side in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{side}", gone.display()));
        }
    }

    let (gateway, _) = start(dir.path(), &db);
    let team = Team {
        gateway: &gateway,
        auth: owner.bearer(),
        team: team_id.clone(),
    };
    eventually(20, "the reconciler ran", || {
        !old_orphan.exists() && kept_file.exists() && blank_file.exists()
    });
    assert!(new_orphan.exists(), "a recent file is left alone");
    assert_eq!(
        team.rows("kept", "SELECT v FROM t"),
        json!([["saved"]]),
        "restored from its snapshot"
    );
    assert_eq!(
        team.rows("blank", "SELECT count(*) FROM sqlite_schema"),
        json!([[0]]),
        "recreated empty"
    );
}
