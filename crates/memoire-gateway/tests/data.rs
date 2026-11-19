//! The data plane through the gateway: files, `query`, `schema`, `rows`,
//! the limits, last use and the idle close, on a real host and PostgreSQL.
mod common;

use common::*;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const SETTINGS: &str = r#"data_dir = "data"
max_database_bytes = 1048576
idle_close_seconds = 1
last_used_flush_seconds = 1"#;

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
            &[
                ("authorization", self.auth.as_str()),
                ("merezha-organization", self.team.as_str()),
            ],
            body.as_ref(),
        )
    }

    fn query(&self, name: &str, body: Value) -> (u16, Value) {
        self.call("POST", &format!("/databases/{name}/query"), Some(body))
    }

    fn write(&self, name: &str, sql: &str) -> (u16, Value) {
        self.query(name, json!({"mode": "write", "statements": [{"sql": sql}]}))
    }
}

fn code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("")
}

fn file(dir: &Path, team: &str, id: &Value) -> PathBuf {
    dir.join("data/teams")
        .join(team)
        .join(format!("{}.sqlite", id.as_str().unwrap()))
}

/// Whether the process holds `path` open.
fn holds(pid: u32, path: &Path) -> bool {
    std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .filter_map(|fd| std::fs::read_link(fd.ok()?.path()).ok())
        .any(|target| target == path)
}

#[test]
fn queries_run_on_the_database_file() {
    let Some(db) = database() else { return };
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), SETTINGS);
    let gateway = Gateway::start(&config, &db);
    let owner = register(&gateway, "data@memoire.test");
    let team_id = create_team(&gateway, &owner, "data-team");
    let team = Team {
        gateway: &gateway,
        auth: owner.bearer(),
        team: team_id.clone(),
    };

    // Create makes the empty file, in WAL mode.
    let (status, notes) = team.call(
        "POST",
        "/databases",
        Some(json!({"name": "notes", "kind": "persistent"})),
    );
    assert_eq!(status, 201, "{notes}");
    let path = file(dir.path(), &team_id, &notes["id"]);
    assert!(path.is_file(), "{path:?}");
    assert!(notes["size_bytes"].as_u64().unwrap() > 0, "{notes}");
    let header = std::fs::read(&path).unwrap();
    assert_eq!(&header[..16], b"SQLite format 3\0");
    assert_eq!((header[18], header[19]), (2, 2), "WAL mode");
    assert_eq!(notes["last_used_at"], Value::Null);

    // The contract's example batch.
    let (status, body) = team.query(
        "notes",
        json!({
            "mode": "write", "transaction": true, "timeout_ms": 5000,
            "statements": [
                {"sql": "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT, data BLOB)"},
                {"sql": "INSERT INTO notes (body, data) VALUES (?, ?)", "params": ["hello", {"type": "blob", "value": "AAE="}]},
                {"sql": "SELECT id, body FROM notes WHERE body = :body", "params": {"body": "hello"}},
            ],
        }),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        json!({"results": [
            {"columns": [], "rows": [], "rows_affected": 0, "last_insert_rowid": null},
            {"columns": [], "rows": [], "rows_affected": 1, "last_insert_rowid": 1},
            {"columns": [{"name": "id", "decltype": "INTEGER"}, {"name": "body", "decltype": "TEXT"}],
             "rows": [[1, "hello"]], "rows_affected": 0, "last_insert_rowid": null},
        ]})
    );

    // Values travel in the contract's encoding, both ways.
    let (status, body) = team.query(
        "notes",
        json!({"statements": [{"sql": "SELECT ?, ?, ?, ?, ?, 9007199254740993, 2.0 * 1, x'0001', NULL",
               "params": [{"type": "integer", "value": "9223372036854775807"}, 1.5, {"type": "real", "value": "Infinity"}, true, "12"]}]}),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["results"][0]["rows"][0],
        json!([{"type": "integer", "value": "9223372036854775807"}, 1.5,
               {"type": "real", "value": "Infinity"}, 1, "12",
               {"type": "integer", "value": "9007199254740993"}, 2.0,
               {"type": "blob", "value": "AAE="}, null])
    );
    // REAL keeps its fraction on the wire.
    let (_, head, raw) = gateway.raw(
        "POST",
        "/v1/memoire/databases/notes/query",
        &[
            ("authorization", team.auth.as_str()),
            ("merezha-organization", team_id.as_str()),
        ],
        Some((
            "application/json",
            br#"{"statements": [{"sql": "SELECT 1.0, 1"}]}"#,
        )),
    );
    assert!(
        String::from_utf8_lossy(&raw).contains("[[1.0,1]]"),
        "{head} {}",
        String::from_utf8_lossy(&raw)
    );

    // Refusals. SQLite's own refusal names the statement in `details` and
    // carries SQLite's message in a header.
    let (status, head, raw) = gateway.raw(
        "POST",
        "/v1/memoire/databases/notes/query",
        &[
            ("authorization", team.auth.as_str()),
            ("merezha-organization", team_id.as_str()),
        ],
        Some((
            "application/json",
            br#"{"mode": "write", "statements": [{"sql": "SELECT 1"}, {"sql": "INSERT INTO notes (id) VALUES (1)"}]}"#,
        )),
    );
    let body: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(status, 422, "{body}");
    assert_eq!(
        body,
        json!({"error": {"code": "sql_error", "classification": "permanent",
               "details": [{"path": "/statements/1/sql", "code": "sqlite_constraint"}]}})
    );
    let message = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("memoire-sql-message")
                .then(|| value.trim().to_owned())
        })
        .unwrap_or_else(|| panic!("no message header: {head}"));
    let detail = memoire_api::SqlErrorDetail::from_wire(
        &serde_json::from_value::<memoire_api::ErrorBody>(body)
            .unwrap()
            .error,
        Some(&message),
    )
    .unwrap();
    assert_eq!(
        (
            detail.statement,
            detail.sqlite_code.as_str(),
            detail.message.as_str()
        ),
        (1, "SQLITE_CONSTRAINT", "UNIQUE constraint failed: notes.id")
    );
    let (status, body) = team.query(
        "notes",
        json!({"statements": [{"sql": "DELETE FROM notes"}]}),
    );
    assert_eq!((status, code(&body)), (422, "read_only"), "{body}");
    let (status, body) = team.write("notes", "ATTACH DATABASE 'x.db' AS x");
    assert_eq!((status, code(&body)), (403, "statement_denied"), "{body}");
    let (status, body) = team.write("notes", "BEGIN");
    assert_eq!((status, code(&body)), (403, "statement_denied"), "{body}");
    let (status, body) = team.query(
        "notes",
        json!({"timeout_ms": 100, "statements": [{"sql":
            "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c) SELECT count(*) FROM c"}]}),
    );
    assert_eq!((status, code(&body)), (422, "statement_timeout"), "{body}");
    let (status, body) = team.query(
        "notes",
        json!({"statements": [{"sql": "SELECT ?", "params": [1, 2]}, {"sql": 7}]}),
    );
    assert_eq!(status, 422, "{body}");
    assert_eq!(
        body["error"]["details"],
        json!([{"path": "/statements/1/sql", "code": "invalid"}])
    );
    let (status, body) = team.query(
        "notes",
        json!({"statements": [{"sql": "SELECT ?", "params": [1, 2]}]}),
    );
    assert_eq!(
        (status, &body["error"]["details"][0]["path"]),
        (422, &json!("/statements/0/params")),
        "{body}"
    );
    let (status, body) = team.query("missing", json!({"statements": [{"sql": "SELECT 1"}]}));
    assert_eq!((status, code(&body)), (404, "database_not_found"));
    let huge = "x".repeat(1 << 20);
    let (status, body) = team.query("notes", json!({"statements": [{"sql": huge}]}));
    assert_eq!((status, code(&body)), (413, "body_limit"), "{body}");

    // The 1 MiB file limit of this gateway: a write past it is refused and
    // leaves nothing.
    team.write("notes", "CREATE TABLE big (b)");
    let (status, body) = team.write(
        "notes",
        "INSERT INTO big WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 300) SELECT randomblob(4096) FROM c",
    );
    assert_eq!((status, code(&body)), (413, "database_too_large"), "{body}");
    let (_, body) = team.query(
        "notes",
        json!({"statements": [{"sql": "SELECT count(*) FROM big"}]}),
    );
    assert_eq!(body["results"][0]["rows"][0][0], 0);

    // Schema and rows.
    team.write(
        "notes",
        "CREATE TABLE \"odd name/x\" (k TEXT PRIMARY KEY, v) WITHOUT ROWID",
    );
    team.write(
        "notes",
        "INSERT INTO \"odd name/x\" VALUES ('b', 2), ('a', 1), ('c', 3)",
    );
    team.write("notes", "CREATE VIEW recent AS SELECT body FROM notes");
    team.write("notes", "CREATE INDEX notes_body ON notes (body)");
    let (status, schema) = team.call("GET", "/databases/notes/schema", None);
    assert_eq!(status, 200, "{schema}");
    let tables: Vec<&str> = schema["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(tables, ["big", "notes", "odd name/x"]);
    assert_eq!(schema["tables"][2]["without_rowid"], true);
    assert_eq!(
        schema["tables"][1]["indexes"][0],
        json!({"name": "notes_body", "unique": false, "columns": ["body"],
               "sql": "CREATE INDEX notes_body ON notes (body)"})
    );
    assert_eq!(schema["views"][0]["name"], "recent");

    let (status, page) = team.call(
        "GET",
        "/databases/notes/tables/odd%20name%2Fx/rows?limit=2",
        None,
    );
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["rows"], json!([["a", 1], ["b", 2]]));
    let after = page["next_after"].as_str().unwrap().to_owned();
    let (_, page) = team.call(
        "GET",
        &format!("/databases/notes/tables/odd%20name%2Fx/rows?limit=2&after={after}"),
        None,
    );
    assert_eq!(page["rows"], json!([["c", 3]]));
    assert_eq!(page["next_after"], Value::Null);
    let (status, body) = team.call(
        "GET",
        &format!("/databases/notes/tables/notes/rows?after={after}"),
        None,
    );
    assert_eq!((status, code(&body)), (422, "invalid_page"));
    let (status, body) = team.call("GET", "/databases/notes/tables/nothing/rows", None);
    assert_eq!((status, code(&body)), (404, "table_not_found"));
    let (status, body) = team.call("GET", "/databases/notes/tables/notes/rows?limit=1001", None);
    assert_eq!((status, code(&body)), (422, "validation_failed"));
    let (status, page) = team.call("GET", "/databases/notes/tables/recent/rows", None);
    assert_eq!((status, &page["rows"]), (200, &json!([["hello"]])));

    // A read-only key reads and cannot write.
    let (_, key) = api_key(
        &gateway,
        &owner,
        &team_id,
        &["memoire.databases.read"],
        None,
    );
    let reader = Team {
        gateway: &gateway,
        auth: format!("Bearer {}", key["key"].as_str().unwrap()),
        team: team_id.clone(),
    };
    assert_eq!(
        reader
            .query("notes", json!({"statements": [{"sql": "SELECT 1"}]}))
            .0,
        200
    );
    let (status, body) = reader.write("notes", "DELETE FROM notes");
    assert_eq!((status, code(&body)), (403, "unauthorized"), "{body}");

    // Queries are uses; last use survives a restart once flushed.
    let (_, detail) = team.call("GET", "/databases/notes", None);
    let used = detail["last_used_at"].clone();
    assert!(used.is_string(), "{detail}");
    std::thread::sleep(Duration::from_millis(2500));
    assert!(gateway.stop().success());
    let gateway = Gateway::start(&config, &db);
    let team = Team {
        gateway: &gateway,
        auth: owner.bearer(),
        team: team_id.clone(),
    };
    let (_, detail) = team.call("GET", "/databases/notes", None);
    assert_eq!(detail["last_used_at"], used, "{detail}");
    let (_, list) = team.call("GET", "/databases", None);
    assert_eq!(list["items"][0]["last_used_at"], used);
    assert!(list["items"][0]["size_bytes"].as_u64().unwrap() > 4096);

    // Delete removes the file.
    assert_eq!(team.call("DELETE", "/databases/notes", None).0, 204);
    assert!(!path.exists());
    for side in ["-wal", "-shm"] {
        assert!(!PathBuf::from(format!("{}{side}", path.display())).exists());
    }
}

/// A burstable database's connections close after `idle_close_seconds`
/// without a request and reopen on the next one.
#[test]
fn burstable_databases_close_when_idle() {
    let Some(db) = database() else { return };
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::start(&config(dir.path(), SETTINGS), &db);
    let owner = register(&gateway, "idle@memoire.test");
    let team_id = create_team(&gateway, &owner, "idle-team");
    let team = Team {
        gateway: &gateway,
        auth: owner.bearer(),
        team: team_id.clone(),
    };
    let (_, scratch) = team.call(
        "POST",
        "/databases",
        Some(json!({"name": "scratch", "kind": "burstable"})),
    );
    let (_, notes) = team.call(
        "POST",
        "/databases",
        Some(json!({"name": "notes", "kind": "persistent"})),
    );
    let scratch_file = file(dir.path(), &team_id, &scratch["id"]);
    let notes_file = file(dir.path(), &team_id, &notes["id"]);
    for name in ["scratch", "notes"] {
        assert_eq!(team.write(name, "CREATE TABLE t (x)").0, 200);
    }
    assert!(holds(gateway.pid(), &scratch_file));
    let started = Instant::now();
    while holds(gateway.pid(), &scratch_file) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the burstable database stayed open"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(holds(gateway.pid(), &notes_file), "persistent stays open");
    let (status, body) = team.query(
        "scratch",
        json!({"statements": [{"sql": "SELECT count(*) FROM t"}]}),
    );
    assert_eq!(status, 200, "{body}");
    assert!(holds(gateway.pid(), &scratch_file), "reopened");
}
