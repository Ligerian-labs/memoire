//! The memory engine on the data plane: the backend the memory routes run
//! its verbs through, and its failures as responses.
//!
//! The engine is the CLI's (`memoire-memory`); here its batches run on the
//! database's own connections, as the query route runs a client's, so a
//! store can be used through either.
use crate::data::{
    DataPlane, Target,
    exec::Refused,
    request::{self, Bound},
};
use crate::http::refuse;
use axum::{
    body::Body,
    http::header,
    response::{IntoResponse, Response},
};
use memoire_api::{Params, QueryMode, StatementResult, limits::DEFAULT_TIMEOUT_MS};
use memoire_memory::{Backend, BackendError, Refusal, RefusalClass, SqlFailure, store::Batch};
use merezha_domain::Failure;
use merezha_platform::{ErrorReply, FieldError};
use serde_json::{Map, Value};
use std::time::Duration;

/// One database's store, reached on the data plane, written as one
/// principal.
pub struct Plane<'a> {
    pub data: &'a DataPlane,
    pub target: Target,
    /// The authenticated caller's subject: what every write records.
    pub principal: String,
}

/// A batch the data plane refused.
#[derive(Debug)]
pub struct Failed(pub Refused);

impl BackendError for Failed {
    fn sql(&self) -> Option<SqlFailure<'_>> {
        match &self.0 {
            Refused::Sql {
                statement,
                sqlite_code,
                message,
            } => Some(SqlFailure {
                statement: *statement,
                code: sqlite_code,
                message,
            }),
            _ => None,
        }
    }
}

impl Backend for Plane<'_> {
    type Error = Failed;

    /// Runs `batch` in one transaction: a write on the database's single
    /// writer, a read on a reader.
    async fn run(&self, mode: QueryMode, batch: Batch) -> Result<Vec<StatementResult>, Failed> {
        let (statements, timeout_ms) = batch.into_parts();
        let statements = statements
            .into_iter()
            .map(|statement| request::Statement {
                sql: statement.sql,
                params: match statement.params {
                    Params::Positional(values) => Bound::Positional(values),
                    Params::Named(values) => Bound::Named(values),
                },
            })
            .collect();
        let batch = request::Batch {
            mode,
            transaction: true,
            timeout: Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)),
            statements,
        };
        self.data.query(&self.target, batch).await.map_err(Failed)
    }

    async fn principal(&self) -> Result<String, Failed> {
        Ok(self.principal.clone())
    }
}

/// A failed verb as its response.
pub fn failure(error: memoire_memory::Error<Failed>) -> Response {
    match error {
        memoire_memory::Error::Refused(refusal) => refused(&refusal),
        // The store's own SQL failed outside the memory's guards: what the
        // query route would answer.
        memoire_memory::Error::Backend(Failed(refused)) => crate::routes::refusal(refused),
        memoire_memory::Error::InvalidResponse(_) => refuse(Failure::transient("internal")),
        memoire_memory::Error::NoRandomness(_) => refuse(Failure::transient("random_unavailable")),
    }
}

/// A refusal of the memory: `validation_failed` with the field for a
/// refused input, else its own code; either with its members in `data`.
pub fn refused(refusal: &Refusal) -> Response {
    let (code, data) = refusal.to_wire();
    if let Some((pointer, why)) = refusal.detail() {
        let details = FieldError::new(pointer, why).into_iter().collect();
        return with_data(
            ErrorReply::new(Failure::permanent(code)).details(details),
            data,
        );
    }
    let failure = match refusal.class() {
        RefusalClass::Conflict => Failure::conflict(code),
        _ => Failure::permanent(code),
    };
    with_data(ErrorReply::new(failure), data)
}

/// The registered error response of `reply` with `data` beside its
/// `error` member, when there is any. The edge passes a registered reply
/// through as it is, setting only its status, so the extra member reaches
/// the client; `error` stays Merezha's own.
pub fn with_data(reply: ErrorReply, data: Map<String, Value>) -> Response {
    let mut body: Value = serde_json::from_slice(&reply.body()).unwrap_or_default();
    let response = reply.into_response();
    if data.is_empty() || !body.is_object() {
        return response;
    }
    body["data"] = Value::Object(data);
    let (mut parts, _) = response.into_parts();
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(
        parts,
        Body::from(serde_json::to_vec(&body).unwrap_or_default()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn body(response: Response) -> (u16, Value) {
        let status = response.status().as_u16();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn a_refusal_carries_its_data_beside_the_error() {
        let conflict = Refusal::Conflict {
            id: "mem_x".into(),
            current_rev: 3,
            since_rev: None,
        };
        // Outside a host the status comes from the kernel's codes alone:
        // an unregistered conflict is 409.
        assert_eq!(
            body(refused(&conflict)).await,
            (
                409,
                json!({"error": {"code": "memory_conflict", "classification": "conflict"},
                       "data": {"id": "mem_x", "current_rev": 3}})
            )
        );
        assert_eq!(
            body(refused(&Refusal::NotInitialized)).await.1,
            json!({"error": {"code": "memory_not_initialized", "classification": "conflict"}})
        );
        let (_, unchanged) = body(refused(&Refusal::Unchanged { id: "mem_x".into() })).await;
        assert_eq!(unchanged["error"]["classification"], "permanent");
        assert_eq!(unchanged["data"], json!({"id": "mem_x"}));
    }

    #[tokio::test]
    async fn a_refused_input_is_a_validation_failure() {
        let refusal = memoire_memory::model::invalid_field("/tags/2", "invalid");
        assert_eq!(
            body(refused(&refusal)).await,
            (
                422,
                json!({"error": {"code": "validation_failed", "classification": "permanent",
                                 "details": [{"path": "/tags/2", "code": "invalid"}]}})
            )
        );
        assert_eq!(
            body(refused(&Refusal::InvalidQuery)).await.1["error"]["details"],
            json!([{"path": "/query", "code": "invalid"}])
        );
        let past = Refusal::InvalidArgument(memoire_memory::Invalid::RevPastCurrent {
            rev: 4,
            id: "mem_x".into(),
            current_rev: 3,
        });
        assert_eq!(
            body(refused(&past)).await,
            (
                422,
                json!({"error": {"code": "validation_failed", "classification": "permanent",
                                 "details": [{"path": "/rev", "code": "out_of_range"}]},
                       "data": {"id": "mem_x", "rev": 4, "current_rev": 3}})
            )
        );
    }

    #[test]
    fn sql_failures_reach_the_engine() {
        let failed = Failed(Refused::Sql {
            statement: 2,
            sqlite_code: "SQLITE_CONSTRAINT".into(),
            message: "NOT NULL constraint failed".into(),
        });
        let sql = failed.sql().unwrap();
        assert_eq!((sql.statement, sql.code), (2, "SQLITE_CONSTRAINT"));
        assert!(Failed(Refused::Code("sqlite_busy")).sql().is_none());
    }
}
