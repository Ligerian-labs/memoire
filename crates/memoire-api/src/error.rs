//! The error shape and memoire's own error codes.
//!
//! Errors use Merezha's shape, `{"error": {"code", "classification",
//! "details"?}}`. `sql_error` carries which statement SQLite refused in
//! `details` and SQLite's message in the [`SQL_MESSAGE_HEADER`] header; see
//! [`SqlErrorDetail`]. Clients branch on `code` only; the HTTP status is
//! fixed per code.
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldError {
    pub path: String,
    pub code: String,
}

/// The response header of a `sql_error` that carries SQLite's message,
/// percent-encoded UTF-8.
pub const SQL_MESSAGE_HEADER: &str = "memoire-sql-message";

/// Longest SQLite message a `sql_error` carries, in bytes before encoding.
pub const MAX_SQL_MESSAGE: usize = 512;

/// Which statement SQLite refused, and why: what a `sql_error` response
/// carries in its one `details` entry
/// (`{"path": "/statements/{statement}/sql", "code": "sqlite_<code>"}`)
/// and its [`SQL_MESSAGE_HEADER`] header. The message is SQLite's own text
/// about the caller's SQL, at most 512 bytes; it never contains file paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqlErrorDetail {
    /// 0-based index into `statements`.
    pub statement: usize,
    /// The primary result code name, such as `SQLITE_ERROR` or
    /// `SQLITE_CONSTRAINT`.
    pub sqlite_code: String,
    pub message: String,
}

impl SqlErrorDetail {
    /// The response's `details` entry and header value.
    pub fn to_wire(&self) -> (FieldError, String) {
        (
            FieldError {
                path: format!("/statements/{}/sql", self.statement),
                code: self.sqlite_code.to_ascii_lowercase(),
            },
            percent_encode(&self.message),
        )
    }

    /// Reads a `sql_error` back from its body and its header (`None` when
    /// the header is absent: the message is then empty). `None` for any
    /// other error.
    pub fn from_wire(error: &ErrorDetail, header: Option<&str>) -> Option<Self> {
        if error.code != "sql_error" {
            return None;
        }
        let detail = error.details.first()?;
        let statement = detail
            .path
            .strip_prefix("/statements/")?
            .strip_suffix("/sql")?
            .parse()
            .ok()?;
        let sqlite_code = detail.code.strip_prefix("sqlite_")?;
        Some(Self {
            statement,
            sqlite_code: format!("SQLITE_{}", sqlite_code.to_ascii_uppercase()),
            message: header.and_then(percent_decode).unwrap_or_default(),
        })
    }
}

/// Percent-encodes every byte of `text` but RFC 3986's unreserved ones
/// (`A-Z a-z 0-9 - . _ ~`), so the result is a valid header value.
pub fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The text [`percent_encode`] encoded; `None` for a malformed escape or
/// bytes that are not UTF-8.
pub fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
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
    fn sql_errors_travel_in_details_and_a_header() {
        let detail = SqlErrorDetail {
            statement: 1,
            sqlite_code: "SQLITE_CONSTRAINT".into(),
            message: "UNIQUE constraint failed: users.émail (100%)".into(),
        };
        let (entry, header) = detail.to_wire();
        assert_eq!(
            (entry.path.as_str(), entry.code.as_str()),
            ("/statements/1/sql", "sqlite_constraint")
        );
        assert_eq!(
            header,
            "UNIQUE%20constraint%20failed%3A%20users.%C3%A9mail%20%28100%25%29"
        );
        let body: ErrorBody = serde_json::from_str(
            r#"{"error":{"code":"sql_error","classification":"permanent",
                "details":[{"path":"/statements/1/sql","code":"sqlite_constraint"}]}}"#,
        )
        .unwrap();
        assert_eq!(
            SqlErrorDetail::from_wire(&body.error, Some(&header)),
            Some(detail)
        );
        assert_eq!(
            SqlErrorDetail::from_wire(&body.error, None)
                .unwrap()
                .message,
            ""
        );
        let other = ErrorDetail {
            code: "validation_failed".into(),
            ..body.error
        };
        assert_eq!(SqlErrorDetail::from_wire(&other, Some("x")), None);
        assert_eq!(percent_decode("%zz"), None);
        assert_eq!(percent_decode("%C3"), None);
    }
}
