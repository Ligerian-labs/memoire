//! Failures, how they are reported and the exit codes they map to.
//!
//! The exit code depends only on the failure's class, so scripts can branch
//! on it without parsing output; `--json` adds the exact `code`.
use memoire_api::{ErrorDetail, SqlErrorDetail};
use serde_json::{Value, json};

/// The exit codes. Stable: scripts and agents rely on them.
pub mod exit {
    /// Success.
    pub const OK: u8 = 0;
    /// Anything else: an unexpected answer, a gateway fault, a local file
    /// that cannot be read or written.
    pub const FAILURE: u8 = 1;
    /// The command line is wrong (also what clap exits with).
    pub const USAGE: u8 = 2;
    /// No credential, or the gateway refused it (`401`).
    pub const AUTH: u8 = 3;
    /// The credential lacks a permission, the team or the statement is
    /// denied (`403`).
    pub const DENIED: u8 = 4;
    /// No such database, table or snapshot (`404`).
    pub const NOT_FOUND: u8 = 5;
    /// The database's state forbids it: it exists, is not empty, has the
    /// wrong kind or expired (`409`, `410`).
    pub const CONFLICT: u8 = 6;
    /// The gateway refused the request as is: invalid input, an SQL error,
    /// a limit (`400`, `413`, `415`, `422`).
    pub const REJECTED: u8 = 7;
    /// Try again later: the gateway is busy, starting, rate limiting or
    /// unreachable (`429`, `503`, connection failures and timeouts).
    pub const UNAVAILABLE: u8 = 8;
}

/// A failed command.
#[derive(Debug)]
pub enum Error {
    /// The gateway answered with an error body.
    Api {
        status: u16,
        error: Box<ErrorDetail>,
        /// `Retry-After` in seconds, sent with `429` and `503`.
        retry_after: Option<u64>,
        /// A `sql_error`'s statement, code and SQLite message.
        sql: Option<Box<SqlErrorDetail>>,
    },
    /// A failure on this side, with a stable snake_case code.
    Local {
        code: &'static str,
        message: String,
        exit: u8,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn local(code: &'static str, exit: u8, message: impl Into<String>) -> Self {
        Self::Local {
            code,
            message: message.into(),
            exit,
        }
    }

    /// The `code` of the failure, the gateway's or the CLI's own.
    pub fn code(&self) -> &str {
        match self {
            Self::Api { error, .. } => &error.code,
            Self::Local { code, .. } => code,
        }
    }

    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Local { exit, .. } => *exit,
            Self::Api { error, .. } if error.classification == "transient" => exit::UNAVAILABLE,
            Self::Api { status, .. } => match status {
                401 => exit::AUTH,
                403 => exit::DENIED,
                404 => exit::NOT_FOUND,
                409 | 410 => exit::CONFLICT,
                400 | 413 | 415 | 422 => exit::REJECTED,
                429 | 503 => exit::UNAVAILABLE,
                _ => exit::FAILURE,
            },
        }
    }

    /// One line for a person.
    pub fn message(&self) -> String {
        match self {
            Self::Local { message, .. } => message.clone(),
            Self::Api {
                error,
                retry_after,
                sql,
                ..
            } => {
                let mut text = describe(error, sql.as_deref());
                if let Some(seconds) = retry_after {
                    text.push_str(&format!("; retry in {seconds}s"));
                }
                text
            }
        }
    }

    /// The JSON form: the gateway's error envelope as received, or the
    /// same envelope for a local failure, both with a `message` for people.
    pub fn to_json(&self) -> Value {
        match self {
            Self::Api {
                status,
                error,
                retry_after,
                sql,
            } => {
                let mut body = serde_json::to_value(error).unwrap_or(Value::Null);
                if let Some(object) = body.as_object_mut() {
                    object.insert("status".into(), json!(status));
                    object.insert("message".into(), json!(self.message()));
                    if let Some(seconds) = retry_after {
                        object.insert("retry_after_seconds".into(), json!(seconds));
                    }
                    if let Some(sql) = sql {
                        object.insert("sql".into(), json!(sql));
                    }
                }
                json!({ "error": body })
            }
            Self::Local {
                code,
                message,
                exit,
            } => json!({"error": {
                "code": code,
                "classification": if *exit == exit::UNAVAILABLE { "transient" } else { "permanent" },
                "message": message,
            }}),
        }
    }
}

/// A sentence for a gateway error code.
fn describe(error: &ErrorDetail, sql: Option<&SqlErrorDetail>) -> String {
    let fixed = match error.code.as_str() {
        "unauthenticated" => {
            "the gateway refused the credential (missing, unknown, revoked or expired); run `memoire login`"
        }
        "unauthorized" => "the credential lacks the permission this needs in the team",
        "tenant_required" => "the credential has no team; use a team API key",
        "ambiguous_credentials" => "the request carried two credentials",
        "origin_rejected" => {
            "the gateway refused the Origin; the --server URL must be the gateway's application origin"
        }
        "statement_denied" => {
            "the statement is not allowed (ATTACH, DETACH, load_extension, VACUUM INTO, file PRAGMAs or transaction control)"
        }
        "database_not_found" => "no database of that name in the team",
        "table_not_found" => "no table or view of that name",
        "snapshot_not_found" => "no snapshot of that id for the database",
        "not_found" => "the gateway has no such route or resource",
        "database_exists" => "the team already has an active database of that name",
        "database_not_empty" => "import needs an empty database (no table, view, index or trigger)",
        "not_burstable" => "only a burstable database can be extended",
        "not_persistent" => "only a persistent database has snapshots",
        "database_expired" => "the database expired; its file is gone",
        "database_too_large" => "the database would pass its 1 GiB limit",
        "body_limit" => "the request body is too large",
        "database_limit" => "the team already has 50 active databases",
        "read_only" => "the statement writes; run it with --write",
        "statement_timeout" => "the statement ran past its deadline",
        "result_too_large" => "the result passed 10 000 rows or 16 MiB; add a LIMIT",
        "invalid_import" => "the file is not a valid SQLite database",
        "invalid_page" => "the gateway refused the page cursor",
        "sqlite_busy" => "the database's writer is busy",
        "rate_limited" => "too many requests",
        "overloaded" => "the gateway is overloaded",
        "starting" => "the gateway is starting",
        _ => "",
    };
    if !fixed.is_empty() {
        return fixed.to_owned();
    }
    if let Some(sql) = sql {
        return format!(
            "SQLite refused statement {}: {} ({})",
            sql.statement + 1,
            sql.message,
            sql.sqlite_code
        );
    }
    if !error.details.is_empty() {
        let fields: Vec<String> = error
            .details
            .iter()
            .map(|d| {
                format!(
                    "{} {}",
                    if d.path.is_empty() { "/" } else { &d.path },
                    d.code
                )
            })
            .collect();
        return format!("invalid request: {}", fields.join(", "));
    }
    error.code.replace('_', " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use memoire_api::FieldError;

    fn api(status: u16, code: &str, classification: &str) -> Error {
        Error::Api {
            status,
            error: Box::new(ErrorDetail {
                code: code.into(),
                classification: classification.into(),
                details: Vec::new(),
            }),
            retry_after: None,
            sql: None,
        }
    }

    #[test]
    fn exit_codes_follow_the_status_class() {
        for (status, code, expected) in [
            (401, "unauthenticated", exit::AUTH),
            (403, "unauthorized", exit::DENIED),
            (403, "statement_denied", exit::DENIED),
            (404, "database_not_found", exit::NOT_FOUND),
            (409, "database_exists", exit::CONFLICT),
            (410, "database_expired", exit::CONFLICT),
            (413, "database_too_large", exit::REJECTED),
            (422, "sql_error", exit::REJECTED),
            (500, "internal", exit::FAILURE),
        ] {
            assert_eq!(
                api(status, code, "permanent").exit_code(),
                expected,
                "{code}"
            );
        }
        assert_eq!(
            api(503, "sqlite_busy", "transient").exit_code(),
            exit::UNAVAILABLE
        );
        assert_eq!(
            api(429, "rate_limited", "transient").exit_code(),
            exit::UNAVAILABLE
        );
    }

    #[test]
    fn messages_carry_sql_and_field_details() {
        let Error::Api { error, .. } = api(422, "sql_error", "permanent") else {
            unreachable!()
        };
        let failure = Error::Api {
            status: 422,
            error,
            retry_after: None,
            sql: Some(Box::new(SqlErrorDetail {
                statement: 0,
                sqlite_code: "SQLITE_ERROR".into(),
                message: "no such table: t".into(),
            })),
        };
        assert_eq!(
            failure.message(),
            "SQLite refused statement 1: no such table: t (SQLITE_ERROR)"
        );
        assert_eq!(
            failure.to_json()["error"]["sql"]["sqlite_code"],
            "SQLITE_ERROR"
        );

        let Error::Api { mut error, .. } = api(422, "validation_failed", "permanent") else {
            unreachable!()
        };
        error.details = vec![FieldError {
            path: "/idle_ttl_seconds".into(),
            code: "out_of_range".into(),
        }];
        let failure = Error::Api {
            status: 422,
            error,
            retry_after: Some(3),
            sql: None,
        };
        assert_eq!(
            failure.message(),
            "invalid request: /idle_ttl_seconds out_of_range; retry in 3s"
        );
        assert_eq!(failure.to_json()["error"]["retry_after_seconds"], 3);
    }

    #[test]
    fn local_failures_use_the_same_envelope() {
        let failure = Error::local("connection_failed", exit::UNAVAILABLE, "no route");
        assert_eq!(
            failure.to_json(),
            json!({"error": {"code": "connection_failed", "classification": "transient", "message": "no route"}})
        );
    }
}
