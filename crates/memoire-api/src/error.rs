//! The error shape and memoire's own error codes.
//!
//! Errors use Merezha's shape, `{"error": {"code", "classification",
//! "details"?}}`. `sql_error` adds an `sql` member (see [`SqlErrorDetail`]).
//! Clients branch on `code` only; the HTTP status is fixed per code.
use serde::{Deserialize, Serialize};

/// An error code and the one HTTP status it always has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ErrorCode {
    pub code: &'static str,
    pub status: u16,
}

const fn code(code: &'static str, status: u16) -> ErrorCode {
    ErrorCode { code, status }
}

/// Codes memoire adds to Merezha's kernel and identity codes.
pub const CODES: &[ErrorCode] = &[
    code("statement_denied", 403),
    code("database_not_found", 404),
    code("table_not_found", 404),
    code("snapshot_not_found", 404),
    code("database_exists", 409),
    code("database_not_empty", 409),
    code("not_burstable", 409),
    code("not_persistent", 409),
    code("database_expired", 410),
    code("database_too_large", 413),
    code("database_limit", 422),
    code("read_only", 422),
    code("sql_error", 422),
    code("statement_timeout", 422),
    code("result_too_large", 422),
    code("invalid_import", 422),
    code("sqlite_busy", 503),
];

/// The status of a memoire code, `None` for codes it does not own.
pub fn status_of(code: &str) -> Option<u16> {
    CODES.iter().find(|c| c.code == code).map(|c| c.status)
}

/// The body of every error response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: String,
    /// `permanent`, `transient` (retry later, honoring `Retry-After`) or
    /// `conflict`.
    pub classification: String,
    /// Field errors of `validation_failed`, each a JSON Pointer into the
    /// request body and a code.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<FieldError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sql: Option<SqlErrorDetail>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldError {
    pub path: String,
    pub code: String,
}

/// Which statement SQLite refused, and why. The message is SQLite's own
/// text about the caller's SQL, at most 512 bytes; it never contains file
/// paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqlErrorDetail {
    /// 0-based index into `statements`.
    pub statement: usize,
    /// The primary result code name, such as `SQLITE_ERROR` or
    /// `SQLITE_CONSTRAINT`.
    pub sqlite_code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_and_well_formed() {
        for (i, c) in CODES.iter().enumerate() {
            assert!(
                c.code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{}",
                c.code
            );
            assert!((400..600).contains(&c.status), "{}", c.code);
            assert!(
                CODES[i + 1..].iter().all(|other| other.code != c.code),
                "duplicate {}",
                c.code
            );
        }
        assert_eq!(status_of("database_exists"), Some(409));
        assert_eq!(status_of("unauthenticated"), None);
    }

    #[test]
    fn sql_errors_parse() {
        let body: ErrorBody = serde_json::from_str(
            r#"{"error":{"code":"sql_error","classification":"permanent",
                "sql":{"statement":1,"sqlite_code":"SQLITE_ERROR","message":"no such table: t"}}}"#,
        )
        .unwrap();
        assert_eq!(body.error.sql.unwrap().statement, 1);
    }
}
