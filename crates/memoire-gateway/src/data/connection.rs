//! Opening a database file: the connection settings every request runs
//! under, and the authorizer that refuses what a client may not do.
use rusqlite::{
    Connection, OpenFlags,
    config::DbConfig,
    hooks::{AuthAction, AuthContext, Authorization},
    limits::Limit,
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// How long a statement waits for a lock another connection holds (a
/// checkpoint, a backup) before failing `sqlite_busy`.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Pragmas a client may run: they read or set per-connection behaviour and
/// never touch a file, the journal or the size limit. Every other pragma
/// fails `statement_denied`.
const PRAGMAS: &[&str] = &[
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

/// Functions a client may not call.
const FUNCTIONS: &[&str] = &["load_extension", "fts3_tokenizer"];

/// Whether the statement being prepared is a client's: the authorizer
/// checks client statements only, so the gateway's own savepoints and
/// schema reads pass. It also records a refusal, because SQLite reports
/// some (a denied function) as a plain `SQLITE_ERROR`.
#[derive(Clone, Default)]
pub struct Guard {
    active: Arc<AtomicBool>,
    denied: Arc<AtomicBool>,
}

impl Guard {
    /// Turns the authorizer on until the returned scope drops. A client
    /// statement keeps it on from its preparation to its last step, since
    /// SQLite prepares again while stepping (a schema change, a table-valued
    /// pragma).
    pub fn activate(&self) -> Active<'_> {
        self.denied.store(false, Ordering::SeqCst);
        self.active.store(true, Ordering::SeqCst);
        Active(self)
    }
}

/// The authorizer is on while this lives.
pub struct Active<'a>(&'a Guard);

impl Active<'_> {
    /// Whether the authorizer refused something since activation.
    pub fn denied(&self) -> bool {
        self.0.denied.load(Ordering::SeqCst)
    }
}

impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.active.store(false, Ordering::SeqCst);
    }
}

/// An open connection and its guard.
pub struct Opened {
    pub connection: Connection,
    pub guard: Guard,
}

/// Opens the writer (`write`) or a read-only reader on an existing file.
/// `max_bytes` bounds the file through `max_page_count`.
pub fn open(path: &Path, write: bool, max_bytes: u64) -> rusqlite::Result<Opened> {
    let mode = if write {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let connection = Connection::open_with_flags(
        path,
        mode | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    configure(&connection, write, max_bytes)?;
    let guard = Guard::default();
    let (active, denied) = (guard.active.clone(), guard.denied.clone());
    connection.authorizer(Some(move |context: AuthContext<'_>| {
        if !active.load(Ordering::SeqCst) {
            return Authorization::Allow;
        }
        let decision = authorize(&context);
        if matches!(decision, Authorization::Deny) {
            denied.store(true, Ordering::SeqCst);
        }
        decision
    }))?;
    Ok(Opened { connection, guard })
}

/// Creates the file in WAL mode when it does not exist; an existing file is
/// left as it is.
pub fn create(path: &Path) -> rusqlite::Result<()> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.pragma_update(None, "journal_mode", "wal")?;
    // Write the header, so the file is a database from the start.
    connection.execute_batch("BEGIN IMMEDIATE; COMMIT;")?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    connection.pragma_update(None, "user_version", version)?;
    Ok(())
}

/// Puts an existing file in WAL mode.
pub fn wal(path: &Path) -> rusqlite::Result<()> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.pragma_update(None, "journal_mode", "wal")
}

fn configure(connection: &Connection, write: bool, max_bytes: u64) -> rusqlite::Result<()> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_FTS3_TOKENIZER, false)?;
    for (limit, value) in [
        // One value, row or result string: 16 MiB, the response cap.
        (Limit::SQLITE_LIMIT_LENGTH, 16 << 20),
        // One statement's text: the contract's 100 KiB.
        (Limit::SQLITE_LIMIT_SQL_LENGTH, 100 * 1024),
        (Limit::SQLITE_LIMIT_COLUMN, 2000),
        (Limit::SQLITE_LIMIT_EXPR_DEPTH, 1000),
        (Limit::SQLITE_LIMIT_COMPOUND_SELECT, 500),
        (Limit::SQLITE_LIMIT_VDBE_OP, 250_000_000),
        (Limit::SQLITE_LIMIT_FUNCTION_ARG, 127),
        // VACUUM attaches one internal database; clients cannot attach.
        (Limit::SQLITE_LIMIT_ATTACHED, 1),
        (Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH, 50_000),
        (Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 999),
        (Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 100),
        (Limit::SQLITE_LIMIT_WORKER_THREADS, 0),
    ] {
        connection.set_limit(limit, value)?;
    }
    if write {
        let page_size: u64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        let pages = (max_bytes / page_size.max(512)).max(1);
        connection.pragma_update(None, "max_page_count", pages)?;
        connection.pragma_update(None, "synchronous", "normal")?;
    } else {
        connection.pragma_update(None, "query_only", true)?;
    }
    Ok(())
}

/// The authorizer for client statements.
fn authorize(context: &AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Attach { .. }
        | AuthAction::Detach { .. }
        // Transaction control is the request's `transaction` flag.
        | AuthAction::Transaction { .. }
        | AuthAction::Savepoint { .. } => Authorization::Deny,
        AuthAction::Pragma { pragma_name, .. } => {
            if PRAGMAS.contains(&pragma_name.to_ascii_lowercase().as_str()) {
                Authorization::Allow
            } else {
                Authorization::Deny
            }
        }
        AuthAction::Function { function_name } => {
            if FUNCTIONS.contains(&function_name.to_ascii_lowercase().as_str()) {
                Authorization::Deny
            } else {
                Authorization::Allow
            }
        }
        _ => Authorization::Allow,
    }
}

/// Whether `sql` is a `VACUUM … INTO` statement, which writes a new file.
/// SQLite's authorizer sees no action for it, so the text decides: its
/// first keyword is `VACUUM` and an `INTO` keyword follows.
pub fn vacuum_into(sql: &str) -> bool {
    vacuum(sql) && keywords(sql).iter().any(|w| w.eq_ignore_ascii_case("into"))
}

/// Whether `sql` is a `VACUUM`, which SQLite runs only outside a
/// transaction and which is atomic on its own.
pub fn vacuum(sql: &str) -> bool {
    keywords(sql)
        .first()
        .is_some_and(|w| w.eq_ignore_ascii_case("vacuum"))
}

/// The bare words of `sql` outside comments, strings and quoted names.
fn keywords(sql: &str) -> Vec<&str> {
    let bytes = sql.as_bytes();
    let mut words = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
            }
            quote @ (b'\'' | b'"' | b'`') => {
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                i += 1;
            }
            b'[' => {
                while i < bytes.len() && bytes[i] != b']' {
                    i += 1;
                }
                i += 1;
            }
            b if b.is_ascii_alphabetic() || b == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                words.push(&sql[start..i]);
            }
            _ => i += 1,
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vacuum_into_is_found_outside_comments_and_strings() {
        assert!(vacuum_into("VACUUM INTO '/tmp/x.db'"));
        assert!(vacuum_into("  /* c */ vacuum main into 'x'"));
        assert!(!vacuum_into("VACUUM"));
        assert!(!vacuum_into("-- VACUUM INTO 'x'\nSELECT 1"));
        assert!(!vacuum_into("SELECT 'VACUUM INTO'"));
        assert!(!vacuum_into("INSERT INTO t VALUES ('vacuum')"));
    }
}
