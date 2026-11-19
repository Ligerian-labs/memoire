//! The `memoire` binary against a real `memoire-gateway` on a throwaway
//! PostgreSQL database (`MEREZHA_TEST_DATABASE_URL`), through every route
//! the CLI uses.
//!
//! The gateway runs the repository's development configuration
//! (`config/dev/`), copied into a temporary directory with synthetic
//! secrets, on 127.0.0.1:28971. Its binary is the workspace build's
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
fn every_command_works_against_a_real_gateway() {
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
#[ignore = "needs the gateway's export, import and snapshot routes"]
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
