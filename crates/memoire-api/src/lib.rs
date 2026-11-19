//! Wire types of the memoire HTTP API.
//!
//! `docs/api.md` is the contract; these types are its Rust form, shared by
//! the gateway and the CLI. Every route mounts under [`PREFIX`].
//! Timestamps are RFC 3339 UTC strings and durations are whole seconds.

pub mod database;
pub mod error;
pub mod query;
pub mod schema;
pub mod snapshot;
pub mod value;
pub mod whoami;

pub use database::{
    CreateDatabase, Database, DatabaseKind, DatabaseState, ExtendDatabase, Page, valid_name,
};
pub use error::{
    ErrorBody, ErrorCode, ErrorDetail, FieldError, SQL_MESSAGE_HEADER, SqlErrorDetail,
};
pub use query::{
    Column, Params, QueryMode, QueryRequest, QueryResponse, Statement, StatementResult,
};
pub use schema::{Index, Rows, Schema, SchemaColumn, Table, View};
pub use snapshot::{Snapshot, SnapshotReason};
pub use value::SqlValue;
pub use whoami::{PrincipalKind, Whoami};

/// Where the memoire routes mount on the gateway.
pub const PREFIX: &str = "/v1/memoire";

/// Contract limits a client can rely on. The gateway may be configured
/// lower, never higher; the configured values are the ones enforced.
pub mod limits {
    /// Largest database file, 1 GiB.
    pub const MAX_DATABASE_BYTES: u64 = 1 << 30;
    /// Databases a team may hold in state `active`.
    pub const MAX_DATABASES_PER_TEAM: usize = 50;
    /// Default idle TTL of a burstable database, 24 hours.
    pub const DEFAULT_IDLE_TTL_SECONDS: u64 = 24 * 60 * 60;
    /// Shortest idle TTL a burstable database accepts, 1 minute.
    pub const MIN_IDLE_TTL_SECONDS: u64 = 60;
    /// Absolute lifetime of a burstable database, 7 days from creation.
    pub const MAX_LIFETIME_SECONDS: u64 = 7 * 24 * 60 * 60;
    /// Statements in one query request.
    pub const MAX_STATEMENTS: usize = 100;
    /// Bytes of SQL text in one statement.
    pub const MAX_SQL_BYTES: usize = 100 * 1024;
    /// Parameters bound to one statement.
    pub const MAX_PARAMS: usize = 999;
    /// Default per-statement deadline.
    pub const DEFAULT_TIMEOUT_MS: u64 = 5_000;
    /// Longest per-statement deadline a request may ask for.
    pub const MAX_TIMEOUT_MS: u64 = 30_000;
    /// Rows one statement may return.
    pub const MAX_RESULT_ROWS: usize = 10_000;
    /// Encoded bytes of one query response.
    pub const MAX_RESULT_BYTES: usize = 16 << 20;
    /// Request body of every route except import.
    pub const MAX_REQUEST_BYTES: usize = 1 << 20;
    /// Default and largest page of the database list.
    pub const DEFAULT_PAGE: usize = 50;
    pub const MAX_PAGE: usize = 100;
    /// Default and largest page of the row browser.
    pub const DEFAULT_ROWS_PAGE: usize = 100;
    pub const MAX_ROWS_PAGE: usize = 1_000;
}
