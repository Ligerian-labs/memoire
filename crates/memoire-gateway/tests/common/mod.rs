//! The process-test harness: real `memoire-gateway run` processes on
//! throwaway PostgreSQL databases (`MEREZHA_TEST_DATABASE_URL`), driven over
//! HTTP, and identity helpers for signed-in members, teams and keys.
//!
//! The catalog guest must be built first (`scripts/build-guests.sh`).
#![allow(dead_code)]
use merezha_postgres::TestDatabase;
use serde_json::{Value, json};
use std::{
    io::{BufRead as _, BufReader, Read as _, Write as _},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const ORIGIN: &str = "http://localhost:4200";
const APPLICATION: &str = include_str!("../../../../config/dev/application.toml");
const RUNTIME: &str = include_str!("../../../../guests/runtime.toml");

/// The built catalog component.
pub fn catalog_component() -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../guests/components/memoire-catalog.wasm")
        .canonicalize()
        .unwrap_or_else(|_| panic!("build the guests first: scripts/build-guests.sh"));
    assert!(
        path.is_file(),
        "build the guests first: scripts/build-guests.sh"
    );
    path
}

/// The throwaway database, or `None` when the tests may skip (no
/// `MEREZHA_TEST_DATABASE_URL` and no `MEREZHA_TEST_REQUIRE_DATABASE`).
pub fn database() -> Option<TestDatabase> {
    let db = TestDatabase::create();
    if db.is_none() {
        eprintln!("skipped: MEREZHA_TEST_DATABASE_URL is unset");
    }
    db
}

/// Writes a gateway configuration into `dir`, the development
/// configuration's application and runtime included; `memoire` is the body
/// of `[modules.memoire]`.
pub fn config(dir: &Path, memoire: &str) -> PathBuf {
    config_with(dir, memoire, "")
}

/// [`config`] with `extra` appended at the top level (`[jobs]` settings).
pub fn config_with(dir: &Path, memoire: &str, extra: &str) -> PathBuf {
    // Runtime paths stay inside the manifest's directory.
    std::fs::create_dir_all(dir.join("components")).unwrap();
    std::fs::copy(
        catalog_component(),
        dir.join("components/memoire-catalog.wasm"),
    )
    .unwrap();
    std::fs::write(dir.join("runtime.toml"), RUNTIME).unwrap();
    std::fs::write(dir.join("application.toml"), APPLICATION).unwrap();
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
pool_size = 16
max_detached = 3
[runtime]
manifest = "runtime.toml"
[application]
path = "application.toml"
[application.projections]
interval_ms = 200
[modules.identity]
app_origin = "{ORIGIN}"
secret_key = {{ file = "identity.key" }}
registration = "open"
[modules.identity.rbac.builtin_grants]
admin = ["memoire.databases.*"]
member = ["memoire.databases.*"]
[modules.identity.password]
argon2_memory_kib = 19456
[modules.notifications]
secret_key = {{ file = "notifications.key" }}
[modules.notifications.templates]
allow_http_links = true
[modules.memoire]
{memoire}
{extra}
"#
    );
    let path = dir.join("memoire.toml");
    std::fs::write(&path, text).unwrap();
    path
}

pub struct Gateway {
    child: Child,
    pub address: String,
    pub logs: Arc<Mutex<Vec<String>>>,
}

impl Gateway {
    pub fn spawn(config: &Path, db: &TestDatabase) -> Child {
        Command::new(env!("CARGO_BIN_EXE_memoire-gateway"))
            .args(["run", "--config"])
            .arg(config)
            .env("MEMOIRE_TEST_DATABASE_URL", db.url())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    pub fn start(config: &Path, db: &TestDatabase) -> Self {
        let mut child = Self::spawn(config, db);
        let logs = Arc::new(Mutex::new(Vec::new()));
        let sink = logs.clone();
        let stderr = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                sink.lock().unwrap().push(line);
            }
        });
        // Owned from here, so a failed assertion still stops the process.
        let mut gateway = Self {
            child,
            address: String::new(),
            logs,
        };
        let started = Instant::now();
        gateway.address = loop {
            let found = gateway.logs.lock().unwrap().iter().find_map(|line| {
                let event: Value = serde_json::from_str(line).ok()?;
                (event["event"] == "listener.ready")
                    .then(|| event["address"].as_str().map(str::to_owned))?
            });
            if let Some(address) = found {
                break address;
            }
            if let Ok(Some(status)) = gateway.child.try_wait() {
                panic!(
                    "gateway exited {status}: {:?}",
                    gateway.logs.lock().unwrap()
                );
            }
            assert!(
                started.elapsed() < Duration::from_secs(120),
                "no listener.ready: {:?}",
                gateway.logs.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        gateway.wait_ready();
        gateway
    }

    pub fn wait_ready(&self) {
        let started = Instant::now();
        while self.get("/health/ready", None).0 != 200 {
            let logs = self.logs.lock().unwrap();
            if logs.iter().any(|l| l.contains("startup.failed")) {
                let failed: Vec<_> = logs.iter().filter(|l| l.contains("failed")).collect();
                panic!("startup failed: {failed:#?}");
            }
            drop(logs);
            assert!(
                started.elapsed() < Duration::from_secs(120),
                "never ready: {:?}",
                self.logs.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// A minimal HTTP/1.1 GET: the status and the JSON body (`Null` when
    /// the body is not JSON).
    pub fn get(&self, path: &str, bearer: Option<&str>) -> (u16, Value) {
        let auth = bearer.map(|t| format!("Bearer {t}"));
        let headers: Vec<(&str, &str)> =
            auth.iter().map(|a| ("authorization", a.as_str())).collect();
        self.request("GET", path, &headers, None)
    }

    /// One HTTP/1.1 request with `connection: close`; `body` is sent as JSON.
    pub fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&Value>,
    ) -> (u16, Value) {
        let body = body.map(Value::to_string);
        let (status, _, body) = self.raw(
            method,
            path,
            headers,
            body.as_deref().map(|b| ("application/json", b.as_bytes())),
        );
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    /// One request with any body: the status, the response head and the
    /// body bytes.
    pub fn raw(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<(&str, &[u8])>,
    ) -> (u16, String, Vec<u8>) {
        let Ok(mut stream) = TcpStream::connect(&self.address) else {
            return (0, String::new(), Vec::new());
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let (content_type, bytes) = body.unwrap_or(("", b""));
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nhost: {}\r\nconnection: close\r\ncontent-length: {}\r\n",
            self.address,
            bytes.len()
        );
        if !content_type.is_empty() {
            head.push_str(&format!("content-type: {content_type}\r\n"));
        }
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).unwrap();
        // A server refusing a large body early may close before it is all
        // sent; its 413 is still there to read.
        let sent = stream.write_all(bytes);
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);
        let split = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap_or(response.len());
        let head = String::from_utf8_lossy(&response[..split]).into_owned();
        let body = response.get(split + 4..).unwrap_or_default().to_vec();
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if let Err(error) = sent {
            assert_eq!(status, 413, "the request was cut short: {error}");
        }
        let body = if head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            dechunk(&body)
        } else {
            body
        };
        (status, head, body)
    }

    /// The process id.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// SIGTERM and the exit status.
    pub fn stop(mut self) -> std::process::ExitStatus {
        Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        self.child.wait().unwrap()
    }
}

/// The payload of a `transfer-encoding: chunked` body.
fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(end) = body.windows(2).position(|w| w == b"\r\n") {
        let Ok(size) = usize::from_str_radix(String::from_utf8_lossy(&body[..end]).trim(), 16)
        else {
            break;
        };
        let rest = &body[end + 2..];
        if size == 0 || rest.len() < size {
            break;
        }
        out.extend_from_slice(&rest[..size]);
        body = rest[size..].strip_prefix(b"\r\n").unwrap_or(&rest[size..]);
    }
    out
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The token of the newest captured mail to `email` (the capture driver
/// lists the newest first, with the recipient as `target`).
pub fn mailed_token(gateway: &Gateway, email: &str) -> String {
    let started = Instant::now();
    loop {
        let (_, captures) = gateway.get("/v1/notifications/dev/captures?limit=100", None);
        let token = captures
            .as_array()
            .into_iter()
            .flatten()
            .filter(|mail| mail["target"] == email)
            .filter_map(|mail| mail["rendered"]["text"].as_str())
            .find_map(|text| {
                let (_, rest) = text.split_once("#token=")?;
                Some(rest.split_whitespace().next()?.to_owned())
            });
        if let Some(token) = token {
            return token;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "no mail to {email}: {captures}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// A signed-in member: the bearer session, the user id and an expiry an
/// API key may take.
pub struct Member {
    pub session: String,
    pub user: String,
    pub expires_at: Value,
}

impl Member {
    pub fn bearer(&self) -> String {
        format!("Bearer {}", self.session)
    }
}

/// The password every test account gets.
pub const PASSWORD: &str = "correct horse battery staple 42";

/// The `name=value` pairs of a response head's `Set-Cookie` headers, as one
/// `Cookie` header value.
pub fn cookies(head: &str) -> String {
    head.lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("set-cookie")
                .then(|| value.trim())
        })
        .filter_map(|value| value.split(';').next())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Registers `email` with [`PASSWORD`], verifies it through the captured
/// mail and returns the bearer session. The confirmation carries the
/// registration's binding cookie, so the account keeps its password.
pub fn register(gateway: &Gateway, email: &str) -> Member {
    let body = json!({"email": email, "password": PASSWORD}).to_string();
    let (status, head, _) = gateway.raw(
        "POST",
        "/v1/identity/registrations",
        &[("origin", ORIGIN)],
        Some(("application/json", body.as_bytes())),
    );
    assert!(status < 300, "{status} {head}");
    let binding = cookies(&head);
    let token = mailed_token(gateway, email);
    let (status, body) = gateway.request(
        "POST",
        "/v1/identity/email-verifications/confirm",
        &[("origin", ORIGIN), ("cookie", binding.as_str())],
        Some(&json!({"token": token, "transport": "bearer"})),
    );
    assert_eq!(status, 201, "{body}");
    assert_ne!(body["session"]["password_unset"], true, "{body}");
    Member {
        session: body["session"]["token"].as_str().unwrap().to_owned(),
        user: body["user"]["id"].as_str().unwrap().to_owned(),
        expires_at: body["session"]["idle_expires_at"].clone(),
    }
}

/// Creates a team owned by `member`; its `org_` id.
pub fn create_team(gateway: &Gateway, member: &Member, slug: &str) -> String {
    let bearer = member.bearer();
    let (status, body) = gateway.request(
        "POST",
        "/v1/identity/organizations",
        &[("authorization", bearer.as_str())],
        Some(&json!({"name": slug, "slug": slug})),
    );
    assert_eq!(status, 201, "{body}");
    body["organization"]["id"].as_str().unwrap().to_owned()
}

/// A team API key with `permissions`, the member's own or, with
/// `service_account`, that machine's: the `mzk_` secret.
pub fn api_key(
    gateway: &Gateway,
    member: &Member,
    team: &str,
    permissions: &[&str],
    service_account: Option<&str>,
) -> (u16, Value) {
    let bearer = member.bearer();
    let mut body =
        json!({"name": "test-key", "permissions": permissions, "expires_at": member.expires_at});
    if let Some(machine) = service_account {
        body["service_account_id"] = json!(machine);
    }
    gateway.request(
        "POST",
        &format!("/v1/identity/organizations/{team}/api-keys"),
        &[("authorization", bearer.as_str())],
        Some(&body),
    )
}

/// The five memoire permissions.
pub const MEMOIRE_PERMISSIONS: &[&str] = &[
    "memoire.databases.read",
    "memoire.databases.write",
    "memoire.databases.create",
    "memoire.databases.delete",
    "memoire.databases.snapshots",
];
