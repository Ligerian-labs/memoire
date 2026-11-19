//! Real `memoire-gateway run` processes on throwaway PostgreSQL databases
//! (`MEREZHA_TEST_DATABASE_URL`), driven over HTTP.
use merezha_postgres::TestDatabase;
use serde_json::Value;
use std::{
    io::{BufRead as _, BufReader, Read as _, Write as _},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const RUNTIME: &str = include_str!("../../../config/dev/runtime.toml");

/// Writes a gateway configuration into `dir`; `memoire` is the body of
/// `[modules.memoire]`.
fn config(dir: &Path, memoire: &str) -> PathBuf {
    std::fs::write(dir.join("runtime.toml"), RUNTIME).unwrap();
    std::fs::write(
        dir.join("identity.key"),
        "a-memoire-process-test-identity-secret-key-of-48-bytes",
    )
    .unwrap();
    std::fs::write(
        dir.join("notifications.key"),
        "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=",
    )
    .unwrap();
    let text = format!(
        r#"
[app]
id = "memoire-test"
[http]
listen = "127.0.0.1:0"
[database]
url = {{ env = "MEMOIRE_TEST_DATABASE_URL" }}
migrate = true
pool_size = 4
max_detached = 3
[runtime]
manifest = "runtime.toml"
[modules.identity]
app_origin = "http://localhost:4200"
secret_key = {{ file = "identity.key" }}
[modules.identity.password]
argon2_memory_kib = 19456
[modules.notifications]
secret_key = {{ file = "notifications.key" }}
[modules.notifications.templates]
allow_http_links = true
[modules.memoire]
{memoire}
"#
    );
    let path = dir.join("memoire.toml");
    std::fs::write(&path, text).unwrap();
    path
}

struct Gateway {
    child: Child,
    address: String,
    logs: Arc<Mutex<Vec<String>>>,
}

impl Gateway {
    fn spawn(config: &Path, db: &TestDatabase) -> Child {
        Command::new(env!("CARGO_BIN_EXE_memoire-gateway"))
            .args(["run", "--config"])
            .arg(config)
            .env("MEMOIRE_TEST_DATABASE_URL", db.url())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn start(config: &Path, db: &TestDatabase) -> Self {
        let mut child = Self::spawn(config, db);
        let logs = Arc::new(Mutex::new(Vec::new()));
        let sink = logs.clone();
        let stderr = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                sink.lock().unwrap().push(line);
            }
        });
        let started = Instant::now();
        let address = loop {
            let found = logs.lock().unwrap().iter().find_map(|line| {
                let event: Value = serde_json::from_str(line).ok()?;
                (event["event"] == "listener.ready")
                    .then(|| event["address"].as_str().map(str::to_owned))?
            });
            if let Some(address) = found {
                break address;
            }
            if let Ok(Some(status)) = child.try_wait() {
                panic!("gateway exited {status}: {:?}", logs.lock().unwrap());
            }
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "no listener.ready: {:?}",
                logs.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        let gateway = Self {
            child,
            address,
            logs,
        };
        gateway.wait_ready();
        gateway
    }

    fn wait_ready(&self) {
        let started = Instant::now();
        while self.get("/health/ready", None).0 != 200 {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "never ready: {:?}",
                self.logs.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// A minimal HTTP/1.1 GET: the status and the JSON body (`Null` when
    /// the body is not JSON).
    fn get(&self, path: &str, bearer: Option<&str>) -> (u16, Value) {
        let auth = bearer.map(|t| format!("Bearer {t}"));
        let headers: Vec<(&str, &str)> =
            auth.iter().map(|a| ("authorization", a.as_str())).collect();
        self.request("GET", path, &headers, None)
    }

    /// One HTTP/1.1 request with `connection: close`; `body` is sent as JSON.
    fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&Value>,
    ) -> (u16, Value) {
        let Ok(mut stream) = TcpStream::connect(&self.address) else {
            return (0, Value::Null);
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let body = body.map(Value::to_string).unwrap_or_default();
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nhost: {}\r\nconnection: close\r\ncontent-length: {}\r\n",
            self.address,
            body.len()
        );
        if !body.is_empty() {
            head.push_str("content-type: application/json\r\n");
        }
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body.as_bytes()).unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        let status = response
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let (head, body) = response.split_once("\r\n\r\n").unwrap_or(("", ""));
        let body = if head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            dechunk(body)
        } else {
            body.to_owned()
        };
        (status, serde_json::from_str(&body).unwrap_or(Value::Null))
    }

    /// SIGTERM and the exit status.
    fn stop(mut self) -> std::process::ExitStatus {
        Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        self.child.wait().unwrap()
    }
}

/// The payload of a `transfer-encoding: chunked` body.
fn dechunk(mut body: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = body.split_once("\r\n") {
        let Ok(size) = usize::from_str_radix(size.trim(), 16) else {
            break;
        };
        if size == 0 || rest.len() < size {
            break;
        }
        out.push_str(&rest[..size]);
        body = rest[size..].trim_start_matches("\r\n");
    }
    out
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn boots_ready_and_serves_the_memoire_routes() {
    let Some(db) = TestDatabase::create() else {
        eprintln!("skipped: MEREZHA_TEST_DATABASE_URL is unset");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::start(&config(dir.path(), r#"data_dir = "data""#), &db);

    assert_eq!(gateway.get("/health/live", None).0, 200);
    assert_eq!(gateway.get("/health/ready", None).0, 200);

    let teams = dir.path().join("data/teams");
    assert!(teams.is_dir(), "the module creates its data directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&teams).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    let (status, body) = gateway.get("/v1/memoire/whoami", None);
    assert_eq!(status, 401, "{body}");
    assert_eq!(body["error"]["code"], "unauthenticated");

    // A malformed credential is refused by identity at the edge.
    let (status, body) = gateway.get("/v1/memoire/whoami", Some("mzk_not-a-key"));
    assert_eq!(status, 401, "{body}");

    let (status, _) = gateway.get("/v1/memoire/no-such-route", None);
    assert_eq!(status, 404);

    assert!(
        gateway.stop().success(),
        "SIGTERM stops the gateway cleanly"
    );
}

#[test]
fn refuses_to_start_without_a_data_dir() {
    let Some(db) = TestDatabase::create() else {
        eprintln!("skipped: MEREZHA_TEST_DATABASE_URL is unset");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut child = Gateway::spawn(&config(dir.path(), ""), &db);
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("invalid_config"), "{stderr}");
    assert!(stderr.contains("modules.memoire.data_dir"), "{stderr}");
}

/// `whoami` through identity: a verified member's bearer session without and
/// with a team, then a machine's API key, which carries its team.
#[test]
fn whoami_reports_members_teams_and_machine_keys() {
    let Some(db) = TestDatabase::create() else {
        eprintln!("skipped: MEREZHA_TEST_DATABASE_URL is unset");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::start(&config(dir.path(), r#"data_dir = "data""#), &db);
    let origin = [("origin", "http://localhost:4200")];

    let (status, body) = gateway.request(
        "POST",
        "/v1/identity/registrations",
        &origin,
        Some(&serde_json::json!({
            "email": "member@memoire.test", "password": "correct horse battery staple 42",
        })),
    );
    assert!(status < 300, "{status} {body}");
    assert_eq!(body["status"], "pending_verification");
    let token = verification_token(&gateway);
    let (status, body) = gateway.request(
        "POST",
        "/v1/identity/email-verifications/confirm",
        &origin,
        Some(&serde_json::json!({"token": token, "transport": "bearer"})),
    );
    assert_eq!(status, 201, "{body}");
    let session = body["session"]["token"].as_str().unwrap().to_owned();
    let user = body["user"]["id"].as_str().unwrap().to_owned();
    // A key expiry inside identity's allowed range: the session's own.
    let expires_at = body["session"]["idle_expires_at"].clone();
    let bearer = format!("Bearer {session}");
    let auth = [("authorization", bearer.as_str())];

    let (status, body) = gateway.get("/v1/memoire/whoami", Some(&session));
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        serde_json::json!({"subject": user, "kind": "user", "team_id": null})
    );

    let (status, body) = gateway.request(
        "POST",
        "/v1/identity/organizations",
        &auth,
        Some(&serde_json::json!({"name": "Test team", "slug": "test-team"})),
    );
    assert_eq!(status, 201, "{body}");
    let team = body["organization"]["id"].as_str().unwrap().to_owned();
    let (_, body) = gateway.request(
        "GET",
        "/v1/memoire/whoami",
        &[auth[0], ("merezha-organization", team.as_str())],
        None,
    );
    assert_eq!(body["team_id"], team.as_str());

    let (status, body) = gateway.request(
        "POST",
        &format!("/v1/identity/organizations/{team}/service-accounts"),
        &auth,
        Some(&serde_json::json!({"name": "ci-runner"})),
    );
    assert_eq!(status, 201, "{body}");
    let machine = body["service_account"]["id"].as_str().unwrap().to_owned();
    let (status, body) = gateway.request(
        "POST",
        &format!("/v1/identity/organizations/{team}/api-keys"),
        &auth,
        Some(&serde_json::json!({
            "name": "ci-runner", "permissions": ["identity.organization.read"],
            "expires_at": expires_at, "service_account_id": machine,
        })),
    );
    assert_eq!(status, 201, "{body}");
    let key = body["key"].as_str().unwrap().to_owned();
    assert!(key.starts_with("mzk_"));
    let key_id = body["api_key"]["id"].as_str().unwrap().to_owned();

    let (status, body) = gateway.get("/v1/memoire/whoami", Some(&key));
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        serde_json::json!({"subject": key_id, "kind": "api_key", "team_id": team})
    );
}

/// The verification token of the last captured mail.
fn verification_token(gateway: &Gateway) -> String {
    let started = Instant::now();
    loop {
        let (_, captures) = gateway.get("/v1/notifications/dev/captures", None);
        let text = captures
            .as_array()
            .and_then(|all| all.last())
            .and_then(|mail| mail["rendered"]["text"].as_str())
            .map(str::to_owned);
        if let Some(token) = text
            .as_deref()
            .and_then(|t| t.split_once("#token="))
            .map(|(_, rest)| rest.split_whitespace().next().unwrap_or("").to_owned())
        {
            return token;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "no verification mail: {captures}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}
