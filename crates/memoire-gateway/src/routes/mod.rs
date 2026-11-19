//! HTTP routes under `/v1/memoire` (docs/api.md).
// A refusal is the rendered error response, built once on a request's
// failure path; boxing it would buy nothing.
#![allow(clippy::result_large_err)]
mod data;

use crate::{
    catalog::{Catalog, random_hex},
    config::Settings,
    data::DataPlane,
    http::{
        Fields, TeamCaller, admit, invalid, json_object, no_content, no_store, principal, refuse,
        timestamp,
    },
    last_used::LastUse,
};
use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{StatusCode, Uri},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use memoire_api::{
    Database, DatabaseKind, DatabaseState, Page, PrincipalKind, Whoami,
    limits::{DEFAULT_PAGE, MAX_LIFETIME_SECONDS, MAX_PAGE},
    valid_name,
};
use merezha_domain::Failure;
use merezha_platform::ports::{Authorizer, PrincipalKind as MerezhaKind};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// memoire's permissions (docs/api.md, "Authentication and the team").
pub mod permission {
    pub const READ: &str = "memoire.databases.read";
    pub const WRITE: &str = "memoire.databases.write";
    pub const CREATE: &str = "memoire.databases.create";
    pub const DELETE: &str = "memoire.databases.delete";
}

#[derive(Clone)]
pub struct Routes {
    pub catalog: Catalog,
    pub data: Arc<DataPlane>,
    pub last_used: Arc<LastUse>,
    pub database: merezha_platform::Database,
    pub policy: Arc<dyn Authorizer>,
    pub settings: Arc<Settings>,
}

pub fn router(routes: Routes) -> Router {
    Router::new()
        .route("/whoami", get(whoami))
        .route("/databases", get(list).post(create))
        .route("/databases/{name}", get(detail).delete(remove))
        .route("/databases/{name}/extend", post(extend))
        .route("/databases/{name}/query", post(data::query))
        .route("/databases/{name}/schema", get(data::schema))
        .route("/databases/{name}/tables/{table}/rows", get(data::rows))
        .with_state(routes)
        .layer(middleware::map_response(no_store))
}

/// `GET /v1/memoire/whoami`: the caller and its active team.
async fn whoami(request: Request) -> Response {
    let principal = match principal(&request) {
        Ok(principal) => principal,
        Err(refusal) => return refusal,
    };
    let kind = match principal.kind {
        MerezhaKind::User => PrincipalKind::User,
        MerezhaKind::ApiKey => PrincipalKind::ApiKey,
        MerezhaKind::System => return refuse(Failure::permanent("unauthorized")),
    };
    Json(Whoami {
        subject: principal.subject.as_str().to_owned(),
        kind,
        team_id: principal.tenant.as_ref().map(|t| t.as_str().to_owned()),
    })
    .into_response()
}

/// One entry of the `databases` projection (the guest's `Entry`).
#[derive(Debug, Deserialize)]
pub(crate) struct Entry {
    id: String,
    name: String,
    kind: DatabaseKind,
    state: DatabaseState,
    created_unix_ms: u64,
    idle_ttl_seconds: Option<u64>,
    extended_unix_ms: Option<u64>,
}

impl Entry {
    /// The contract's database object. `last_used` is the data plane's
    /// last query or import, `size` the file's size.
    fn database(&self, last_used: Option<u64>, size: Option<u64>) -> Database {
        let last_used = self.extended_unix_ms.max(last_used);
        let burstable = self.kind == DatabaseKind::Burstable;
        let max = self.created_unix_ms + MAX_LIFETIME_SECONDS * 1000;
        let expires = self.idle_ttl_seconds.filter(|_| burstable).map(|ttl| {
            last_used
                .unwrap_or(self.created_unix_ms)
                .max(self.created_unix_ms)
                .saturating_add(ttl * 1000)
                .min(max)
        });
        Database {
            id: self.id.clone(),
            name: self.name.clone(),
            kind: self.kind,
            state: self.state,
            created_at: timestamp(self.created_unix_ms),
            last_used_at: last_used.map(timestamp),
            size_bytes: size.filter(|_| self.state == DatabaseState::Active),
            idle_ttl_seconds: self.idle_ttl_seconds.filter(|_| burstable),
            expires_at: expires.map(timestamp),
            expires_at_max: burstable.then(|| timestamp(max)),
        }
    }

    /// The data plane's target for this entry.
    pub(crate) fn target(&self, team: &str) -> crate::data::Target {
        crate::data::Target {
            team: team.to_owned(),
            id: self.id.clone(),
            burstable: self.kind == DatabaseKind::Burstable,
        }
    }
}

pub(crate) fn entry(value: Value) -> Result<Entry, Response> {
    serde_json::from_value(value).map_err(|_| refuse(Failure::transient("internal")))
}

/// `GET /v1/memoire/databases?state&cursor&limit`.
async fn list(State(routes): State<Routes>, request: Request) -> Response {
    let caller = match admit(&request, &routes.policy, permission::READ) {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    let (all, after, limit) = match list_params(request.uri()) {
        Ok(params) => params,
        Err(refusal) => return refusal,
    };
    let answer = routes
        .catalog
        .query(caller.edge(), "ListDatabases", json!({}), None)
        .await;
    let value = match answer {
        Ok(value) => value,
        Err(failure) => return refuse(failure),
    };
    let entries: Vec<Entry> = match serde_json::from_value(value["databases"].clone()) {
        Ok(entries) => entries,
        Err(_) => return refuse(Failure::transient("internal")),
    };
    let team = caller.team.as_str();
    let uses = match routes.last_used.team(&routes.database, team).await {
        Ok(uses) => uses,
        Err(failure) => return refuse(failure),
    };
    let mut items: Vec<Database> = entries
        .iter()
        .filter(|e| all || e.state == DatabaseState::Active)
        .filter(|e| after.as_deref().is_none_or(|after| e.name.as_str() > after))
        .take(limit + 1)
        .map(|e| e.database(uses.get(&e.id).copied(), routes.data.size(team, &e.id)))
        .collect();
    let next_cursor = (items.len() > limit).then(|| {
        items.truncate(limit);
        cursor(all, &items[limit - 1].name)
    });
    Json(Page { items, next_cursor }).into_response()
}

/// `state`, `cursor` and `limit`, percent-decoded; other parameters are
/// ignored.
fn list_params(uri: &Uri) -> Result<(bool, Option<String>, usize), Response> {
    let mut all = false;
    let mut cursor_text = None;
    let mut limit = DEFAULT_PAGE;
    let mut details = Vec::new();
    let Ok(Query(pairs)) = Query::<Vec<(String, String)>>::try_from_uri(uri) else {
        return Err(invalid(vec![(String::new(), "invalid")]));
    };
    for (key, value) in &pairs {
        let (key, value) = (key.as_str(), value.as_str());
        match key {
            "state" => match value {
                "active" => all = false,
                "all" => all = true,
                _ => details.push(("/state".to_owned(), "invalid")),
            },
            "cursor" => cursor_text = Some(value.to_owned()),
            "limit" => match value.parse::<usize>() {
                Ok(n) if (1..=MAX_PAGE).contains(&n) => limit = n,
                _ => details.push(("/limit".to_owned(), "out_of_range")),
            },
            _ => {}
        }
    }
    if !details.is_empty() {
        return Err(invalid(details));
    }
    let after = match cursor_text {
        None => None,
        Some(text) => {
            Some(uncursor(all, &text).ok_or_else(|| refuse(Failure::permanent("invalid_page")))?)
        }
    };
    Ok((all, after, limit))
}

/// An opaque cursor: the state filter and the last name of the page.
fn cursor(all: bool, last: &str) -> String {
    let filter = if all { "all" } else { "active" };
    URL_SAFE_NO_PAD.encode(format!("{filter}:{last}"))
}

/// The last name of the previous page, when `text` is a cursor of the same
/// filter.
fn uncursor(all: bool, text: &str) -> Option<String> {
    let bytes = URL_SAFE_NO_PAD.decode(text).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let (filter, last) = text.split_once(':')?;
    (filter == if all { "all" } else { "active" } && valid_name(last)).then(|| last.to_owned())
}

/// `POST /v1/memoire/databases`.
async fn create(State(routes): State<Routes>, request: Request) -> Response {
    let caller = match admit(&request, &routes.policy, permission::CREATE) {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    let body = match json_object(request, false).await {
        Ok(body) => body,
        Err(refusal) => return refusal,
    };
    let mut fields = Fields::new(body, &["name", "kind", "idle_ttl_seconds"]);
    let name = match fields.required("name").map(|v| v.as_str()) {
        Some(Some(name)) if valid_name(name) => Some(name.to_owned()),
        Some(_) => {
            fields.fail("name", "invalid");
            None
        }
        None => None,
    };
    let kind = match fields.required("kind").map(|v| v.as_str()) {
        Some(Some("burstable")) => Some(DatabaseKind::Burstable),
        Some(Some("persistent")) => Some(DatabaseKind::Persistent),
        Some(_) => {
            fields.fail("kind", "invalid");
            None
        }
        None => None,
    };
    let ttl = match fields.optional("idle_ttl_seconds").map(Value::as_u64) {
        None => None,
        Some(_) if kind == Some(DatabaseKind::Persistent) => {
            fields.fail("idle_ttl_seconds", "not_allowed");
            None
        }
        Some(Some(ttl)) if routes.settings.idle_ttl_in_range(ttl) => Some(ttl),
        Some(_) => {
            fields.fail("idle_ttl_seconds", "out_of_range");
            None
        }
    };
    if let Err(refusal) = fields.finish() {
        return refusal;
    }
    let (Some(name), Some(kind)) = (name, kind) else {
        return refuse(Failure::permanent("validation_failed"));
    };
    let id = match random_hex() {
        Ok(hex) => format!("db_{hex}"),
        Err(failure) => return refuse(failure),
    };
    let mut payload = json!({"id": id, "name": name, "kind": kind});
    if kind == DatabaseKind::Burstable {
        payload["idle_ttl_seconds"] =
            json!(ttl.unwrap_or(routes.settings.default_idle_ttl_seconds));
    }
    let receipt = match routes
        .catalog
        .command(caller.edge(), "CreateDatabase", payload)
        .await
    {
        Ok(receipt) => receipt,
        Err(failure) => return refuse(failure),
    };
    // The catalog accepted it: its empty file follows.
    if let Err(failure) = routes.data.create(caller.team.as_str(), &id).await {
        return refuse(failure);
    }
    respond(&routes, &caller, &name, Some(&receipt), StatusCode::CREATED).await
}

/// `GET /v1/memoire/databases/{name}`.
async fn detail(
    State(routes): State<Routes>,
    Path(name): Path<String>,
    request: Request,
) -> Response {
    let caller = match admit(&request, &routes.policy, permission::READ) {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    if !valid_name(&name) {
        return refuse(Failure::permanent("database_not_found"));
    }
    respond(&routes, &caller, &name, None, StatusCode::OK).await
}

/// `DELETE /v1/memoire/databases/{name}`.
async fn remove(
    State(routes): State<Routes>,
    Path(name): Path<String>,
    request: Request,
) -> Response {
    let caller = match admit(&request, &routes.policy, permission::DELETE) {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    if !valid_name(&name) {
        return refuse(Failure::permanent("database_not_found"));
    }
    // The entry the delete removes, pinned by id: a database created again
    // under the name in between is not touched.
    let id = match routes
        .catalog
        .query(
            caller.admitted(),
            "GetDatabase",
            json!({"name": name}),
            None,
        )
        .await
        .map(entry)
    {
        Ok(Ok(entry)) => entry.id,
        Ok(Err(refusal)) => return refusal,
        Err(failure) => return refuse(failure),
    };
    let receipt = match routes
        .catalog
        .command(
            caller.edge(),
            "DeleteDatabase",
            json!({"name": name, "id": id}),
        )
        .await
    {
        Ok(receipt) => receipt,
        Err(failure) => return refuse(failure),
    };
    // Answer once reads see the delete, as every route reads its writes.
    if let Err(failure) = routes
        .catalog
        .query(
            caller.admitted(),
            "ListDatabases",
            json!({}),
            Some(&receipt),
        )
        .await
    {
        return refuse(failure);
    }
    // The catalog deleted it: its file and last use follow.
    let team = caller.team.as_str();
    if let Err(failure) = routes.data.remove(team, &id).await {
        return refuse(failure);
    }
    let _ = routes.last_used.forget(&routes.database, team, &id).await;
    no_content()
}

/// `POST /v1/memoire/databases/{name}/extend`.
async fn extend(
    State(routes): State<Routes>,
    Path(name): Path<String>,
    request: Request,
) -> Response {
    let caller = match admit(&request, &routes.policy, permission::WRITE) {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    if !valid_name(&name) {
        return refuse(Failure::permanent("database_not_found"));
    }
    let body = match json_object(request, true).await {
        Ok(body) => body,
        Err(refusal) => return refusal,
    };
    let mut fields = Fields::new(body, &["idle_ttl_seconds"]);
    let ttl = match fields.optional("idle_ttl_seconds").map(Value::as_u64) {
        None => None,
        Some(Some(ttl)) if routes.settings.idle_ttl_in_range(ttl) => Some(ttl),
        Some(_) => {
            fields.fail("idle_ttl_seconds", "out_of_range");
            None
        }
    };
    if let Err(refusal) = fields.finish() {
        return refusal;
    }
    let mut payload = json!({"name": name});
    if let Some(ttl) = ttl {
        payload["idle_ttl_seconds"] = json!(ttl);
    }
    let receipt = match routes
        .catalog
        .command(caller.edge(), "ExtendDatabase", payload)
        .await
    {
        Ok(receipt) => receipt,
        Err(failure) => return refuse(failure),
    };
    respond(&routes, &caller, &name, Some(&receipt), StatusCode::OK).await
}

/// The database `name` as the projection holds it, once it reached
/// `after` when given.
pub(crate) async fn respond(
    routes: &Routes,
    caller: &TeamCaller,
    name: &str,
    after: Option<&merezha_domain::Receipt>,
    status: StatusCode,
) -> Response {
    let payload = json!({"name": name});
    let answer = match after {
        // The answer to a change: the caller was admitted to make it.
        Some(receipt) => {
            routes
                .catalog
                .query(caller.admitted(), "GetDatabase", payload, Some(receipt))
                .await
        }
        None => {
            routes
                .catalog
                .query(caller.edge(), "GetDatabase", payload, None)
                .await
        }
    };
    match answer.map(entry) {
        Ok(Ok(entry)) => {
            let team = caller.team.as_str();
            let used = match routes.last_used.team(&routes.database, team).await {
                Ok(uses) => uses.get(&entry.id).copied(),
                Err(failure) => return refuse(failure),
            };
            let size = routes.data.size(team, &entry.id);
            (status, Json(entry.database(used, size))).into_response()
        }
        Ok(Err(refusal)) => refusal,
        Err(failure) => refuse(failure),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_for_their_own_filter_only() {
        let active = cursor(false, "scratch");
        assert_eq!(uncursor(false, &active).as_deref(), Some("scratch"));
        assert_eq!(uncursor(true, &active), None);
        assert_eq!(uncursor(false, "not-a-cursor!"), None);
        assert_eq!(uncursor(false, &URL_SAFE_NO_PAD.encode("active:Bad")), None);
    }

    #[test]
    fn list_parameters_are_checked() {
        let params = |query: &str| list_params(&format!("/databases?{query}").parse().unwrap());
        assert_eq!(params("").ok(), Some((false, None, DEFAULT_PAGE)));
        assert_eq!(params("state=all&limit=3").ok(), Some((true, None, 3)));
        assert_eq!(params("state=%61ll").ok(), Some((true, None, DEFAULT_PAGE)));
        for bad in ["state=deleted", "limit=0", "limit=101", "limit=x"] {
            assert_eq!(params(bad).unwrap_err().status(), 422, "{bad}");
        }
        assert_eq!(params("cursor=zzz").unwrap_err().status(), 422);
    }

    #[test]
    fn burstable_expiry_is_capped_at_the_lifetime() {
        let entry = Entry {
            id: "db_0".into(),
            name: "scratch".into(),
            kind: DatabaseKind::Burstable,
            state: DatabaseState::Active,
            created_unix_ms: 1_795_000_000_000,
            idle_ttl_seconds: Some(604_800),
            extended_unix_ms: Some(1_795_000_060_000),
        };
        let db = entry.database(None, Some(4096));
        assert_eq!(db.expires_at, db.expires_at_max);
        assert_eq!(db.size_bytes, Some(4096));
        assert_eq!(db.last_used_at.as_deref(), Some("2026-11-18T11:07:40Z"));
        let persistent = Entry {
            kind: DatabaseKind::Persistent,
            idle_ttl_seconds: None,
            extended_unix_ms: None,
            ..entry
        };
        let db = persistent.database(Some(1_795_000_090_000), None);
        assert_eq!(db.last_used_at.as_deref(), Some("2026-11-18T11:08:10Z"));
        assert_eq!(
            (db.idle_ttl_seconds, db.expires_at, db.expires_at_max),
            (None, None, None)
        );
    }
}
