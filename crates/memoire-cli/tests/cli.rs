//! The `memoire` binary against an in-process gateway that follows
//! `docs/api.md` (`support::fake`).
mod support;

use serde_json::json;
use support::{
    Memoire,
    fake::{EMAIL, Fake, MEMBER_KEY, PASSWORD, TEAM},
};

/// A CLI logged in to a fresh fake.
fn logged_in() -> (Fake, Memoire) {
    let fake = Fake::start();
    let memoire = Memoire::new();
    memoire
        .run(&["login", "--server", &fake.url], MEMBER_KEY)
        .ok();
    (fake, memoire)
}

#[test]
fn login_checks_the_key_and_stores_it_privately() {
    let fake = Fake::start();
    let memoire = Memoire::new();

    let run = memoire.run(&["login", "--server", &fake.url], "mzk_unknown\n");
    assert_eq!(run.code, 3, "{}", run.stderr);
    assert!(
        !memoire.credentials().exists(),
        "a refused key is not stored"
    );

    let run = memoire
        .run(
            &["login", "--server", &format!("{}/", fake.url), "--json"],
            &format!("{MEMBER_KEY}\n"),
        )
        .ok();
    assert_eq!(
        run.json(),
        json!({"server": fake.url, "subject": "key_fake_member", "kind": "api_key", "team_id": TEAM})
    );
    assert!(!run.stdout.contains(MEMBER_KEY) && !run.stderr.contains(MEMBER_KEY));
    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(memoire.credentials()).unwrap()).unwrap();
    assert_eq!(stored, json!({"server": fake.url, "api_key": MEMBER_KEY}));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&memoire.credentials()), 0o600);
        assert_eq!(mode(memoire.credentials().parent().unwrap()), 0o700);
    }
    let auth = fake.requests().last().unwrap().authorization.clone();
    assert_eq!(auth.as_deref(), Some(&*format!("Bearer {MEMBER_KEY}")));

    let run = memoire.run(&["whoami"], "").ok();
    assert!(
        run.stdout.contains("team     org_fake_team"),
        "{}",
        run.stdout
    );
}

#[test]
fn the_key_also_comes_from_a_file_or_the_environment() {
    let fake = Fake::start();
    let memoire = Memoire::new();
    let file = memoire.config.path().join("key");
    std::fs::write(&file, format!("  {MEMBER_KEY}\n")).unwrap();
    memoire
        .run(
            &[
                "login",
                "--server",
                &fake.url,
                "--key-file",
                file.to_str().unwrap(),
            ],
            "",
        )
        .ok();

    // No login at all: MEMOIRE_SERVER and MEMOIRE_API_KEY.
    let agent = Memoire::new()
        .env("MEMOIRE_SERVER", &fake.url)
        .env("MEMOIRE_API_KEY", MEMBER_KEY);
    let run = agent.run(&["whoami", "--json"], "").ok();
    assert_eq!(run.json()["team_id"], TEAM);
    assert!(!agent.credentials().exists());

    let run = Memoire::new().run(&["login", "--server", &fake.url], "not-a-key");
    assert_eq!(run.code, 3);
    let run = Memoire::new().run(&["login", "--server", "ftp://x.example"], MEMBER_KEY);
    assert_eq!(run.code, 2);
}

#[test]
fn commands_without_a_login_say_so() {
    let run = Memoire::new().run(&["db", "ls", "--json"], "");
    assert_eq!(run.code, 3);
    assert_eq!(run.error()["error"]["code"], "not_logged_in");
    assert_eq!(run.stdout, "");
}

#[test]
fn databases_are_created_listed_extended_and_removed() {
    let (fake, memoire) = logged_in();
    let run = memoire
        .run(
            &[
                "db",
                "create",
                "scratch",
                "--burstable",
                "--ttl",
                "2h",
                "--json",
            ],
            "",
        )
        .ok();
    let db = run.json();
    assert_eq!(db["kind"], "burstable");
    assert_eq!(db["idle_ttl_seconds"], 7_200);
    let body: serde_json::Value =
        serde_json::from_slice(&fake.requests().last().unwrap().body).unwrap();
    assert_eq!(
        body,
        json!({"name": "scratch", "kind": "burstable", "idle_ttl_seconds": 7200})
    );

    let run = memoire
        .run(&["db", "create", "notes", "--persistent"], "")
        .ok();
    assert!(
        run.stdout.contains("kind             persistent"),
        "{}",
        run.stdout
    );
    assert!(!run.stdout.contains("expires"), "{}", run.stdout);
    memoire.run(&["db", "create", "zeta"], "").ok();

    let run = memoire.run(&["db", "create", "scratch", "--json"], "");
    assert_eq!(run.code, 6);
    assert_eq!(run.error()["error"]["code"], "database_exists");
    let run = memoire.run(&["db", "create", "tiny", "--ttl", "10s"], "");
    assert_eq!(run.code, 7);
    assert!(
        run.stderr.contains("/idle_ttl_seconds out_of_range"),
        "{}",
        run.stderr
    );

    // The fake pages two at a time; the CLI follows the cursor.
    fake.expire("zeta");
    let run = memoire.run(&["db", "ls", "--json"], "").ok();
    let names: Vec<_> = run
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].clone())
        .collect();
    assert_eq!(names, [json!("notes"), json!("scratch")]);
    let run = memoire.run(&["db", "ls", "--all"], "").ok();
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(lines.len(), 5, "{}", run.stdout);
    assert!(lines[0].starts_with("NAME"));
    assert!(
        lines[4].starts_with("zeta") && lines[4].contains("expired"),
        "{}",
        run.stdout
    );
    assert!(fake.requests().iter().any(|r| {
        r.query
            .as_deref()
            .is_some_and(|q| q.contains("cursor=scratch"))
    }));

    let run = memoire
        .run(&["db", "extend", "scratch", "--ttl", "1d", "--json"], "")
        .ok();
    assert_eq!(run.json()["idle_ttl_seconds"], 86_400);
    let run = memoire.run(&["db", "extend", "notes"], "");
    assert_eq!(run.code, 6);
    assert!(
        run.stderr.contains("only a burstable database"),
        "{}",
        run.stderr
    );
    let run = memoire.run(&["db", "extend", "zeta"], "");
    assert_eq!(run.code, 6, "an expired database answers 410");

    let run = memoire.run(&["db", "info", "scratch"], "").ok();
    assert!(run.stdout.contains("idle ttl         1d"), "{}", run.stdout);
    memoire.run(&["db", "rm", "scratch"], "").ok();
    let run = memoire.run(&["db", "info", "scratch", "--json"], "");
    assert_eq!(run.code, 5);
    assert_eq!(run.error()["error"]["code"], "database_not_found");
}

#[test]
fn queries_bind_params_and_print_tables_or_json() {
    let (fake, memoire) = logged_in();
    memoire.run(&["db", "create", "scratch"], "").ok();
    let run = memoire
        .run(
            &[
                "query",
                "scratch",
                "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT, n, data BLOB)",
                "--write",
            ],
            "",
        )
        .ok();
    assert_eq!(run.stdout.trim(), "0 rows affected");
    let run = memoire
        .run(
            &[
                "query",
                "scratch",
                "INSERT INTO notes (body, n, data) VALUES (?, ?, ?)",
                "-p",
                "hello\nworld",
                "-p",
                "2.5",
                "-p",
                r#"{"type":"blob","value":"AAE="}"#,
                "--write",
            ],
            "",
        )
        .ok();
    assert_eq!(run.stdout.trim(), "1 row affected, last insert rowid 1");
    let body: serde_json::Value =
        serde_json::from_slice(&fake.requests().last().unwrap().body).unwrap();
    assert_eq!(
        body,
        json!({"mode": "write", "transaction": false, "statements": [{
            "sql": "INSERT INTO notes (body, n, data) VALUES (?, ?, ?)",
            "params": ["hello\nworld", 2.5, {"type": "blob", "value": "AAE="}],
        }]})
    );
    memoire
        .run(
            &[
                "query",
                "scratch",
                "INSERT INTO notes (body, n) VALUES (?, ?)",
                "-p",
                "\"7\"",
                "-p",
                "null",
                "--write",
            ],
            "",
        )
        .ok();

    let run = memoire
        .run(
            &[
                "query",
                "scratch",
                "SELECT id, body, n, data FROM notes ORDER BY id",
            ],
            "",
        )
        .ok();
    assert_eq!(
        run.stdout,
        "id  body          n     data\n\
         --  ------------  ----  -------\n\
         1   hello\\nworld  2.5   x'0001'\n\
         2   7             NULL  NULL\n\
         (2 rows)\n"
    );
    let run = memoire
        .run(
            &[
                "query",
                "scratch",
                "SELECT body, data FROM notes WHERE id = ?",
                "-p",
                "1",
                "--json",
            ],
            "",
        )
        .ok();
    assert_eq!(
        run.json(),
        json!({"columns": [{"name": "body", "decltype": null}, {"name": "data", "decltype": null}],
               "rows": [["hello\nworld", {"type": "blob", "value": "AAE="}]],
               "rows_affected": 0, "last_insert_rowid": null})
    );

    let run = memoire.run(&["query", "scratch", "DELETE FROM notes"], "");
    assert_eq!(run.code, 7);
    assert!(run.stderr.contains("--write"), "{}", run.stderr);
    let run = memoire.run(&["query", "scratch", "SELECT * FROM missing", "--json"], "");
    assert_eq!(run.code, 7);
    let error = run.error();
    assert_eq!(error["error"]["sql"]["message"], "no such table: missing");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("SQLite refused the statement: no such table")
    );
    let run = memoire.run(&["query", "scratch", "ATTACH 'x' AS y", "--write"], "");
    assert_eq!(run.code, 4);
    let run = memoire.run(&["query", "scratch", "SELECT ?", "-p", "[1]"], "");
    assert_eq!(run.code, 2);
}

#[test]
fn the_shell_runs_a_script_and_stops_at_the_first_error() {
    let (_fake, memoire) = logged_in();
    memoire.run(&["db", "create", "scratch"], "").ok();
    let script = "\
.mode write
CREATE TABLE t (
  a INTEGER
);
INSERT INTO t VALUES (1), (2);
.mode read
.tables
SELECT sum(a) AS total FROM t; SELECT 'x;y' AS s; -- trailing comment
.schema t
";
    let run = memoire.run(&["shell", "scratch"], script).ok();
    assert_eq!(
        run.stdout,
        "0 rows affected\n\
         2 rows affected, last insert rowid 2\n\
         t\n\
         total\n-----\n3\n(1 row)\n\
         s\n---\nx;y\n(1 row)\n\
         CREATE TABLE t (\n  a INTEGER\n);\n"
    );
    assert_eq!(run.stderr, "", "no prompts without a terminal");

    let run = memoire.run(
        &["shell", "scratch"],
        "INSERT INTO t VALUES (3);\nSELECT 1;\n",
    );
    assert_eq!(run.code, 7, "read mode refuses the write: {}", run.stderr);
    assert_eq!(run.stdout, "", "the shell stopped before SELECT");
    let run = memoire.run(
        &["shell", "scratch", "--write", "--json"],
        "INSERT INTO t VALUES (3);\n",
    );
    assert_eq!(run.ok().json()["rows_affected"], 1);
    let run = memoire.run(&["shell", "scratch"], "SELECT 1");
    assert_eq!(run.code, 2, "an unterminated statement is an error");
    let run = memoire.run(&["shell", "scratch"], ".bogus\n");
    assert_eq!(run.code, 2);
}

#[test]
fn export_and_import_round_trip_a_database() {
    let (fake, memoire) = logged_in();
    memoire.run(&["db", "create", "source"], "").ok();
    memoire.run(&["db", "create", "copy"], "").ok();
    memoire
        .run(
            &["query", "source", "CREATE TABLE t (a TEXT)", "--write"],
            "",
        )
        .ok();
    memoire
        .run(
            &[
                "query",
                "source",
                "INSERT INTO t VALUES ('kept')",
                "--write",
            ],
            "",
        )
        .ok();

    let file = memoire.config.path().join("source.sqlite");
    let run = memoire
        .run(
            &["export", "source", "-o", file.to_str().unwrap(), "--json"],
            "",
        )
        .ok();
    let size = std::fs::metadata(&file).unwrap().len();
    assert_eq!(run.json()["size_bytes"], size);
    assert!(
        std::fs::read(&file)
            .unwrap()
            .starts_with(b"SQLite format 3\0")
    );

    let run = memoire
        .run(&["import", "copy", file.to_str().unwrap(), "--json"], "")
        .ok();
    assert_eq!(run.json()["name"], "copy");
    let import = fake
        .requests()
        .into_iter()
        .find(|r| r.method == "PUT")
        .unwrap();
    assert_eq!(
        import.content_type.as_deref(),
        Some("application/vnd.sqlite3")
    );
    assert_eq!(import.body.len() as u64, size);
    let run = memoire
        .run(&["query", "copy", "SELECT a FROM t", "--json"], "")
        .ok();
    assert_eq!(run.json()["rows"], json!([["kept"]]));

    let run = memoire.run(&["import", "copy", file.to_str().unwrap()], "");
    assert_eq!(run.code, 6, "the target is no longer empty");
    let run = memoire.run(&["import", "source", "/nonexistent/file.sqlite"], "");
    assert_eq!(run.code, 1);
    let missing = memoire.config.path().join("missing.sqlite");
    let run = memoire.run(&["export", "nope", "-o", missing.to_str().unwrap()], "");
    assert_eq!(run.code, 5);
    assert!(!missing.exists(), "a failed export leaves no file");
    // A 200 that is not a SQLite file (a proxy's page) is refused.
    fake.fail_next(200, "not_sqlite", "permanent", None);
    let run = memoire.run(
        &["export", "source", "-o", file.to_str().unwrap(), "--json"],
        "",
    );
    assert_eq!(run.code, 1);
    assert_eq!(run.error()["error"]["code"], "invalid_response");
    assert!(
        std::fs::read(&file)
            .unwrap()
            .starts_with(b"SQLite format 3\0")
    );
}

#[test]
fn snapshots_list_and_restore_persistent_databases() {
    let (fake, memoire) = logged_in();
    memoire
        .run(&["db", "create", "notes", "--persistent"], "")
        .ok();
    memoire.run(&["db", "create", "scratch"], "").ok();
    let run = memoire.run(&["snapshots", "ls", "notes"], "").ok();
    assert_eq!(run.stdout, "");
    memoire
        .run(&["query", "notes", "CREATE TABLE t (a)", "--write"], "")
        .ok();
    let id = fake.snapshot("notes");
    memoire
        .run(&["query", "notes", "DROP TABLE t", "--write"], "")
        .ok();

    let run = memoire
        .run(&["snapshots", "ls", "notes", "--json"], "")
        .ok();
    assert_eq!(run.json()[0]["id"], id.as_str());
    assert_eq!(run.json()[0]["reason"], "scheduled");
    let run = memoire
        .run(&["snapshots", "restore", "notes", &id], "")
        .ok();
    assert!(run.stderr.contains(&format!("Restored notes from {id}")));
    let run = memoire
        .run(
            &["query", "notes", "SELECT count(*) AS n FROM t", "--json"],
            "",
        )
        .ok();
    assert_eq!(run.json()["rows"], json!([[0]]));
    let run = memoire.run(&["snapshots", "ls", "notes"], "").ok();
    assert!(
        run.stdout.lines().nth(2).unwrap().contains("pre_restore"),
        "{}",
        run.stdout
    );

    let run = memoire.run(&["snapshots", "ls", "scratch"], "");
    assert_eq!(run.code, 6);
    let run = memoire.run(&["snapshots", "restore", "notes", "snap_missing"], "");
    assert_eq!(run.code, 5);
}

#[test]
fn keys_create_mints_a_machine_key_through_a_temporary_session() {
    let (fake, memoire) = logged_in();
    let login = format!("{EMAIL}\n{PASSWORD}\n");
    let run = memoire
        .run(
            &[
                "keys",
                "create",
                "--name",
                "ci-runner",
                "--read-only",
                "--expires",
                "30d",
                "--json",
            ],
            &login,
        )
        .ok();
    let created = run.json();
    let key = created["key"].as_str().unwrap().to_owned();
    assert!(key.starts_with("mzk_"));
    assert_eq!(created["team_id"], TEAM);
    assert_eq!(
        created["api_key"]["permissions"],
        json!(["memoire.databases.read"])
    );
    assert_eq!(
        fake.open_sessions(),
        0,
        "the temporary session is signed out"
    );
    assert!(!run.stderr.contains(PASSWORD));

    let requests = fake.requests();
    let sign_in = requests
        .iter()
        .find(|r| r.path == "/v1/identity/sessions")
        .unwrap();
    assert_eq!(sign_in.origin.as_deref(), Some(fake.url.as_str()));
    assert_eq!(sign_in.authorization, None);
    let mint = requests
        .iter()
        .find(|r| r.path.ends_with("/api-keys"))
        .unwrap();
    assert_eq!(mint.organization.as_deref(), Some(TEAM));
    assert!(
        mint.authorization
            .as_deref()
            .unwrap()
            .starts_with("Bearer mzs_")
    );

    // The new key works, read-only.
    let machine = Memoire::new()
        .env("MEMOIRE_SERVER", &fake.url)
        .env("MEMOIRE_API_KEY", &key);
    machine.run(&["db", "ls"], "").ok();
    let run = machine.run(&["db", "create", "nope"], "");
    assert_eq!(run.code, 4);

    // Without --json the key alone goes to standard output.
    let run = memoire
        .run(
            &["keys", "create", "--name", "deployer", "--team", TEAM],
            &login,
        )
        .ok();
    assert!(run.stdout.trim().starts_with("mzk_") && run.stdout.lines().count() == 1);
    assert!(run.stderr.contains("read and write"), "{}", run.stderr);
    let body: serde_json::Value = serde_json::from_slice(
        &fake
            .requests()
            .iter()
            .rev()
            .find(|r| r.path.ends_with("/api-keys"))
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(body["permissions"].as_array().unwrap().len(), 5);

    let run = memoire.run(
        &["keys", "create", "--name", "x1"],
        &format!("{EMAIL}\nwrong\n"),
    );
    assert_eq!(run.code, 3);
    assert_eq!(fake.open_sessions(), 0);
    let run = Memoire::new()
        .env("MEMOIRE_SERVER", &fake.url)
        .run(&["keys", "create", "--name", "x1"], &login);
    assert_eq!(
        run.code, 2,
        "no team without --team or a login: {}",
        run.stderr
    );
}

#[test]
fn failures_map_to_stable_exit_codes() {
    let (fake, memoire) = logged_in();
    fake.fail_next(503, "sqlite_busy", "transient", Some(2));
    let run = memoire.run(&["db", "ls", "--json"], "");
    assert_eq!(run.code, 8);
    let error = run.error();
    assert_eq!(error["error"]["code"], "sqlite_busy");
    assert_eq!(error["error"]["retry_after_seconds"], 2);
    fake.fail_next(403, "unauthorized", "permanent", None);
    assert_eq!(memoire.run(&["db", "ls"], "").code, 4);

    fake.add_key("mzk_application_key", None, &[]);
    let app = Memoire::new()
        .env("MEMOIRE_SERVER", &fake.url)
        .env("MEMOIRE_API_KEY", "mzk_application_key");
    let run = app.run(&["db", "ls"], "");
    assert_eq!(run.code, 4);
    assert!(run.stderr.contains("no team"), "{}", run.stderr);

    // Nothing listens on port 9 of loopback.
    let down = Memoire::new()
        .env("MEMOIRE_SERVER", "http://127.0.0.1:9")
        .env("MEMOIRE_API_KEY", MEMBER_KEY);
    let run = down.run(&["whoami", "--json"], "");
    assert_eq!(run.code, 8);
    assert_eq!(run.error()["error"]["code"], "connection_failed");
    assert!(!run.stderr.contains(MEMBER_KEY));

    let run = memoire.run(&["db", "create", "Bad_Name"], "");
    assert_eq!(run.code, 2);
    let run = memoire.run(&["db", "create", "Bad_Name", "--json"], "");
    assert_eq!(run.code, 2);
    assert_eq!(run.error()["error"]["code"], "invalid_usage");
}
