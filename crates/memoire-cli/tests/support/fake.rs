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
use rusqlite::{Connection, OpenFlags, types::ValueRef};
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
}

#[derive(Clone)]
pub struct Fake {
    pub url: String,
    inner: Arc<Mutex<Inner>>,
}

impl Fake {
    pub fn start() -> Self {
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
        self.inner.lock().unwrap().keys.insert(
            key.into(),
            Key {
                subject: format!("key_{}", key.len()),
                team: team.map(str::to_owned),
                permissions: permissions.iter().map(|p| (*p).to_owned()).collect(),
                machine: None,
            },
        );
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
    State(inner): State<Shared>,
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
    let mut inner = inner.lock().unwrap();
    inner.requests.push(seen.clone());
    if let Some((status, body, retry)) = inner.fail_next.take() {
        let mut response = reply(status, &body);
        if let Some(retry) = retry {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, retry.to_string().parse().unwrap());
        }
        return response;
    }
    let segments: Vec<String> = uri
        .path()
        .trim_start_matches('/')
        .split('/')
        .map(percent_decode)
        .collect();
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
                    if permissions.iter().any(|p| !ALL.contains(&p.as_str())) {
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
        if permissions.iter().any(|p| p.ends_with(permission)) {
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
            let results = run_query(&db.path, &body)?;
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

fn sql_error(statement: usize, error: &rusqlite::Error) -> Response {
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
    // Merezha's edge carries only code, classification and details; the
    // message travels percent-encoded in a header.
    let mut response = reply(
        422,
        &json!({"error": {"code": "sql_error", "classification": "permanent",
                          "details": [{"path": format!("/statements/{statement}/sql"),
                                       "code": code.to_ascii_lowercase()}]}}),
    );
    let encoded: String = message
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    response
        .headers_mut()
        .insert("memoire-sql-message", encoded.parse().unwrap());
    response
}

fn run_query(
    path: &std::path::Path,
    body: &QueryRequest,
) -> Result<Vec<StatementResult>, Response> {
    let conn = match body.mode {
        QueryMode::Read => Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY),
        QueryMode::Write => Connection::open(path),
    }
    .unwrap();
    let mut results = Vec::new();
    for (i, statement) in body.statements.iter().enumerate() {
        let first = statement
            .sql
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if [
            "BEGIN",
            "COMMIT",
            "END",
            "ROLLBACK",
            "SAVEPOINT",
            "RELEASE",
            "ATTACH",
            "DETACH",
        ]
        .contains(&first.as_str())
        {
            return Err(fail(403, "statement_denied"));
        }
        let mut stmt = conn.prepare(&statement.sql).map_err(|e| sql_error(i, &e))?;
        if body.mode == QueryMode::Read && !stmt.readonly() {
            return Err(fail(422, "read_only"));
        }
        // The CLI binds positionally only.
        let Params::Positional(values) = &statement.params else {
            panic!("unexpected named parameters");
        };
        if values.len() != stmt.parameter_count() {
            return Err(invalid(&format!("/statements/{i}/params"), "count"));
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
