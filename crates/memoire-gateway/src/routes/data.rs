//! The data plane's routes: `POST …/{name}/query`, `GET …/{name}/schema`
//! and `GET …/{name}/tables/{table}/rows`.
use super::{Entry, Routes, entry, permission};
use crate::{
    data::{exec::Refused, request},
    http::{TeamCaller, admit, invalid, refuse},
};
use axum::{
    Json,
    body::Body,
    extract::{Path, Query, Request, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use memoire_api::{
    DatabaseState, QueryMode, QueryResponse, SQL_MESSAGE_HEADER, SqlErrorDetail,
    limits::{DEFAULT_ROWS_PAGE, MAX_REQUEST_BYTES, MAX_ROWS_PAGE},
    valid_name,
};
use merezha_domain::Failure;
use merezha_platform::{ErrorReply, FieldError};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

/// `POST …/{name}/query`.
pub async fn query(
    State(routes): State<Routes>,
    Path(name): Path<String>,
    request: Request,
) -> Response {
    let (parts, body) = request.into_parts();
    let declared = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > MAX_REQUEST_BYTES) {
        return refuse(Failure::permanent("body_limit"));
    }
    let Ok(bytes) = axum::body::to_bytes(body, MAX_REQUEST_BYTES).await else {
        return refuse(Failure::permanent("body_limit"));
    };
    let request = Request::from_parts(parts, Body::empty());
    // The mode decides the permission, so it is read before the caller is
    // admitted; the body's problems are reported after.
    let needed = match request::mode_of(&bytes) {
        QueryMode::Read => permission::READ,
        QueryMode::Write => permission::WRITE,
    };
    let caller = match admit(&request, &routes.policy, needed) {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    let json = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
    if !json {
        return refuse(Failure::permanent("unsupported_media_type"));
    }
    let batch = match request::parse(&bytes) {
        Ok(batch) => batch,
        Err(details) => return invalid(details),
    };
    let entry = match active(&routes, &caller, &name).await {
        Ok(entry) => entry,
        Err(refusal) => return refusal,
    };
    let team = caller.team.as_str();
    // Any query is a use, whatever it answers; recorded before it runs, so
    // a sweep deciding meanwhile sees it.
    routes.last_used.touch(team, &entry.id, now_unix_ms());
    let outcome = routes.data.query(&entry.target(team), batch).await;
    match outcome {
        Ok(results) => Json(QueryResponse { results }).into_response(),
        Err(refused) => refusal(refused),
    }
}

/// `GET …/{name}/schema`.
pub async fn schema(
    State(routes): State<Routes>,
    Path(name): Path<String>,
    request: Request,
) -> Response {
    let caller = match admit(&request, &routes.policy, permission::READ) {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    let entry = match active(&routes, &caller, &name).await {
        Ok(entry) => entry,
        Err(refusal) => return refusal,
    };
    match routes
        .data
        .schema(&entry.target(caller.team.as_str()))
        .await
    {
        Ok(schema) => Json(schema).into_response(),
        Err(refused) => refusal(refused),
    }
}

#[derive(serde::Deserialize)]
pub struct RowsQuery {
    limit: Option<String>,
    after: Option<String>,
}

/// `GET …/{name}/tables/{table}/rows?limit&after`.
pub async fn rows(
    State(routes): State<Routes>,
    Path((name, table)): Path<(String, String)>,
    Query(params): Query<RowsQuery>,
    request: Request,
) -> Response {
    let caller = match admit(&request, &routes.policy, permission::READ) {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    let limit = match params.limit.as_deref().map(str::parse::<usize>) {
        None => DEFAULT_ROWS_PAGE,
        Some(Ok(n)) if (1..=MAX_ROWS_PAGE).contains(&n) => n,
        Some(_) => return invalid(vec![("/limit".into(), "out_of_range")]),
    };
    let entry = match active(&routes, &caller, &name).await {
        Ok(entry) => entry,
        Err(refusal) => return refusal,
    };
    match routes
        .data
        .rows(
            &entry.target(caller.team.as_str()),
            table,
            limit,
            params.after,
        )
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(refused) => refusal(refused),
    }
}

/// The database `name` when it is active: `404 database_not_found`
/// otherwise, `410 database_expired` once it expired. The caller was
/// admitted to the route, so the lookup checks no second permission.
pub(super) async fn active(
    routes: &Routes,
    caller: &TeamCaller,
    name: &str,
) -> Result<Entry, Response> {
    if !valid_name(name) {
        return Err(refuse(Failure::permanent("database_not_found")));
    }
    let found = routes
        .catalog
        .query(
            caller.admitted(),
            "GetDatabase",
            json!({"name": name}),
            None,
        )
        .await
        .map_err(refuse)?;
    let entry = entry(found)?;
    if entry.state == DatabaseState::Expired {
        return Err(refuse(Failure::permanent("database_expired")));
    }
    Ok(entry)
}

/// The response for a refused batch or page.
pub(crate) fn refusal(refused: Refused) -> Response {
    match refused {
        Refused::Invalid { path, code } => invalid(vec![(path, code)]),
        Refused::Code(code) => refuse(match code {
            "sqlite_busy" | "database_unavailable" | "internal" => Failure::transient(code),
            _ => Failure::permanent(code),
        }),
        Refused::Sql {
            statement,
            sqlite_code,
            message,
        } => {
            let (entry, header) = SqlErrorDetail {
                statement,
                sqlite_code,
                message,
            }
            .to_wire();
            let details = FieldError::new(entry.path, &entry.code)
                .map(|detail| vec![detail])
                .unwrap_or_default();
            let mut response = ErrorReply::new(Failure::permanent("sql_error"))
                .details(details)
                .into_response();
            if let Ok(value) = HeaderValue::from_str(&header) {
                response.headers_mut().insert(SQL_MESSAGE_HEADER, value);
            }
            response
        }
    }
}

pub(super) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
