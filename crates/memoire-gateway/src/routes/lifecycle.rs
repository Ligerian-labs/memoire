//! Snapshots, export and import: `GET …/{name}/snapshots`,
//! `POST …/{name}/snapshots/{id}/restore`, `GET …/{name}/export` and
//! `PUT …/{name}/import`.
use super::{
    Routes,
    data::{active, now_unix_ms, refusal},
    permission, respond,
};
use crate::{
    data::exec::Refused,
    http::{admit, refuse, timestamp},
    store,
};
use axum::{
    Json,
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use http_body_util::BodyExt as _;
use memoire_api::{DatabaseKind, Page, Snapshot, SnapshotReason};
use merezha_domain::Failure;
use std::io::Write as _;

/// The media type of a raw SQLite file.
const SQLITE: &str = "application/vnd.sqlite3";

/// `GET …/{name}/snapshots`: newest first, one page.
pub async fn snapshots(
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
    if entry.kind != DatabaseKind::Persistent {
        return refuse(Failure::permanent("not_persistent"));
    }
    match store::snapshots(&routes.database, caller.team.as_str(), &entry.id).await {
        Ok(rows) => Json(Page {
            items: rows.into_iter().map(snapshot).collect(),
            next_cursor: None,
        })
        .into_response(),
        Err(failure) => refuse(failure),
    }
}

fn snapshot(row: store::SnapshotRow) -> Snapshot {
    Snapshot {
        id: row.id,
        created_at: timestamp(row.created_unix_ms),
        size_bytes: row.size_bytes,
        reason: if row.reason == "pre_restore" {
            SnapshotReason::PreRestore
        } else {
            SnapshotReason::Scheduled
        },
    }
}

/// `POST …/{name}/snapshots/{id}/restore`: a `pre_restore` snapshot, then
/// the snapshot's contents, once the requests in flight finished.
pub async fn restore(
    State(routes): State<Routes>,
    Path((name, id)): Path<(String, String)>,
    request: Request,
) -> Response {
    let caller = match admit(&request, &routes.policy, permission::SNAPSHOTS) {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    let entry = match active(&routes, &caller, &name).await {
        Ok(entry) => entry,
        Err(refusal) => return refusal,
    };
    if entry.kind != DatabaseKind::Persistent {
        return refuse(Failure::permanent("not_persistent"));
    }
    let team = caller.team.as_str();
    let known = match store::snapshots(&routes.database, team, &entry.id).await {
        Ok(rows) => rows.iter().any(|row| row.id == id),
        Err(failure) => return refuse(failure),
    };
    let Ok(source) = routes.data.snapshot_path(team, &entry.id, &id) else {
        return refuse(Failure::permanent("snapshot_not_found"));
    };
    if !known || !source.is_file() {
        return refuse(Failure::permanent("snapshot_not_found"));
    }
    if let Err(failure) = routes.lifecycle.snapshot(team, &entry, "pre_restore").await {
        return refuse(failure);
    }
    let scratch = match routes.data.scratch() {
        Ok(path) => path,
        Err(failure) => return refuse(failure),
    };
    if std::fs::copy(&source, &scratch).is_err() {
        return refuse(Failure::transient("data_dir_unavailable"));
    }
    if let Err(refused) = routes
        .data
        .replace_with(&entry.target(team), scratch.clone(), false)
        .await
    {
        let _ = std::fs::remove_file(&scratch);
        return refusal(refused);
    }
    respond(&routes, &caller, &name, None, StatusCode::OK).await
}

/// `GET …/{name}/export`: a consistent copy, streamed.
pub async fn export(
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
    let scratch = match routes.data.scratch() {
        Ok(path) => path,
        Err(failure) => return refuse(failure),
    };
    let size = match routes
        .data
        .copy_to(&entry.target(caller.team.as_str()), scratch.clone())
        .await
    {
        Ok(size) => size,
        Err(refused) => {
            let _ = std::fs::remove_file(&scratch);
            return refusal(refused);
        }
    };
    // Open, then unlink: the stream reads a file nobody else can see.
    let file = tokio::fs::File::open(&scratch).await;
    let _ = std::fs::remove_file(&scratch);
    let Ok(file) = file else {
        return refuse(Failure::transient("data_dir_unavailable"));
    };
    let disposition = format!("attachment; filename=\"{name}.sqlite\"");
    let mut response = Response::new(Body::from_stream(tokio_util::io::ReaderStream::new(file)));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(SQLITE));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    response
}

/// `PUT …/{name}/import`: a SQLite file replaces an empty database.
pub async fn import(
    State(routes): State<Routes>,
    Path(name): Path<String>,
    request: Request,
) -> Response {
    let caller = match admit(&request, &routes.policy, permission::WRITE) {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    let sqlite = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case(SQLITE));
    if !sqlite {
        return refuse(Failure::permanent("unsupported_media_type"));
    }
    let limit = routes.settings.max_database_bytes;
    let declared = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > limit) {
        return refuse(Failure::permanent("database_too_large"));
    }
    let entry = match active(&routes, &caller, &name).await {
        Ok(entry) => entry,
        Err(refusal) => return refusal,
    };
    let team = caller.team.as_str();
    let target = entry.target(team);
    // Before reading up to 1 GiB: the target must be empty (checked again
    // when the file is swapped in).
    match routes.data.is_empty(&target).await {
        Ok(true) => {}
        Ok(false) => return refuse(Failure::permanent("database_not_empty")),
        Err(refused) => return refusal(refused),
    }
    let scratch = match routes.data.scratch() {
        Ok(path) => path,
        Err(failure) => return refuse(failure),
    };
    let received = receive(request.into_body(), &scratch, limit).await;
    let outcome = match received {
        Ok(()) => check(scratch.clone()).await,
        Err(refused) => Err(refused),
    };
    let outcome = match outcome {
        Ok(()) => {
            routes
                .data
                .replace_with(&target, scratch.clone(), true)
                .await
        }
        Err(refused) => Err(refused),
    };
    if let Err(refused) = outcome {
        let _ = std::fs::remove_file(&scratch);
        return refusal(refused);
    }
    routes.last_used.touch(team, &entry.id, now_unix_ms());
    respond(&routes, &caller, &name, None, StatusCode::OK).await
}

/// Writes the body to `path`, refusing past `limit` bytes.
async fn receive(mut body: Body, path: &std::path::Path, limit: u64) -> Result<(), Refused> {
    let mut file =
        std::fs::File::create(path).map_err(|_| Refused::Code("data_dir_unavailable"))?;
    let mut written: u64 = 0;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| {
            if merezha_platform::edge::is_body_limit(&error) {
                Refused::Code("body_limit")
            } else {
                Refused::Code("invalid_import")
            }
        })?;
        let Ok(bytes) = frame.into_data() else {
            continue;
        };
        written += bytes.len() as u64;
        if written > limit {
            return Err(Refused::Code("database_too_large"));
        }
        file.write_all(&bytes)
            .map_err(|_| Refused::Code("data_dir_unavailable"))?;
    }
    file.sync_all()
        .map_err(|_| Refused::Code("data_dir_unavailable"))
}

/// `invalid_import` unless the file is a SQLite database that passes
/// `PRAGMA integrity_check`; left in rollback-journal mode for the swap.
async fn check(path: std::path::PathBuf) -> Result<(), Refused> {
    tokio::task::spawn_blocking(move || {
        let invalid = || Refused::Code("invalid_import");
        let mut header = [0u8; 16];
        let mut file = std::fs::File::open(&path).map_err(|_| invalid())?;
        std::io::Read::read_exact(&mut file, &mut header).map_err(|_| invalid())?;
        if &header != b"SQLite format 3\0" {
            return Err(invalid());
        }
        let connection = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| invalid())?;
        let verdict: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(|_| invalid())?;
        if verdict != "ok" {
            return Err(invalid());
        }
        connection
            .pragma_update(None, "journal_mode", "delete")
            .map_err(|_| invalid())
    })
    .await
    .map_err(|_| Refused::Code("internal"))?
}
