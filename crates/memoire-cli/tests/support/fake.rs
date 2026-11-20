//! An in-process gateway that follows `docs/api.md`: the memoire routes on
//! real SQLite files, and the identity routes `keys create` uses. Its clock
//! stands still; tests move state with the hooks on [`Fake`].
// Handlers fail with the HTTP reply itself.
#![allow(clippy::result_large_err)]
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse as _, Response},
};
use memoire_api::{
    Column, CreateDatabase, Database, DatabaseKind, DatabaseState, ExtendDatabase, Params,
    QueryMode, QueryRequest, Schema, SchemaColumn, Snapshot, SnapshotReason, SqlValue,
    StatementResult, Table, View,
};
use rusqlite::{
    Connection, OpenFlags,
    config::DbConfig,
    hooks::{AuthAction, AuthContext, Authorization},
    types::ValueRef,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub const MEMBER_KEY: &str = "mzk_fake_member_key";
pub const TEAM: &str = "org_fake_team";
pub const EMAIL: &str = "member@memoire.test";
pub const PASSWORD: &str = "a synthetic password";
const CREATED_AT: &str = "2026-11-19T10:00:00Z";
const USED_AT: &str = "2026-11-19T10:30:00Z";

/// One request as the fake saw it.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub authorization: Option<String>,
    pub origin: Option<String>,
    pub organization: Option<String>,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

struct Key {
    subject: String,
    team: Option<String>,
    permissions: Vec<String>,
    /// A machine's key acts only once its service account holds a role.
    machine: Option<String>,
}

struct Db {
    info: Database,
    path: PathBuf,
    snapshots: Vec<(Snapshot, PathBuf)>,
}

struct Inner {
    dir: tempfile::TempDir,
    keys: HashMap<String, Key>,
    sessions: Vec<String>,
    /// Service accounts that hold the `member` role.
    with_role: Vec<String>,
    databases: BTreeMap<String, Db>,
    serial: u64,
    page_size: usize,
    requests: Vec<Seen>,
    fail_next: Option<(u16, Value, Option<u64>)>,
    before_write: Option<Box<dyn FnOnce() + Send>>,
    /// Whether it serves the memory routes and knows their permissions;
    /// without them it is a gateway from before the memory API.
    memory_routes: bool,
}

#[derive(Clone)]
pub struct Fake {
    pub url: String,
    inner: Arc<Mutex<Inner>>,
}

impl Fake {
    /// A gateway with the memory routes.
    pub fn start() -> Self {
        Self::start_with(true)
    }

    /// A gateway from before the memory API: its memory routes answer
    /// `404 not_found`, as any unknown route, and identity refuses the
    /// `memoire.memory.*` permissions.
    pub fn start_without_memory() -> Self {
        Self::start_with(false)
    }

    fn start_with(memory_routes: bool) -> Self {
        let mut keys = HashMap::new();
        keys.insert(
            MEMBER_KEY.to_owned(),
            Key {
                subject: "key_fake_member".into(),
                team: Some(TEAM.into()),
                permissions: ALL.iter().map(|p| (*p).to_owned()).collect(),
                machine: None,
            },
        );
        let inner = Arc::new(Mutex::new(Inner {
            dir: tempfile::tempdir().unwrap(),
            keys,
            sessions: Vec::new(),
            with_role: Vec::new(),
            databases: BTreeMap::new(),
            serial: 0,
            page_size: 2,
            requests: Vec::new(),
            fail_next: None,
            before_write: None,
            memory_routes,
        }));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().fallback(handle).with_state(inner.clone());
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                axum::serve(listener, app).await.unwrap();
            });
        });
        Self { url, inner }
    }

    /// A key of `team` (`None`: an application key) holding `permissions`.
    pub fn add_key(&self, key: &str, team: Option<&str>, permissions: &[&str]) {
        self.add_key_as(key, &format!("key_{}", key.len()), team, permissions);
    }

    /// [`Fake::add_key`] with the `subject` whoami reports for it.
    pub fn add_key_as(&self, key: &str, subject: &str, team: Option<&str>, permissions: &[&str]) {
        self.inner.lock().unwrap().keys.insert(
            key.into(),
            Key {
                subject: subject.into(),
                team: team.map(str::to_owned),
                permissions: permissions.iter().map(|p| (*p).to_owned()).collect(),
                machine: None,
            },
        );
    }

    /// Runs `hook` once, just before the fake runs the next write batch (a
    /// write query, or a memory route's write), without holding the fake's
    /// lock: the hook may drive the CLI
    /// against this fake, to slip a second writer in between another
    /// writer's read and its write.
    pub fn before_next_write(&self, hook: impl FnOnce() + Send + 'static) {
        self.inner.lock().unwrap().before_write = Some(Box::new(hook));
    }

    /// The SQLite file of database `name`.
    pub fn database_path(&self, name: &str) -> PathBuf {
        self.inner.lock().unwrap().databases[name].path.clone()
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.inner.lock().unwrap().requests.clone()
    }

    pub fn open_sessions(&self) -> usize {
        self.inner.lock().unwrap().sessions.len()
    }

    /// The next request answers `status` with this error code.
    pub fn fail_next(&self, status: u16, code: &str, classification: &str, retry: Option<u64>) {
        self.inner.lock().unwrap().fail_next = Some((
            status,
            json!({"error": {"code": code, "classification": classification}}),
            retry,
        ));
    }

    /// The sweeper's work: the file goes, the entry stays as expired.
    pub fn expire(&self, name: &str) {
        let mut inner = self.inner.lock().unwrap();
        let db = inner.databases.get_mut(name).unwrap();
        std::fs::remove_file(&db.path).unwrap();
        db.info.state = DatabaseState::Expired;
        db.info.size_bytes = None;
    }

    /// A scheduled snapshot of a persistent database; its id.
    pub fn snapshot(&self, name: &str) -> String {
        let mut inner = self.inner.lock().unwrap();
        take_snapshot(&mut inner, name, SnapshotReason::Scheduled)
    }
}

const ALL: [&str; 5] = [
    "memoire.databases.read",
    "memoire.databases.write",
    "memoire.databases.create",
    "memoire.databases.delete",
    "memoire.databases.snapshots",
];
const MEMORY: [&str; 2] = ["memoire.memory.read", "memoire.memory.write"];

fn take_snapshot(inner: &mut Inner, name: &str, reason: SnapshotReason) -> String {
    inner.serial += 1;
    let id = format!("snap_{:04}", inner.serial);
    let path = inner.dir.path().join(format!("{id}.sqlite"));
    let db = inner.databases.get_mut(name).unwrap();
    vacuum_into(&db.path, &path);
    let snapshot = Snapshot {
        id: id.clone(),
        created_at: CREATED_AT.into(),
        size_bytes: std::fs::metadata(&path).unwrap().len(),
        reason,
    };
    db.snapshots.insert(0, (snapshot, path));
    id
}

fn vacuum_into(from: &std::path::Path, to: &std::path::Path) {
    let conn = Connection::open(from).unwrap();
    conn.execute("VACUUM INTO ?1", [to.to_str().unwrap()])
        .unwrap();
}

type Shared = Arc<Mutex<Inner>>;
type Reply = Result<Response, Response>;

fn reply(status: u16, body: &Value) -> Response {
    let mut response = (
        StatusCode::from_u16(status).unwrap(),
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body.to_string(),
    )
        .into_response();
    if status == 204 {
        *response.body_mut() = axum::body::Body::empty();
    }
    response
}

fn ok(body: impl serde::Serialize) -> Reply {
    Ok(reply(200, &serde_json::to_value(body).unwrap()))
}

fn fail(status: u16, code: &str) -> Response {
    let classification = match status {
        409 => "conflict",
        429 | 503 => "transient",
        _ => "permanent",
    };
    reply(
        status,
        &json!({"error": {"code": code, "classification": classification}}),
    )
}

fn invalid(path: &str, code: &str) -> Response {
    reply(
        422,
        &json!({"error": {"code": "validation_failed", "classification": "permanent",
                          "details": [{"path": path, "code": code}]}}),
    )
}

async fn handle(
    State(shared): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let seen = Seen {
        method: method.to_string(),
        path: uri.path().to_owned(),
        query: uri.query().map(str::to_owned),
        authorization: header("authorization"),
        origin: header("origin"),
        organization: header("merezha-organization"),
        content_type: header("content-type"),
        body: body.to_vec(),
    };
    let write_query = method == Method::POST
        && uri.path().ends_with("/query")
        && serde_json::from_slice::<Value>(&body).is_ok_and(|b| b["mode"] == "write");
    if write_query {
        let hook = shared.lock().unwrap().before_write.take();
        if let Some(hook) = hook {
            // Off the server's thread, so the hook's own requests are served.
            tokio::task::spawn_blocking(hook).await.unwrap();
        }
    }
    let segments: Vec<String> = uri
        .path()
        .trim_start_matches('/')
        .split('/')
        .map(percent_decode)
        .collect();
    let memory_route = matches!(
        segments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["v1", "memoire", "databases", _, "memory", ..]
    );
    let answered = {
        let mut inner = shared.lock().unwrap();
        inner.requests.push(seen.clone());
        if let Some((status, body, retry)) = inner.fail_next.take() {
            let mut response = reply(status, &body);
            if let Some(retry) = retry {
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, retry.to_string().parse().unwrap());
            }
            Some(response)
        } else if memory_route && !inner.memory_routes {
            Some(fail(404, "not_found"))
        } else {
            None
        }
    };
    if let Some(response) = answered {
        return response;
    }
    if memory_route {
        let shared = shared.clone();
        // Off the server's thread: a write may run the write hook, whose
        // requests the server must serve meanwhile.
        return tokio::task::spawn_blocking(move || {
            memory::route(&shared, &method, &uri, &segments[3..], &seen)
        })
        .await
        .unwrap();
    }
    let mut inner = shared.lock().unwrap();
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    let result = match segments.as_slice() {
        ["v1", "identity", rest @ ..] => identity(&mut inner, &method, rest, &seen),
        ["v1", "memoire", rest @ ..] => memoire(&mut inner, &method, rest, &seen),
        _ => Err(fail(404, "not_found")),
    };
    result.unwrap_or_else(|e| e)
}

fn percent_decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&segment[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap()
}

fn bearer(seen: &Seen) -> Option<&str> {
    seen.authorization.as_deref()?.strip_prefix("Bearer ")
}

fn json_body<T: serde::de::DeserializeOwned>(seen: &Seen) -> Result<T, Response> {
    if seen.content_type.as_deref() != Some("application/json") {
        return Err(fail(415, "unsupported_media_type"));
    }
    serde_json::from_slice(&seen.body).map_err(|_| invalid("", "invalid"))
}

fn identity(inner: &mut Inner, method: &Method, rest: &[&str], seen: &Seen) -> Reply {
    match (method.as_str(), rest) {
        ("POST", ["sessions"]) => {
            if seen.origin.is_none() {
                return Err(fail(403, "origin_rejected"));
            }
            let body: Value = json_body(seen)?;
            if body["email"] != EMAIL || body["password"] != PASSWORD {
                return Err(fail(401, "invalid_credentials"));
            }
            assert_eq!(body["transport"], "bearer");
            inner.serial += 1;
            let token = format!("mzs_fake_session_{}", inner.serial);
            inner.sessions.push(token.clone());
            Ok(reply(
                201,
                &json!({"session": {"token": token}, "user": {"id": "usr_fake"}, "csrf_token": "c"}),
            ))
        }
        ("DELETE", ["session"]) => {
            let token = bearer(seen).ok_or_else(|| fail(401, "unauthenticated"))?;
            let before = inner.sessions.len();
            inner.sessions.retain(|s| s != token);
            if inner.sessions.len() == before {
                return Err(fail(401, "unauthenticated"));
            }
            Ok(reply(204, &Value::Null))
        }
        ("GET", ["organizations", TEAM, "roles"]) => {
            let token = bearer(seen).ok_or_else(|| fail(401, "unauthenticated"))?;
            if !inner.sessions.iter().any(|s| s == token) {
                return Err(fail(401, "unauthenticated"));
            }
            ok(json!({"items": [{"id": "role_admin", "key": "admin"},
                                {"id": "role_member", "key": "member"}]}))
        }
        ("POST", ["organizations", org, kind]) => {
            let token = bearer(seen).ok_or_else(|| fail(401, "unauthenticated"))?;
            if !inner.sessions.iter().any(|s| s == token) {
                return Err(fail(401, "unauthenticated"));
            }
            if *org != TEAM || seen.organization.as_deref() != Some(TEAM) {
                return Err(fail(404, "not_found"));
            }
            let body: Value = json_body(seen)?;
            inner.serial += 1;
            match *kind {
                "role-assignments" => {
                    if body["role_id"] != "role_member" {
                        return Err(invalid("/role_id", "invalid"));
                    }
                    let machine = body["service_account_id"].as_str().unwrap().to_owned();
                    inner.with_role.push(machine);
                    Ok(reply(201, &json!({"role_assignment": body})))
                }
                "service-accounts" => Ok(reply(
                    201,
                    &json!({"service_account": {"id": format!("sa_{:04}", inner.serial), "name": body["name"]}}),
                )),
                "api-keys" => {
                    let permissions: Vec<String> =
                        serde_json::from_value(body["permissions"].clone()).unwrap();
                    let known =
                        |p: &str| ALL.contains(&p) || (inner.memory_routes && MEMORY.contains(&p));
                    if permissions.iter().any(|p| !known(p)) {
                        return Err(fail(422, "unknown_permission"));
                    }
                    if !body["expires_at"]
                        .as_str()
                        .is_some_and(|t| t.ends_with('Z'))
                    {
                        return Err(invalid("/expires_at", "invalid"));
                    }
                    let key = format!("mzk_fake_machine_{}", inner.serial);
                    let id = format!("key_{:04}", inner.serial);
                    inner.keys.insert(
                        key.clone(),
                        Key {
                            subject: id.clone(),
                            team: Some(TEAM.into()),
                            permissions: permissions.clone(),
                            machine: body["service_account_id"].as_str().map(str::to_owned),
                        },
                    );
                    Ok(reply(
                        201,
                        &json!({"api_key": {"id": id, "name": body["name"], "permissions": permissions,
                                            "expires_at": body["expires_at"],
                                            "service_account_id": body["service_account_id"]},
                                "key": key}),
                    ))
                }
                _ => Err(fail(404, "not_found")),
            }
        }
        _ => Err(fail(404, "not_found")),
    }
}

fn memoire(inner: &mut Inner, method: &Method, rest: &[&str], seen: &Seen) -> Reply {
    let key = bearer(seen)
        .and_then(|k| inner.keys.get(k))
        .ok_or_else(|| fail(401, "unauthenticated"))?;
    if let ("GET", ["whoami"]) = (method.as_str(), rest) {
        return ok(json!({"subject": key.subject, "kind": "api_key", "team_id": key.team}));
    }
    if key.team.is_none() {
        return Err(fail(403, "tenant_required"));
    }
    let roleless = key
        .machine
        .as_ref()
        .is_some_and(|m| !inner.with_role.contains(m));
    let permissions = if roleless {
        Vec::new()
    } else {
        key.permissions.clone()
    };
    let need = |permission: &str| {
        if permissions
            .iter()
            .any(|p| p.strip_prefix("memoire.databases") == Some(permission))
        {
            Ok(())
        } else {
            Err(fail(403, "unauthorized"))
        }
    };
    match (method.as_str(), rest) {
        ("GET", ["databases"]) => {
            need(".read")?;
            list(inner, seen.query.as_deref().unwrap_or(""))
        }
        ("POST", ["databases"]) => {
            need(".create")?;
            create(inner, json_body(seen)?)
        }
        (_, ["databases", name, rest @ ..]) => {
            let name = (*name).to_owned();
            if !inner.databases.contains_key(&name) {
                return Err(fail(404, "database_not_found"));
            }
            database(inner, method.as_str(), &name, rest, seen, &need)
        }
        _ => Err(fail(404, "not_found")),
    }
}

fn list(inner: &Inner, query: &str) -> Reply {
    let params: HashMap<&str, &str> = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .collect();
    let all = match params.get("state").copied().unwrap_or("active") {
        "active" => false,
        "all" => true,
        _ => return Err(invalid("/state", "invalid")),
    };
    let limit: usize = params.get("limit").map_or(50, |l| l.parse().unwrap());
    let limit = limit.min(inner.page_size);
    let after = params.get("cursor").copied();
    let items: Vec<&Database> = inner
        .databases
        .values()
        .map(|db| &db.info)
        .filter(|db| all || db.state == DatabaseState::Active)
        .filter(|db| after.is_none_or(|a| db.name.as_str() > a))
        .collect();
    let page: Vec<&Database> = items.iter().take(limit).copied().collect();
    let next = (items.len() > limit).then(|| page.last().unwrap().name.clone());
    ok(json!({"items": page, "next_cursor": next}))
}

fn plus(seconds: u64) -> String {
    // CREATED_AT.
    let base = time::OffsetDateTime::from_unix_timestamp(1_795_082_400).unwrap();
    (base + time::Duration::seconds(seconds as i64))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

fn create(inner: &mut Inner, body: CreateDatabase) -> Reply {
    if !memoire_api::valid_name(&body.name) {
        return Err(invalid("/name", "invalid"));
    }
    let ttl = match (body.kind, body.idle_ttl_seconds) {
        (DatabaseKind::Persistent, Some(_)) => return Err(invalid("/idle_ttl_seconds", "invalid")),
        (DatabaseKind::Persistent, None) => None,
        (DatabaseKind::Burstable, ttl) => {
            let ttl = ttl.unwrap_or(86_400);
            if !(60..=604_800).contains(&ttl) {
                return Err(invalid("/idle_ttl_seconds", "out_of_range"));
            }
            Some(ttl)
        }
    };
    if inner
        .databases
        .get(&body.name)
        .is_some_and(|db| db.info.state == DatabaseState::Active)
    {
        return Err(fail(409, "database_exists"));
    }
    inner.serial += 1;
    let id = format!("db_{:032x}", inner.serial);
    let path = inner.dir.path().join(format!("{id}.sqlite"));
    Connection::open(&path)
        .unwrap()
        .execute_batch("PRAGMA journal_mode = DELETE")
        .unwrap();
    let info = Database {
        id,
        name: body.name.clone(),
        kind: body.kind,
        state: DatabaseState::Active,
        created_at: CREATED_AT.into(),
        last_used_at: None,
        size_bytes: Some(std::fs::metadata(&path).unwrap().len()),
        idle_ttl_seconds: ttl,
        expires_at: ttl.map(plus),
        expires_at_max: ttl.map(|_| plus(604_800)),
    };
    inner.databases.insert(
        body.name,
        Db {
            info: info.clone(),
            path,
            snapshots: Vec::new(),
        },
    );
    Ok(reply(201, &serde_json::to_value(info).unwrap()))
}

fn touch(db: &mut Db) {
    db.info.last_used_at = Some(USED_AT.into());
    db.info.size_bytes = Some(std::fs::metadata(&db.path).unwrap().len());
}

fn database(
    inner: &mut Inner,
    method: &str,
    name: &str,
    rest: &[&str],
    seen: &Seen,
    need: &dyn Fn(&str) -> Result<(), Response>,
) -> Reply {
    let expired = inner.databases[name].info.state == DatabaseState::Expired;
    let persistent = inner.databases[name].info.kind == DatabaseKind::Persistent;
    let gone = || {
        if expired {
            Err(fail(410, "database_expired"))
        } else {
            Ok(())
        }
    };
    match (method, rest) {
        ("GET", []) => {
            need(".read")?;
            ok(&inner.databases[name].info)
        }
        ("DELETE", []) => {
            need(".delete")?;
            let db = inner.databases.remove(name).unwrap();
            let _ = std::fs::remove_file(db.path);
            Ok(reply(204, &Value::Null))
        }
        ("POST", ["extend"]) => {
            need(".write")?;
            let body: ExtendDatabase = json_body(seen)?;
            if persistent {
                return Err(fail(409, "not_burstable"));
            }
            gone()?;
            let db = inner.databases.get_mut(name).unwrap();
            if let Some(ttl) = body.idle_ttl_seconds {
                if !(60..=604_800).contains(&ttl) {
                    return Err(invalid("/idle_ttl_seconds", "out_of_range"));
                }
                db.info.idle_ttl_seconds = Some(ttl);
            }
            touch(db);
            db.info.expires_at = Some(plus(1_800 + db.info.idle_ttl_seconds.unwrap()));
            ok(&db.info)
        }
        ("POST", ["query"]) => {
            let body: QueryRequest = if seen.content_type.as_deref() == Some("application/json") {
                serde_json::from_slice(&seen.body).map_err(|_| invalid("", "invalid"))?
            } else {
                return Err(fail(415, "unsupported_media_type"));
            };
            need(match body.mode {
                QueryMode::Read => ".read",
                QueryMode::Write => ".write",
            })?;
            gone()?;
            let db = inner.databases.get_mut(name).unwrap();
            let results = run_query(&db.path, &body).map_err(Failed::response)?;
            touch(db);
            ok(json!({"results": results}))
        }
        ("GET", ["schema"]) => {
            need(".read")?;
            gone()?;
            ok(schema(&inner.databases[name].path))
        }
        ("GET", ["export"]) => {
            need(".read")?;
            gone()?;
            let copy = inner.dir.path().join("export.sqlite");
            let _ = std::fs::remove_file(&copy);
            vacuum_into(&inner.databases[name].path, &copy);
            let bytes = std::fs::read(&copy).unwrap();
            Ok((
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "application/vnd.sqlite3".to_owned()),
                    (
                        header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{name}.sqlite\""),
                    ),
                ],
                bytes,
            )
                .into_response())
        }
        ("PUT", ["import"]) => {
            need(".write")?;
            if seen.content_type.as_deref() != Some("application/vnd.sqlite3") {
                return Err(fail(415, "unsupported_media_type"));
            }
            gone()?;
            let db = inner.databases.get_mut(name).unwrap();
            if !schema(&db.path).tables.is_empty() {
                return Err(fail(409, "database_not_empty"));
            }
            if !seen.body.starts_with(b"SQLite format 3\0") {
                return Err(fail(422, "invalid_import"));
            }
            std::fs::write(&db.path, &seen.body).unwrap();
            let check: String = Connection::open(&db.path)
                .unwrap()
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .unwrap();
            assert_eq!(check, "ok");
            touch(db);
            ok(&db.info)
        }
        ("GET", ["snapshots"]) => {
            need(".read")?;
            if !persistent {
                return Err(fail(409, "not_persistent"));
            }
            let items: Vec<&Snapshot> = inner.databases[name]
                .snapshots
                .iter()
                .map(|(s, _)| s)
                .collect();
            ok(json!({"items": items, "next_cursor": null}))
        }
        ("POST", ["snapshots", id, "restore"]) => {
            need(".snapshots")?;
            if !persistent {
                return Err(fail(409, "not_persistent"));
            }
            let Some(source) = inner.databases[name]
                .snapshots
                .iter()
                .find(|(s, _)| s.id == *id)
                .map(|(_, path)| path.clone())
            else {
                return Err(fail(404, "snapshot_not_found"));
            };
            take_snapshot(inner, name, SnapshotReason::PreRestore);
            let db = inner.databases.get_mut(name).unwrap();
            std::fs::copy(source, &db.path).unwrap();
            touch(db);
            ok(&db.info)
        }
        _ => Err(fail(404, "not_found")),
    }
}

fn value(v: ValueRef<'_>) -> SqlValue {
    match v {
        ValueRef::Null => SqlValue::Null,
        ValueRef::Integer(n) => SqlValue::Integer(n),
        ValueRef::Real(x) => SqlValue::Real(x),
        ValueRef::Text(t) => SqlValue::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => SqlValue::Blob(b.to_vec()),
    }
}

fn bindable(v: &SqlValue) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    match v {
        SqlValue::Null => V::Null,
        SqlValue::Integer(n) => V::Integer(*n),
        SqlValue::Real(x) => V::Real(*x),
        SqlValue::Text(s) => V::Text(s.clone()),
        SqlValue::Blob(b) => V::Blob(b.clone()),
    }
}

/// Why a batch failed: SQLite refused a statement, or the data plane
/// answered something else.
#[derive(Debug)]
pub enum Failed {
    Sql {
        statement: usize,
        code: &'static str,
        message: String,
    },
    Reply(Response),
}

impl memoire_memory::BackendError for Failed {
    fn sql(&self) -> Option<memoire_memory::SqlFailure<'_>> {
        match self {
            Self::Sql {
                statement,
                code,
                message,
            } => Some(memoire_memory::SqlFailure {
                statement: *statement,
                code,
                message,
            }),
            Self::Reply(_) => None,
        }
    }
}

impl From<Response> for Failed {
    fn from(response: Response) -> Self {
        Self::Reply(response)
    }
}

impl Failed {
    /// The response the query route answers.
    fn response(self) -> Response {
        match self {
            Self::Reply(response) => response,
            Self::Sql {
                statement,
                code,
                message,
            } => sql_reply(statement, code, message),
        }
    }
}

fn sql_error(statement: usize, error: &rusqlite::Error) -> Failed {
    let (code, message) = match error {
        rusqlite::Error::SqliteFailure(e, message) => (
            match e.code {
                rusqlite::ErrorCode::ConstraintViolation => "SQLITE_CONSTRAINT",
                _ => "SQLITE_ERROR",
            },
            message.clone().unwrap_or_else(|| e.to_string()),
        ),
        other => ("SQLITE_ERROR", other.to_string()),
    };
    Failed::Sql {
        statement,
        code,
        message,
    }
}

fn sql_reply(statement: usize, code: &str, message: String) -> Response {
    // Merezha's edge carries only code, classification and details; the
    // message travels percent-encoded in a header.
    let (detail, header) = memoire_api::SqlErrorDetail {
        statement,
        sqlite_code: code.into(),
        message,
    }
    .to_wire();
    let mut response = reply(
        422,
        &json!({"error": {"code": "sql_error", "classification": "permanent", "details": [detail]}}),
    );
    response
        .headers_mut()
        .insert(memoire_api::SQL_MESSAGE_HEADER, header.parse().unwrap());
    response
}

/// A statement SQLite refused to prepare: the authorizer's denial, or an
/// SQL error.
fn prepare_error(statement: usize, error: &rusqlite::Error) -> Failed {
    match error {
        rusqlite::Error::SqliteFailure(e, _)
            if e.code == rusqlite::ErrorCode::AuthorizationForStatementDenied =>
        {
            fail(403, "statement_denied").into()
        }
        other => sql_error(statement, other),
    }
}

/// The pragmas a client statement may use: none that touches a file or the
/// connection's safety settings.
const PRAGMAS: [&str; 34] = [
    "analysis_limit",
    "application_id",
    "case_sensitive_like",
    "cell_size_check",
    "collation_list",
    "compile_options",
    "data_version",
    "defer_foreign_keys",
    "encoding",
    "foreign_key_check",
    "foreign_key_list",
    "foreign_keys",
    "freelist_count",
    "function_list",
    "ignore_check_constraints",
    "index_info",
    "index_list",
    "index_xinfo",
    "integrity_check",
    "legacy_alter_table",
    "module_list",
    "optimize",
    "page_count",
    "page_size",
    "pragma_list",
    "quick_check",
    "recursive_triggers",
    "reverse_unordered_selects",
    "schema_version",
    "table_info",
    "table_list",
    "table_xinfo",
    "trusted_schema",
    "user_version",
];

/// The data plane's authorizer, active while a client statement is
/// prepared.
fn authorize(context: AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Attach { .. }
        | AuthAction::Detach { .. }
        | AuthAction::Transaction { .. }
        | AuthAction::Savepoint { .. } => Authorization::Deny,
        AuthAction::Function { function_name }
            if ["load_extension", "fts3_tokenizer"]
                .contains(&function_name.to_ascii_lowercase().as_str()) =>
        {
            Authorization::Deny
        }
        AuthAction::Pragma { pragma_name, .. }
            if !PRAGMAS.contains(&pragma_name.to_ascii_lowercase().as_str()) =>
        {
            Authorization::Deny
        }
        _ => Authorization::Allow,
    }
}

/// A connection configured as the data plane's: defensive, no trust in
/// the schema, and `query_only` on the read-only connection.
fn connect(path: &std::path::Path, mode: QueryMode) -> Connection {
    let conn = match mode {
        QueryMode::Read => Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY),
        QueryMode::Write => Connection::open(path),
    }
    .unwrap();
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
        .unwrap();
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)
        .unwrap();
    if mode == QueryMode::Read {
        conn.pragma_update(None, "query_only", 1).unwrap();
    }
    conn
}

/// Prepares a client statement under the authorizer.
fn prepare<'c>(conn: &'c Connection, sql: &str) -> rusqlite::Result<rusqlite::Statement<'c>> {
    conn.authorizer(Some(authorize))?;
    let prepared = conn.prepare(sql);
    conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
    prepared
}

fn run_query(path: &std::path::Path, body: &QueryRequest) -> Result<Vec<StatementResult>, Failed> {
    let conn = connect(path, body.mode);
    if !body.transaction {
        // Each statement commits on its own; SQLite undoes a failing one.
        return run_statements(&conn, body);
    }
    conn.execute_batch(match body.mode {
        QueryMode::Read => "BEGIN",
        QueryMode::Write => "BEGIN IMMEDIATE",
    })
    .unwrap();
    let results = run_statements(&conn, body);
    if results.is_ok() {
        conn.execute_batch("COMMIT").unwrap();
    } else if !conn.is_autocommit() {
        conn.execute_batch("ROLLBACK").unwrap();
    }
    results
}

fn run_statements(conn: &Connection, body: &QueryRequest) -> Result<Vec<StatementResult>, Failed> {
    let mut results = Vec::new();
    for (i, statement) in body.statements.iter().enumerate() {
        // The authorizer has no action for VACUUM, which can write a file.
        let words: Vec<String> = statement
            .sql
            .split_whitespace()
            .take(3)
            .map(str::to_ascii_uppercase)
            .collect();
        if words.first().is_some_and(|w| w == "VACUUM") && words.iter().any(|w| w == "INTO") {
            return Err(fail(403, "statement_denied").into());
        }
        let mut stmt = prepare(conn, &statement.sql).map_err(|e| prepare_error(i, &e))?;
        if body.mode == QueryMode::Read && !stmt.readonly() {
            return Err(fail(422, "read_only").into());
        }
        // The CLI binds positionally only.
        let Params::Positional(values) = &statement.params else {
            panic!("unexpected named parameters");
        };
        if values.len() != stmt.parameter_count() {
            return Err(invalid(&format!("/statements/{i}/params"), "count").into());
        }
        for (j, v) in values.iter().enumerate() {
            stmt.raw_bind_parameter(j + 1, bindable(v)).unwrap();
        }
        let columns: Vec<Column> = stmt
            .column_names()
            .into_iter()
            .map(|name| Column {
                name: name.to_owned(),
                decltype: None,
            })
            .collect();
        let readonly = stmt.readonly();
        let before = conn.last_insert_rowid();
        let mut rows = Vec::new();
        {
            let count = columns.len();
            let mut cursor = stmt.raw_query();
            while let Some(row) = cursor.next().map_err(|e| sql_error(i, &e))? {
                rows.push((0..count).map(|c| value(row.get_ref(c).unwrap())).collect());
            }
        }
        let after = conn.last_insert_rowid();
        results.push(StatementResult {
            rows_affected: if columns.is_empty() && !readonly {
                conn.changes()
            } else {
                0
            },
            last_insert_rowid: (after != before).then_some(SqlValue::Integer(after)),
            columns,
            rows,
        });
    }
    Ok(results)
}

fn schema(path: &std::path::Path) -> Schema {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let objects: Vec<(String, String, String)> = conn
        .prepare(
            "SELECT type, name, sql FROM sqlite_schema \
             WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let columns = |name: &str| -> Vec<SchemaColumn> {
        conn.prepare("SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_info(?1)")
            .unwrap()
            .query_map([name], |r| {
                Ok(SchemaColumn {
                    name: r.get(0)?,
                    decltype: r.get(1)?,
                    not_null: r.get(2)?,
                    default: r.get(3)?,
                    primary_key: r.get(4)?,
                    generated: false,
                })
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let mut schema = Schema {
        tables: Vec::new(),
        views: Vec::new(),
    };
    for (kind, name, sql) in objects {
        if kind == "table" {
            schema.tables.push(Table {
                columns: columns(&name),
                name,
                sql,
                without_rowid: false,
                indexes: Vec::new(),
            });
        } else {
            schema.views.push(View {
                columns: columns(&name),
                name,
                sql,
            });
        }
    }
    schema
}

/// The memory routes (docs/api.md, "Memory routes"): the engine, run here
/// on the database's file through the same batch runner as the query
/// route, behind the gateway's admission and wire shapes. Only transport:
/// every rule of the memory is the engine's.
mod memory {
    use super::{Failed, Inner, Seen, Shared, bearer, fail, invalid, reply, run_query, touch};
    use axum::{
        extract::Query,
        http::{Method, Uri},
        response::Response,
    };
    use memoire_api::memory::{
        ConfirmRequest, DisputeRequest, EventsResponse, ExportResponse, ForgetRequest, Initialized,
        Labels as WireLabels, Purged, RecallResponse, Recalled, RememberRequest, ResolveRequest,
        ReviseRequest, ShowResponse, Status, StoreInfo, SupersedeRequest, SupersedeResponse,
    };
    use memoire_api::{DatabaseState, QueryMode, QueryRequest, StatementResult};
    use memoire_memory::store::{After, Batch, Cursor};
    use memoire_memory::{
        Backend, Labels, Refusal, RefusalClass, history, model, retire, schema, trust, verbs,
    };
    use serde_json::{Value, json};
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    /// One database's store, written as the caller's principal.
    struct Plane<'a> {
        shared: &'a Shared,
        name: String,
        principal: String,
    }

    impl Backend for Plane<'_> {
        type Error = Failed;

        async fn run(&self, mode: QueryMode, batch: Batch) -> Result<Vec<StatementResult>, Failed> {
            if mode == QueryMode::Write {
                let hook = self.shared.lock().unwrap().before_write.take();
                if let Some(hook) = hook {
                    hook();
                }
            }
            let (statements, timeout_ms) = batch.into_parts();
            let mut inner = self.shared.lock().unwrap();
            let db = inner
                .databases
                .get_mut(&self.name)
                .ok_or_else(|| fail(404, "database_not_found"))?;
            let results = run_query(
                &db.path,
                &QueryRequest {
                    mode,
                    transaction: true,
                    timeout_ms,
                    statements,
                },
            )?;
            touch(db);
            Ok(results)
        }

        async fn principal(&self) -> Result<String, Failed> {
            Ok(self.principal.clone())
        }
    }

    /// The fake's backend never waits.
    fn ready<T>(future: impl Future<Output = T>) -> T {
        match pin!(future)
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("the fake's memory backend never waits"),
        }
    }

    type Reply = Result<Response, Response>;

    /// `200` with `value`.
    fn to(value: &impl serde::Serialize) -> Reply {
        Ok(reply(200, &serde_json::to_value(value).unwrap()))
    }

    /// A failed verb as the gateway answers it.
    fn failure(error: memoire_memory::Error<Failed>) -> Response {
        match error {
            memoire_memory::Error::Refused(refusal) => refused(&refusal),
            // The store's own SQL failed outside the guards: the query
            // route's `sql_error`, without SQLite's message.
            memoire_memory::Error::Backend(Failed::Sql {
                statement, code, ..
            }) => reply(
                422,
                &json!({"error": {"code": "sql_error", "classification": "permanent",
                                  "details": [{"path": format!("/statements/{statement}/sql"),
                                               "code": code.to_ascii_lowercase()}]}}),
            ),
            memoire_memory::Error::Backend(failed) => failed.response(),
            memoire_memory::Error::InvalidResponse(_) => fail(503, "internal"),
            memoire_memory::Error::NoRandomness(_) => fail(503, "random_unavailable"),
        }
    }

    /// A refusal: `validation_failed` with the field for a refused input,
    /// else its own code; its members in `data` beside `error`.
    fn refused(refusal: &Refusal) -> Response {
        let (code, data) = refusal.to_wire();
        let mut body = match refusal.detail() {
            Some((path, detail)) => {
                json!({"error": {"code": "validation_failed", "classification": "permanent",
                                 "details": [{"path": path, "code": detail}]}})
            }
            None => {
                let classification = if refusal.class() == RefusalClass::Conflict {
                    "conflict"
                } else {
                    "permanent"
                };
                json!({"error": {"code": code, "classification": classification}})
            }
        };
        // An input refusal carries `data` once the engine's wire form gives
        // it one under `validation_failed`.
        let input = refusal.detail().is_some();
        if !data.is_empty() && (!input || code == "validation_failed") {
            body["data"] = Value::Object(data);
        }
        let status = match refusal.detail() {
            Some(_) => 422,
            None => memoire_api::error::status_of(code).unwrap(),
        };
        reply(status, &body)
    }

    fn engine<T>(result: Result<T, memoire_memory::Error<Failed>>) -> Result<T, Response> {
        result.map_err(failure)
    }

    fn body<T: serde::de::DeserializeOwned>(seen: &Seen) -> Result<T, Response> {
        super::json_body(seen)
    }

    fn labels(wire: WireLabels) -> Labels {
        Labels {
            agent: wire.agent.unwrap_or_default(),
            session: wire.session.unwrap_or_default(),
        }
    }

    struct Params(Vec<(String, String)>);

    impl Params {
        fn all(&self, key: &str) -> Vec<String> {
            self.0
                .iter()
                .filter(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .collect()
        }

        fn last(&self, key: &str) -> Option<&str> {
            self.0
                .iter()
                .rev()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        }

        fn int(&self, key: &str, default: i64) -> Result<i64, Response> {
            self.last(key).map_or(Ok(default), |text| {
                text.parse()
                    .map_err(|_| invalid(&format!("/{key}"), "invalid"))
            })
        }

        fn flag(&self, key: &str) -> Result<bool, Response> {
            match self.last(key) {
                None | Some("false") => Ok(false),
                Some("true") => Ok(true),
                Some(_) => Err(invalid(&format!("/{key}"), "invalid")),
            }
        }

        fn scopes(&self) -> Option<Vec<String>> {
            Some(self.all("scope")).filter(|s| !s.is_empty())
        }
    }

    /// `[name, "memory", rest…]` of the path.
    pub fn route(
        shared: &Shared,
        method: &Method,
        uri: &Uri,
        path: &[String],
        seen: &Seen,
    ) -> Response {
        handle(shared, method, uri, path, seen).unwrap_or_else(|e| e)
    }

    const READERS: [&str; 2] = ["memoire.memory.read", "memoire.databases.read"];
    const WRITERS: [&str; 2] = ["memoire.memory.write", "memoire.databases.write"];

    /// Admits the caller as the gateway does, any of `allowed` at the
    /// team; its subject, and whether it may read.
    fn admit(inner: &Inner, seen: &Seen, allowed: [&str; 2]) -> Result<(String, bool), Response> {
        let key = bearer(seen)
            .and_then(|k| inner.keys.get(k))
            .ok_or_else(|| fail(401, "unauthenticated"))?;
        if key.team.is_none() {
            return Err(fail(403, "tenant_required"));
        }
        let roleless = key
            .machine
            .as_ref()
            .is_some_and(|m| !inner.with_role.contains(m));
        if roleless
            || !key
                .permissions
                .iter()
                .any(|p| allowed.contains(&p.as_str()))
        {
            return Err(fail(403, "unauthorized"));
        }
        let reads = key
            .permissions
            .iter()
            .any(|p| READERS.contains(&p.as_str()));
        Ok((key.subject.clone(), reads))
    }

    fn handle(shared: &Shared, method: &Method, uri: &Uri, path: &[String], seen: &Seen) -> Reply {
        let path: Vec<&str> = path.iter().map(String::as_str).collect();
        let [name, "memory", rest @ ..] = path.as_slice() else {
            return Err(fail(404, "not_found"));
        };
        let write = match (method.as_str(), rest) {
            ("GET", [] | ["memories"] | ["memories", _] | ["events"] | ["export"]) => false,
            ("POST", ["init"] | ["memories"] | ["memories", _, _]) => true,
            _ => return Err(fail(404, "not_found")),
        };
        let (principal, reads) = {
            let inner = shared.lock().unwrap();
            let admitted = admit(&inner, seen, if write { WRITERS } else { READERS })?;
            match inner.databases.get(*name) {
                None => return Err(fail(404, "database_not_found")),
                Some(db) if db.info.state == DatabaseState::Expired => {
                    return Err(fail(410, "database_expired"));
                }
                Some(_) => {}
            }
            admitted
        };
        let plane = Plane {
            shared,
            name: (*name).to_owned(),
            principal,
        };
        let params = Params(
            Query::<Vec<(String, String)>>::try_from_uri(uri)
                .map(|Query(pairs)| pairs)
                .map_err(|_| invalid("", "invalid"))?,
        );
        // The parameters each route knows; any other is refused.
        let known: &[&str] = match rest {
            ["memories"] => &["query", "scope", "kind", "tag", "status", "limit"],
            ["memories", _] => &["history"],
            ["events"] => &[
                "scope",
                "since_seconds",
                "limit",
                "after",
                "store_id",
                "first_look",
            ],
            ["export"] => &["scope", "after", "limit"],
            _ => &[],
        };
        if let Some((name, _)) = params.0.iter().find(|(k, _)| !known.contains(&k.as_str())) {
            return Err(invalid(&format!("/{name}"), "unknown_field"));
        }
        let id =
            |text: &str| model::parse_memory_id(text).map_err(|_| fail(404, "memory_not_found"));
        match (method.as_str(), rest) {
            ("GET", []) => {
                let store_id = engine(ready(schema::store_id(&plane)))?;
                to(&StoreInfo {
                    schema_version: schema::SCHEMA_VERSION,
                    supported_version: schema::SCHEMA_VERSION,
                    store_id,
                })
            }
            ("POST", ["init"]) => {
                if !seen.body.is_empty() {
                    let _: serde_json::Map<String, Value> = body(seen)?;
                }
                let migrated_from = engine(ready(schema::migrate(&plane)))?;
                let store_id = engine(ready(schema::store_id(&plane)))?;
                to(&Initialized {
                    schema_version: schema::SCHEMA_VERSION,
                    migrated_from,
                    store_id,
                })
            }
            ("POST", ["memories"]) => {
                let input: RememberRequest = body(seen)?;
                let remembered = ready(verbs::remember(
                    &plane,
                    &labels(input.labels),
                    verbs::Remember {
                        title: input.title,
                        body: input.body.unwrap_or_default(),
                        kind: input.kind,
                        scope: input.scope,
                        tags: input.tags,
                        evidence: input.evidence,
                        force: input.force,
                        supersedes: input.supersedes,
                        reason: input.reason,
                    },
                    || None,
                ));
                // A caller that may not read is not shown what it found.
                let memory =
                    engine(remembered.map_err(|e| {
                        match e {
                            memoire_memory::Error::Refused(Refusal::Duplicate {
                                scope, ..
                            }) if !reads => Refusal::Duplicate {
                                scope,
                                candidates: Vec::new(),
                            }
                            .into(),
                            e => e,
                        }
                    }))?;
                Ok(reply(201, &serde_json::to_value(memory).unwrap()))
            }
            ("GET", ["memories"]) => {
                let mut statuses = Vec::new();
                for (i, text) in params.all("status").iter().enumerate() {
                    statuses.push(
                        Status::parse(text)
                            .ok_or_else(|| invalid(&format!("/status/{i}"), "invalid"))?,
                    );
                }
                let scopes = params.scopes();
                let found = engine(ready(verbs::recall(
                    &plane,
                    verbs::Recall {
                        query: params.last("query"),
                        scopes: scopes.as_deref(),
                        statuses: &statuses,
                        kinds: &params.all("kind"),
                        tags: &params.all("tag"),
                        limit: params.int("limit", 10)?,
                    },
                )))?;
                to(&RecallResponse {
                    memories: found
                        .into_iter()
                        .map(|(memory, score)| Recalled { memory, score })
                        .collect(),
                })
            }
            ("GET", ["memories", text]) => {
                let history = params.flag("history")?;
                let shown = engine(ready(verbs::show(&plane, &id(text)?, history)))?;
                to(&ShowResponse {
                    memory: shown.memory,
                    confirmations: shown.confirmations,
                    disputes: shown.disputes,
                    revisions: shown.revisions,
                })
            }
            ("POST", ["memories", text, "revise"]) => {
                let input: ReviseRequest = body(seen)?;
                let memory = engine(ready(verbs::revise(
                    &plane,
                    &labels(input.labels),
                    verbs::Revise {
                        id: id(text)?,
                        title: input.title,
                        body: input.body,
                        kind: input.kind,
                        scope: input.scope,
                        tags: input.tags,
                        add_tags: input.add_tags,
                        evidence: input.evidence,
                        add_evidence: input.add_evidence,
                        reason: input.reason,
                    },
                )))?;
                to(&memory)
            }
            ("POST", ["memories", text, "confirm"]) => {
                let input: ConfirmRequest = body(seen)?;
                let memory = engine(ready(trust::confirm(
                    &plane,
                    &labels(input.labels),
                    &id(text)?,
                    input.evidence,
                    input.rev,
                )))?;
                to(&memory)
            }
            ("POST", ["memories", text, "dispute"]) => {
                let input: DisputeRequest = body(seen)?;
                let memory = engine(ready(trust::dispute(
                    &plane,
                    &labels(input.labels),
                    &id(text)?,
                    &input.reason,
                    input.evidence,
                    input.rev,
                )))?;
                to(&memory)
            }
            ("POST", ["memories", text, "resolve"]) => {
                let input: ResolveRequest = body(seen)?;
                let memory = engine(ready(trust::resolve(
                    &plane,
                    &labels(input.labels),
                    &id(text)?,
                    &input.reason,
                )))?;
                to(&memory)
            }
            ("POST", ["memories", text, "supersede"]) => {
                let input: SupersedeRequest = body(seen)?;
                let (superseded, by) = engine(ready(retire::supersede(
                    &plane,
                    &labels(input.labels),
                    &id(text)?,
                    &input.by,
                    &input.reason,
                )))?;
                to(&SupersedeResponse { superseded, by })
            }
            ("POST", ["memories", text, "forget"]) => {
                let input: ForgetRequest = body(seen)?;
                let (id, labels) = (id(text)?, labels(input.labels));
                if input.purge {
                    engine(ready(retire::purge(&plane, &labels, &id, &input.reason)))?;
                    return to(&Purged { id, purged: true });
                }
                to(&engine(ready(retire::forget(
                    &plane,
                    &labels,
                    &id,
                    &input.reason,
                )))?)
            }
            ("GET", ["events"]) => {
                let since_seconds = params
                    .last("since_seconds")
                    .map(|t| {
                        t.parse::<u64>()
                            .map_err(|_| invalid("/since_seconds", "invalid"))
                    })
                    .transpose()?;
                let after = params
                    .last("after")
                    .map(|t| t.parse::<i64>().map_err(|_| invalid("/after", "invalid")))
                    .transpose()?;
                let store_id = params.last("store_id").map(str::to_owned);
                let first_look = params.flag("first_look")?;
                let cursor = match (after, store_id) {
                    (Some(seq), Some(store_id)) => Some(Cursor { store_id, seq }),
                    (Some(_), None) => return Err(invalid("/store_id", "required")),
                    (None, Some(_)) => return Err(invalid("/after", "required")),
                    (None, None) => None,
                };
                if first_look && cursor.is_some() {
                    return Err(invalid("/first_look", "not_allowed"));
                }
                let scopes = params.scopes();
                let page = engine(ready(history::events(
                    &plane,
                    history::Events {
                        scopes: scopes.as_deref(),
                        since_seconds,
                        after: match (&cursor, first_look) {
                            (Some(cursor), _) => After::Cursor(cursor),
                            (None, true) => After::FirstLook,
                            (None, false) => After::Start,
                        },
                        limit: params.int("limit", 50)?,
                    },
                )))?;
                to(&EventsResponse {
                    events: page.events,
                    store_id: page.store_id,
                    after: page.after,
                    top: page.top,
                    matching: page.matching,
                })
            }
            ("GET", ["export"]) => {
                let scopes = params.scopes();
                let page = engine(ready(history::export(
                    &plane,
                    scopes.as_deref(),
                    params.int("after", 0)?,
                    params.int("limit", history::EXPORT_PAGE)?,
                )))?;
                to(&ExportResponse {
                    memories: page.memories,
                    next_after: page.next,
                })
            }
            _ => Err(fail(404, "not_found")),
        }
    }
}
