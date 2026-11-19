//! The `memoire` binary against a real `memoire-gateway` on a throwaway
//! PostgreSQL database (`MEREZHA_TEST_DATABASE_URL`), through every route
//! the CLI uses.
//!
//! The gateway runs the repository's development configuration
//! (`config/dev/`), copied into a temporary directory with synthetic
//! secrets, one gateway per test on its own loopback port. Its binary is the workspace build's
//! (`cargo test --workspace` builds it; `cargo build -p memoire-gateway`
//! otherwise), or `MEMOIRE_GATEWAY_BIN`.
#![cfg(unix)]
mod support;

use merezha_postgres::TestDatabase;
use serde_json::{Value, json};
use std::{
    io::{BufRead as _, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use support::Memoire;

/// Each test runs its own gateway on its own port of the lane's band.
const CATALOG_AND_DATA_PORT: u16 = 28971;
const FILES_PORT: u16 = 28973;
const MEMORY_PORT: u16 = 28975;
const EMAIL: &str = "member@memoire.test";
const PASSWORD: &str = "correct horse battery staple 42";

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn gateway_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("MEMOIRE_GATEWAY_BIN") {
        return path.into();
    }
    // target/debug/deps/gateway-… → target/debug/memoire-gateway
    let exe = std::env::current_exe().unwrap();
    let path = exe
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("memoire-gateway");
    assert!(
        path.is_file(),
        "no gateway at {}: run `cargo build -p memoire-gateway` or set MEMOIRE_GATEWAY_BIN",
        path.display()
    );
    path
}

/// The development configuration in `dir`, laid out as in the repository
/// so its relative paths hold: `config/dev/*`, `.memoire-dev/secrets/`, and
/// `guests/` linked to the repository's built components.
fn layout(dir: &Path) -> PathBuf {
    let config = dir.join("config/dev");
    std::fs::create_dir_all(&config).unwrap();
    for entry in std::fs::read_dir(repository().join("config/dev")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), config.join(entry.file_name())).unwrap();
    }
    let secrets = dir.join(".memoire-dev/secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    std::fs::write(
        secrets.join("identity.key"),
        "a-memoire-cli-gateway-test-identity-secret-key-of-48-bytes",
    )
    .unwrap();
    std::fs::write(
        secrets.join("notifications.key"),
        "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=",
    )
    .unwrap();
    std::os::unix::fs::symlink(repository().join("guests"), dir.join("guests")).unwrap();
    config.join("memoire.toml")
}

struct Gateway {
    /// `http://localhost:PORT`, the gateway's application origin.
    server: String,
    child: Child,
    logs: Arc<Mutex<Vec<String>>>,
    http: reqwest::blocking::Client,
}

impl Gateway {
    fn start(dir: &Path, db: &TestDatabase, port: u16) -> Self {
        let server = format!("http://localhost:{port}");
        let config = layout(dir);
        let data = dir.join("data");
        let mut child = Command::new(gateway_binary())
            .args(["run", "--config"])
            .arg(&config)
            .args(["--set", &format!("http.listen=127.0.0.1:{port}")])
            .args(["--set", &format!("modules.identity.app_origin={server}")])
            .args([
                "--set",
                &format!("modules.memoire.data_dir={}", data.display()),
            ])
            .env("MEMOIRE_DATABASE_URL", db.url())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let logs = Arc::new(Mutex::new(Vec::new()));
        let sink = logs.clone();
        let stderr = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                sink.lock().unwrap().push(line);
            }
        });
        let mut gateway = Self {
            server,
            child,
            logs,
            http: reqwest::blocking::Client::new(),
        };
        let started = Instant::now();
        loop {
            let ready = gateway
                .http
                .get(format!("{}/health/ready", gateway.server))
                .send()
                .is_ok_and(|r| r.status() == 200);
            if let Ok(Some(status)) = gateway.child.try_wait() {
                panic!(
                    "the gateway exited {status}: {:?}",
                    gateway.logs.lock().unwrap()
                );
            }
            if ready {
                return gateway;
            }
            assert!(
                started.elapsed() < Duration::from_secs(90),
                "the gateway never became ready: {:?}",
                gateway.logs.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// A JSON request; the status and body.
    fn call(
        &self,
        method: &str,
        path: &str,
        bearer: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let (status, body, _) = self.call_with_cookies(method, path, bearer, body, "");
        (status, body)
    }

    /// A JSON request carrying `cookies`; the status, the body and the
    /// cookies it set, as a `Cookie` header value.
    fn call_with_cookies(
        &self,
        method: &str,
        path: &str,
        bearer: Option<&str>,
        body: Option<Value>,
        cookies: &str,
    ) -> (u16, Value, String) {
        let mut request = self
            .http
            .request(method.parse().unwrap(), format!("{}{path}", self.server))
            .header("origin", &self.server);
        if let Some(token) = bearer {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        if !cookies.is_empty() {
            request = request.header("cookie", cookies);
        }
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(body.to_string());
        }
        let response = request.send().unwrap();
        let status = response.status().as_u16();
        let set: Vec<String> = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok()?.split(';').next().map(str::to_owned))
            .collect();
        let text = response.text().unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::Null),
            set.join("; "),
        )
    }

    /// A verified member with a team: the team id.
    fn member_with_team(&self) -> String {
        // Confirming in the registering browser (its binding cookie) keeps
        // the password; identity treats any other confirm as a claim.
        let (status, body, binding) = self.call_with_cookies(
            "POST",
            "/v1/identity/registrations",
            None,
            Some(json!({"email": EMAIL, "password": PASSWORD})),
            "",
        );
        assert!(status < 300, "{status} {body}");
        let token = self.verification_token();
        let (status, body, _) = self.call_with_cookies(
            "POST",
            "/v1/identity/email-verifications/confirm",
            None,
            Some(json!({"token": token, "transport": "bearer"})),
            &binding,
        );
        assert_eq!(status, 201, "{body}");
        let session = body["session"]["token"].as_str().unwrap().to_owned();
        let (status, body) = self.call(
            "POST",
            "/v1/identity/organizations",
            Some(&session),
            Some(json!({"name": "CLI team", "slug": "cli-team"})),
        );
        assert_eq!(status, 201, "{body}");
        let team = body["organization"]["id"].as_str().unwrap().to_owned();
        self.call("DELETE", "/v1/identity/session", Some(&session), None);
        team
    }

    fn verification_token(&self) -> String {
        let started = Instant::now();
        loop {
            let (_, captures) = self.call("GET", "/v1/notifications/dev/captures", None, None);
            let token = captures
                .as_array()
                .and_then(|all| all.last())
                .and_then(|mail| mail["rendered"]["text"].as_str())
                .and_then(|text| text.split_once("#token="))
                .map(|(_, rest)| rest.split_whitespace().next().unwrap_or("").to_owned());
            if let Some(token) = token {
                return token;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "no verification mail: {captures}"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if std::thread::panicking() {
            eprintln!("gateway logs:\n{}", self.logs.lock().unwrap().join("\n"));
        }
    }
}

/// A member with a team, a machine key minted by `keys create` before any
/// login, and a CLI logged in with it: the team, the key and the CLI.
fn signed_in(gateway: &Gateway) -> (String, String, Memoire) {
    let team = gateway.member_with_team();
    let sign_in = format!("{EMAIL}\n{PASSWORD}\n");
    let server = gateway.server.as_str();

    // A machine key through identity's routes, before any login.
    let bootstrap = Memoire::new().env("MEMOIRE_SERVER", server);
    let run = bootstrap
        .run(
            &[
                "keys",
                "create",
                "--name",
                "ci-runner",
                "--team",
                &team,
                "--json",
            ],
            &sign_in,
        )
        .ok();
    let created = run.json();
    let key = created["key"].as_str().unwrap().to_owned();
    assert!(key.starts_with("mzk_"), "{created}");
    assert_eq!(
        created["api_key"]["permissions"].as_array().unwrap().len(),
        5
    );

    let memoire = Memoire::new();
    let run = memoire
        .run(&["login", "--server", server, "--json"], &key)
        .ok();
    assert_eq!(run.json()["team_id"], team.as_str());
    assert_eq!(run.json()["kind"], "api_key");
    let run = memoire.run(&["whoami"], "").ok();
    assert!(run.stdout.contains(&team), "{}", run.stdout);

    (team, key, memoire)
}

#[test]
fn catalog_and_data_commands_work_against_a_real_gateway() {
    let Some(db) = TestDatabase::create() else {
        eprintln!("skipped: MEREZHA_TEST_DATABASE_URL is unset");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::start(dir.path(), &db, CATALOG_AND_DATA_PORT);
    let (_team, key, memoire) = signed_in(&gateway);
    let sign_in = format!("{EMAIL}\n{PASSWORD}\n");

    // keys create again, the team from the stored login.
    let run = memoire
        .run(
            &["keys", "create", "--name", "reader", "--read-only"],
            &sign_in,
        )
        .ok();
    let reader_key = run.stdout.trim().to_owned();
    assert!(reader_key.starts_with("mzk_"), "{}", run.stdout);

    // Catalog.
    let run = memoire
        .run(
            &[
                "db",
                "create",
                "scratch",
                "--burstable",
                "--ttl",
                "1h",
                "--json",
            ],
            "",
        )
        .ok();
    let scratch = run.json();
    assert_eq!(scratch["kind"], "burstable");
    assert_eq!(scratch["state"], "active");
    assert_eq!(scratch["idle_ttl_seconds"], 3_600);
    assert!(scratch["id"].as_str().unwrap().starts_with("db_"));
    memoire
        .run(&["db", "create", "notes", "--persistent"], "")
        .ok();
    memoire.run(&["db", "create", "copy"], "").ok();
    let run = memoire.run(&["db", "create", "scratch", "--json"], "");
    assert_eq!(run.code, 6, "{}", run.stderr);
    assert_eq!(run.error()["error"]["code"], "database_exists");
    let run = memoire.run(&["db", "create", "tiny", "--ttl", "10s"], "");
    assert_eq!(run.code, 7, "{}", run.stderr);

    let run = memoire.run(&["db", "ls", "--json"], "").ok();
    let names: Vec<String> = run
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["copy", "notes", "scratch"]);
    let run = memoire.run(&["db", "ls", "--all"], "").ok();
    assert_eq!(run.stdout.lines().count(), 5, "{}", run.stdout);
    let run = memoire.run(&["db", "info", "notes", "--json"], "").ok();
    assert_eq!(run.json()["kind"], "persistent");
    assert_eq!(run.json()["expires_at"], Value::Null);
    let run = memoire
        .run(&["db", "extend", "scratch", "--ttl", "2h", "--json"], "")
        .ok();
    assert_eq!(run.json()["idle_ttl_seconds"], 7_200);
    let run = memoire.run(&["db", "extend", "notes"], "");
    assert_eq!(run.code, 6, "{}", run.stderr);
    let run = memoire.run(&["db", "info", "missing", "--json"], "");
    assert_eq!(run.code, 5, "{}", run.stderr);
    assert_eq!(run.error()["error"]["code"], "database_not_found");

    // Data plane.
    memoire
        .run(
            &[
                "query",
                "scratch",
                "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT, data BLOB)",
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
                "INSERT INTO notes (body, data) VALUES (?, ?)",
                "-p",
                "hello",
                "-p",
                r#"{"type":"blob","value":"AAE="}"#,
                "--write",
                "--json",
            ],
            "",
        )
        .ok();
    assert_eq!(run.json()["rows_affected"], 1);
    assert_eq!(run.json()["last_insert_rowid"], 1);
    let run = memoire
        .run(
            &[
                "query",
                "scratch",
                "SELECT id, body, data FROM notes",
                "--json",
            ],
            "",
        )
        .ok();
    assert_eq!(
        run.json(),
        json!({"columns": [{"name": "id", "decltype": "INTEGER"}, {"name": "body", "decltype": "TEXT"},
                           {"name": "data", "decltype": "BLOB"}],
               "rows": [[1, "hello", {"type": "blob", "value": "AAE="}]],
               "rows_affected": 0, "last_insert_rowid": null})
    );
    let run = memoire
        .run(&["query", "scratch", "SELECT id, body FROM notes"], "")
        .ok();
    assert_eq!(run.stdout, "id  body\n--  -----\n1   hello\n(1 row)\n");
    let run = memoire.run(&["query", "scratch", "DELETE FROM notes", "--json"], "");
    assert_eq!(run.code, 7, "{}", run.stderr);
    assert_eq!(run.error()["error"]["code"], "read_only");
    let run = memoire.run(&["query", "scratch", "SELECT * FROM missing", "--json"], "");
    assert_eq!(run.code, 7, "{}", run.stderr);
    assert_eq!(run.error()["error"]["code"], "sql_error");
    let run = memoire.run(
        &["query", "scratch", "ATTACH 'x' AS y", "--write", "--json"],
        "",
    );
    assert_eq!(run.code, 4, "{}", run.stderr);
    assert_eq!(run.error()["error"]["code"], "statement_denied");

    let script = ".mode write\nINSERT INTO notes (body)\n  VALUES ('two');\n.mode read\n.tables\nSELECT count(*) AS n FROM notes;\n";
    let run = memoire.run(&["shell", "scratch"], script).ok();
    assert_eq!(
        run.stdout,
        "1 row affected, last insert rowid 2\nnotes\nn\n-\n2\n(1 row)\n"
    );

    // The read-only machine reads and cannot write.
    let reader = Memoire::new()
        .env("MEMOIRE_SERVER", &gateway.server)
        .env("MEMOIRE_API_KEY", &reader_key);
    reader.run(&["query", "scratch", "SELECT 1"], "").ok();
    let run = reader.run(&["query", "scratch", "DELETE FROM notes", "--write"], "");
    assert_eq!(run.code, 4, "{}", run.stderr);
    let run = reader.run(&["db", "create", "nope"], "");
    assert_eq!(run.code, 4, "{}", run.stderr);

    // Delete.
    memoire.run(&["db", "rm", "scratch"], "").ok();
    let run = memoire.run(&["db", "info", "scratch"], "");
    assert_eq!(run.code, 5, "{}", run.stderr);

    // The stored key never reached the gateway's logs.
    let logs = gateway.logs.lock().unwrap().join("\n");
    assert!(!logs.contains(&key) && !logs.contains(PASSWORD));
}

/// Export, import and snapshots, the gateway's last routes.
#[test]
fn files_and_snapshots_work_against_a_real_gateway() {
    let Some(db) = TestDatabase::create() else {
        eprintln!("skipped: MEREZHA_TEST_DATABASE_URL is unset");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::start(dir.path(), &db, FILES_PORT);
    let (_team, _key, memoire) = signed_in(&gateway);
    memoire.run(&["db", "create", "scratch"], "").ok();
    memoire
        .run(&["db", "create", "notes", "--persistent"], "")
        .ok();
    memoire.run(&["db", "create", "copy"], "").ok();
    memoire
        .run(&["shell", "scratch", "--write"], "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);\nINSERT INTO notes (body) VALUES ('hello'), ('two');\n")
        .ok();

    // Export and import.
    let file = dir.path().join("scratch.sqlite");
    let run = memoire
        .run(
            &["export", "scratch", "-o", file.to_str().unwrap(), "--json"],
            "",
        )
        .ok();
    assert_eq!(
        run.json()["size_bytes"],
        std::fs::metadata(&file).unwrap().len()
    );
    memoire
        .run(&["import", "copy", file.to_str().unwrap()], "")
        .ok();
    let run = memoire
        .run(
            &[
                "query",
                "copy",
                "SELECT body FROM notes ORDER BY id",
                "--json",
            ],
            "",
        )
        .ok();
    assert_eq!(run.json()["rows"], json!([["hello"], ["two"]]));
    let run = memoire.run(&["import", "copy", file.to_str().unwrap(), "--json"], "");
    assert_eq!(run.code, 6, "{}", run.stderr);
    assert_eq!(run.error()["error"]["code"], "database_not_empty");

    // Snapshots: none yet on a new persistent database; burstable has none.
    let run = memoire
        .run(&["snapshots", "ls", "notes", "--json"], "")
        .ok();
    assert_eq!(run.json(), json!([]));
    let run = memoire.run(
        &["snapshots", "restore", "notes", "snap_missing", "--json"],
        "",
    );
    assert_eq!(run.code, 5, "{}", run.stderr);
    let run = memoire.run(&["snapshots", "ls", "scratch", "--json"], "");
    assert_eq!(run.code, 6, "{}", run.stderr);
    assert_eq!(run.error()["error"]["code"], "not_persistent");
}

/// A CLI for one machine of the team: its key and its agent and session
/// in the environment, run in `dir`, outside any git checkout.
fn machine(gateway: &Gateway, dir: &Path, key: &str, agent: &str, session: &str) -> Memoire {
    Memoire::new()
        .dir(dir)
        .env("MEMOIRE_SERVER", &gateway.server)
        .env("MEMOIRE_API_KEY", key)
        .env("MEMOIRE_AGENT", agent)
        .env("MEMOIRE_SESSION", session)
}

/// `memoire ARGS --json`, which must succeed; its JSON.
fn json_of(memoire: &Memoire, args: &[&str]) -> Value {
    let mut args = args.to_vec();
    args.push("--json");
    memoire.run(&args, "").ok().json()
}

/// `memoire ARGS --json`, which must exit `code`; its error object.
fn failing(memoire: &Memoire, args: &[&str], code: i32) -> Value {
    let mut args = args.to_vec();
    args.push("--json");
    let run = memoire.run(&args, "");
    assert_eq!(
        run.code, code,
        "{args:?}\nstdout: {}\nstderr: {}",
        run.stdout, run.stderr
    );
    run.error()["error"].clone()
}

fn ids(value: &Value) -> Vec<String> {
    value["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_owned())
        .collect()
}

/// The memory store on the real data plane: two machines of one team share
/// it, a read-only machine reads it, and every verb's SQL runs as the
/// gateway runs it (defensive connections, the authorizer, transactions).
#[test]
fn memory_is_shared_between_machines_on_a_real_gateway() {
    let Some(db) = TestDatabase::create() else {
        eprintln!("skipped: MEREZHA_TEST_DATABASE_URL is unset");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::start(dir.path(), &db, MEMORY_PORT);
    let (_team, key_a, member) = signed_in(&gateway);
    let sign_in = format!("{EMAIL}\n{PASSWORD}\n");
    let key_b = member
        .run(&["keys", "create", "--name", "machine-b"], &sign_in)
        .ok()
        .stdout
        .trim()
        .to_owned();
    let key_reader = member
        .run(
            &["keys", "create", "--name", "reader", "--read-only"],
            &sign_in,
        )
        .ok()
        .stdout
        .trim()
        .to_owned();
    assert!(key_b.starts_with("mzk_") && key_reader.starts_with("mzk_"));
    let work = tempfile::tempdir().unwrap();
    let a = machine(&gateway, work.path(), &key_a, "agent-a", "run-a");
    let b = machine(&gateway, work.path(), &key_b, "agent-b", "run-b");
    let reader = machine(&gateway, work.path(), &key_reader, "reader", "run-r");

    // A sets the store up and records a pitfall.
    let init = json_of(&a, &["memory", "init"]);
    assert_eq!(init["created"], true);
    assert_eq!(init["database"]["kind"], "persistent");
    assert_eq!(
        (
            init["schema_version"].clone(),
            init["migrated_from"].clone()
        ),
        (json!(1), json!(0))
    );
    let again = json_of(&a, &["memory", "init"]);
    assert_eq!(
        (again["created"].clone(), again["store_id"].clone()),
        (json!(false), init["store_id"].clone())
    );
    let pitfall = json_of(
        &a,
        &[
            "memory",
            "remember",
            "--title",
            "Migrations lock the orders table",
            "--kind",
            "pitfall",
            "--body",
            "Run them outside business hours.",
            "--tag",
            "migrations",
            "--evidence",
            "incident 2026-11-03",
        ],
    );
    let id = pitfall["id"].as_str().unwrap().to_owned();
    assert_eq!(
        (
            pitfall["rev"].clone(),
            pitfall["status"].clone(),
            pitfall["scope"].clone()
        ),
        (json!(1), json!("inferred"), json!("global"))
    );
    let principal_a = pitfall["author"]["principal"].as_str().unwrap().to_owned();
    assert!(principal_a.starts_with("key_"), "{pitfall}");

    // B finds it, checks it and confirms the revision it read.
    let found = json_of(&b, &["memory", "recall", "orders table migrations"]);
    assert_eq!(found["scopes"], json!(["global"]));
    assert_eq!(found["memories"][0]["id"], id);
    assert_eq!(found["memories"][0]["status"], "inferred");
    assert!(found["memories"][0]["score"].as_f64().unwrap() > 0.0);
    let confirmed = json_of(
        &b,
        &[
            "memory",
            "confirm",
            &id,
            "--evidence",
            "reproduced on staging",
            "--rev",
            "1",
        ],
    );
    assert_eq!(
        (confirmed["status"].clone(), confirmed["confirmers"].clone()),
        (json!("confirmed"), json!(1))
    );
    let shown = json_of(&b, &["memory", "show", &id]);
    let confirmation = &shown["confirmations"][0];
    assert_ne!(confirmation["principal"], principal_a.as_str());
    assert!(
        confirmation["principal"]
            .as_str()
            .unwrap()
            .starts_with("key_")
    );
    assert_eq!(
        (
            confirmation["counts"].clone(),
            confirmation["agent"].clone()
        ),
        (json!(true), json!("agent-b"))
    );

    // A hears of it once.
    let news = json_of(&a, &["memory", "log", "--unseen"]);
    let verbs: Vec<&str> = news["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["verb"].as_str().unwrap())
        .collect();
    assert_eq!(verbs, ["confirm", "remember"]);
    assert_eq!(news["events"][0]["agent"], "agent-b");
    assert_eq!(
        json_of(&a, &["memory", "log", "--unseen"])["events"],
        json!([])
    );

    // The read-only machine reads, and only reads.
    assert_eq!(
        ids(&json_of(&reader, &["memory", "recall", "migrations"])),
        [id.as_str()]
    );
    assert_eq!(
        json_of(&reader, &["memory", "show", &id, "--history"])["revisions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        json_of(&reader, &["memory", "log"])["events"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let reader_export = work.path().join("reader.jsonl");
    assert_eq!(
        json_of(
            &reader,
            &["memory", "export", "-o", reader_export.to_str().unwrap()]
        )["count"],
        1
    );
    assert_eq!(
        failing(
            &reader,
            &["memory", "remember", "--title", "Readers cannot write"],
            4
        )["code"],
        "unauthorized"
    );
    assert_eq!(
        failing(&reader, &["memory", "confirm", &id, "--evidence", "e"], 4)["code"],
        "unauthorized"
    );

    // Every other verb once, on the real data plane.
    let revised = json_of(
        &a,
        &[
            "memory",
            "revise",
            &id,
            "--body",
            "Run them after 20:00.",
            "--add-tag",
            "ops",
            "--reason",
            "clearer",
        ],
    );
    assert_eq!(
        (
            revised["rev"].clone(),
            revised["status"].clone(),
            revised["tags"].clone()
        ),
        (json!(2), json!("inferred"), json!(["migrations", "ops"]))
    );
    let tagged = json_of(
        &b,
        &[
            "memory",
            "revise",
            &id,
            "--add-tag",
            "db",
            "--reason",
            "tag",
        ],
    );
    assert_eq!(tagged["rev"], 3);
    let stale = failing(
        &b,
        &[
            "memory",
            "confirm",
            &id,
            "--evidence",
            "old check",
            "--rev",
            "1",
        ],
        6,
    );
    assert_eq!(
        (stale["code"].clone(), stale["current_rev"].clone()),
        (json!("memory_conflict"), json!(3))
    );
    let duplicate = failing(
        &b,
        &[
            "memory",
            "remember",
            "--title",
            "Migration locks the orders table",
        ],
        6,
    );
    assert_eq!(duplicate["code"], "memory_duplicate");
    assert_eq!(duplicate["candidates"][0]["id"], id);
    let forced = json_of(
        &b,
        &[
            "memory",
            "remember",
            "--title",
            "Migration locks the orders table",
            "--force",
        ],
    );
    let second = forced["id"].as_str().unwrap().to_owned();

    let disputed = json_of(
        &b,
        &["memory", "dispute", &id, "--reason", "only on large tables"],
    );
    assert_eq!(
        (
            disputed["status"].clone(),
            disputed["open_disputes"].clone()
        ),
        (json!("disputed"), json!(1))
    );
    let resolved = json_of(
        &a,
        &["memory", "resolve", &id, "--reason", "true for every table"],
    );
    assert_eq!(resolved["status"], "inferred");
    assert_eq!(
        failing(&a, &["memory", "resolve", &id, "--reason", "again"], 6)["code"],
        "memory_no_open_dispute"
    );

    let superseded = json_of(
        &a,
        &[
            "memory",
            "supersede",
            &second,
            "--by",
            &id,
            "--reason",
            "duplicate",
        ],
    );
    assert_eq!(superseded["superseded"]["status"], "superseded");
    assert_eq!(superseded["by"]["supersedes"], second.as_str());
    assert_eq!(
        failing(
            &a,
            &[
                "memory",
                "supersede",
                &id,
                "--by",
                &second,
                "--reason",
                "back"
            ],
            6
        )["code"],
        "memory_cycle"
    );

    let note = json_of(
        &a,
        &[
            "memory",
            "remember",
            "--title",
            "Build cache lives in the bucket",
            "--kind",
            "fact",
            "--scope",
            "client:acme",
        ],
    );
    let note_id = note["id"].as_str().unwrap().to_owned();
    assert_eq!(
        json_of(
            &a,
            &["memory", "forget", &note_id, "--reason", "bucket gone"]
        )["status"],
        "forgotten"
    );
    let leak = json_of(
        &a,
        &[
            "memory",
            "remember",
            "--title",
            "Scratch note to purge",
            "--body",
            "nothing secret",
        ],
    );
    let leak_id = leak["id"].as_str().unwrap().to_owned();
    assert_eq!(
        json_of(
            &a,
            &[
                "memory",
                "forget",
                &leak_id,
                "--reason",
                "test purge",
                "--purge"
            ]
        ),
        json!({"id": leak_id, "purged": true})
    );
    let gone = failing(&a, &["memory", "show", &leak_id], 5);
    assert!(
        gone["message"].as_str().unwrap().contains("was purged at"),
        "{gone}"
    );

    assert_eq!(
        ids(&json_of(
            &a,
            &["memory", "recall", "orders", "--kind", "pitfall"]
        )),
        [id.as_str()]
    );
    assert_eq!(
        ids(&json_of(
            &a,
            &[
                "memory",
                "recall",
                "orders",
                "--tag",
                "migrations",
                "--tag",
                "ops"
            ]
        )),
        [id.as_str()]
    );
    assert_eq!(
        ids(&json_of(
            &a,
            &["memory", "recall", "orders", "--status", "superseded"]
        )),
        [second.as_str()]
    );
    assert_eq!(
        ids(&json_of(
            &a,
            &[
                "memory",
                "recall",
                "bucket",
                "--all-scopes",
                "--status",
                "forgotten"
            ]
        )),
        [note_id.as_str()]
    );
    let listed = json_of(&a, &["memory", "recall", "--all-scopes"]);
    assert_eq!(listed["scopes"], Value::Null);
    assert_eq!(
        ids(&listed),
        [id.as_str()],
        "superseded and forgotten are hidden"
    );

    let export = work.path().join("memories.jsonl");
    let exported = json_of(&a, &["memory", "export", "-o", export.to_str().unwrap()]);
    assert_eq!(exported["count"], 2);
    let lines: Vec<Value> = std::fs::read_to_string(&export)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let exported_ids: Vec<&str> = lines.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(exported_ids, [id.as_str(), second.as_str()]);

    let secret = format!("{}{}", ["mz", "k_"].concat(), "q7Rx".repeat(6));
    let refused = failing(
        &a,
        &["memory", "remember", "--title", "t", "--body", &secret],
        7,
    );
    assert_eq!(
        (refused["code"].clone(), refused["field"].clone()),
        (json!("memory_secret_detected"), json!("body"))
    );

    a.run(&["db", "create", "plain"], "").ok();
    assert_eq!(
        failing(&a, &["memory", "recall", "--db", "plain"], 6)["code"],
        "memory_not_initialized"
    );
    let run = json_of(
        &a,
        &[
            "memory",
            "init",
            "--db",
            "run-1",
            "--burstable",
            "--ttl",
            "1h",
        ],
    );
    assert_eq!(
        (
            run["created"].clone(),
            run["database"]["kind"].clone(),
            run["database"]["idle_ttl_seconds"].clone()
        ),
        (json!(true), json!("burstable"), json!(3600))
    );

    // Writers racing on one rev: the guard fails in a real write batch,
    // and its SQLITE_CONSTRAINT travels back through the client.
    let target = json_of(
        &a,
        &[
            "memory",
            "remember",
            "--title",
            "Burst target on the real gateway",
        ],
    );
    let target = target["id"].as_str().unwrap().to_owned();
    let mut conflicts = 0;
    for round in 0..5 {
        conflicts += burst(&gateway, work.path(), [&key_a, &key_b], &target, round);
        if conflicts > 0 {
            break;
        }
    }
    assert!(conflicts > 0, "no burst ever made a writer conflict");

    let logs = gateway.logs.lock().unwrap().join("\n");
    assert!(!logs.contains(&key_a) && !logs.contains(&key_b) && !logs.contains(&secret));
}

/// Six writers of two machines, each in its own session, `revise` memory
/// `id` at once. Every one succeeds or conflicts, the rev grows by the
/// successes and the history stays dense. The number of conflicts.
fn burst(gateway: &Gateway, dir: &Path, keys: [&str; 2], id: &str, round: usize) -> usize {
    const WRITERS: usize = 6;
    let reader = machine(gateway, dir, keys[0], "burst", "burst-reader");
    let before = json_of(&reader, &["memory", "show", id])["memory"]["rev"]
        .as_i64()
        .unwrap();
    let writers: Vec<Memoire> = (0..WRITERS)
        .map(|i| {
            machine(
                gateway,
                dir,
                keys[i % 2],
                &format!("burst-{}", i % 2),
                &format!("burst-{round}-{i}"),
            )
        })
        .collect();
    let runs: Vec<support::Run> = std::thread::scope(|scope| {
        let handles: Vec<_> = writers
            .iter()
            .enumerate()
            .map(|(i, writer)| {
                scope.spawn(move || {
                    let title = format!("Burst target, round {round}, writer {i}");
                    writer.run(
                        &[
                            "memory", "revise", id, "--title", &title, "--reason", "burst",
                            "--json",
                        ],
                        "",
                    )
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut successes = 0;
    let mut conflicts = 0;
    for run in &runs {
        match run.code {
            0 => successes += 1,
            6 => {
                let error = &run.error()["error"];
                assert_eq!(error["code"], "memory_conflict", "{}", run.stderr);
                assert!(error["current_rev"].as_i64().unwrap() > before, "{error}");
                conflicts += 1;
            }
            other => panic!("a burst writer exited {other}: {}", run.stderr),
        }
    }
    assert!(successes >= 1);
    let shown = json_of(&reader, &["memory", "show", id, "--history"]);
    let rev = shown["memory"]["rev"].as_i64().unwrap();
    assert_eq!(rev, before + successes);
    let revs: Vec<i64> = shown["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["rev"].as_i64().unwrap())
        .collect();
    assert_eq!(revs, (1..=rev).collect::<Vec<_>>(), "a dense history");
    conflicts
}
