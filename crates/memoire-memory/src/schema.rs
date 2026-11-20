//! The store's tables, how a store moves from one version to the next, and
//! the version check every verb makes first.
//!
//! Every object carries the `memoire_memory_` prefix, so a store can share
//! a database with an application's own tables.
use crate::store::{Batch, Lacks, NOW, Store, text};
use crate::{Backend, BackendError, Error, Refusal};
use memoire_api::{SqlValue, StatementResult};

/// The statements that take a store from version *n − 1* to *n*, entry
/// *n − 1* for version *n*. Append only: a released entry never changes.
pub const MIGRATIONS: &[&[&str]] = &[&[
    // No IF NOT EXISTS: a second client applying version 1 at the same
    // time fails here, and re-reads the version instead.
    "CREATE TABLE memoire_memory_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID",
    "CREATE TABLE memoire_memory_memories (
  seq INTEGER PRIMARY KEY,
  id TEXT NOT NULL UNIQUE,
  rev INTEGER NOT NULL,
  content_rev INTEGER NOT NULL,
  content_principal TEXT NOT NULL,
  content_session TEXT NOT NULL,
  kind TEXT NOT NULL, scope TEXT NOT NULL,
  title TEXT NOT NULL, body TEXT NOT NULL,
  tags TEXT NOT NULL, evidence TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('inferred','confirmed','disputed','superseded','forgotten')),
  supersedes TEXT, superseded_by TEXT,
  forgotten INTEGER NOT NULL DEFAULT 0,
  author_principal TEXT NOT NULL, author_agent TEXT NOT NULL, author_session TEXT NOT NULL,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL
)",
    "CREATE INDEX memoire_memory_memories_scope ON memoire_memory_memories (scope, updated_at)",
    "CREATE TABLE memoire_memory_revisions (
  memory_id TEXT NOT NULL, rev INTEGER NOT NULL,
  kind TEXT NOT NULL, scope TEXT NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL,
  tags TEXT NOT NULL, evidence TEXT NOT NULL, status TEXT NOT NULL,
  supersedes TEXT, superseded_by TEXT,
  author_principal TEXT NOT NULL, author_agent TEXT NOT NULL, author_session TEXT NOT NULL,
  reason TEXT NOT NULL, created_at TEXT NOT NULL,
  PRIMARY KEY (memory_id, rev)
) WITHOUT ROWID",
    "CREATE TABLE memoire_memory_confirmations (
  id INTEGER PRIMARY KEY, memory_id TEXT NOT NULL, rev INTEGER NOT NULL,
  principal TEXT NOT NULL, agent TEXT NOT NULL, session TEXT NOT NULL,
  evidence TEXT NOT NULL, created_at TEXT NOT NULL
)",
    "CREATE INDEX memoire_memory_confirmations_memory ON memoire_memory_confirmations (memory_id, rev)",
    "CREATE TABLE memoire_memory_disputes (
  id INTEGER PRIMARY KEY, memory_id TEXT NOT NULL, rev INTEGER NOT NULL,
  principal TEXT NOT NULL, agent TEXT NOT NULL, session TEXT NOT NULL,
  reason TEXT NOT NULL, evidence TEXT NOT NULL, created_at TEXT NOT NULL,
  resolved_at TEXT, resolved_principal TEXT, resolved_agent TEXT, resolved_session TEXT, resolution TEXT
)",
    "CREATE INDEX memoire_memory_disputes_memory ON memoire_memory_disputes (memory_id, resolved_at)",
    "CREATE TABLE memoire_memory_events (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  memory_id TEXT NOT NULL, verb TEXT NOT NULL, rev INTEGER, scope TEXT,
  principal TEXT NOT NULL, agent TEXT NOT NULL, session TEXT NOT NULL,
  summary TEXT, created_at TEXT NOT NULL
)",
    "CREATE INDEX memoire_memory_events_memory ON memoire_memory_events (memory_id)",
    "CREATE VIRTUAL TABLE memoire_memory_fts USING fts5(
  title, body, tags, scope, tokenize = 'porter unicode61 remove_diacritics 2'
)",
]];

/// The version this engine reads and writes.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Statement 0 of every verb's first read.
pub const VERSION_SQL: &str = "SELECT value FROM memoire_memory_meta WHERE key = 'schema_version'";

/// Whether `error` says the database has no store yet.
///
/// Only for a batch whose statement 0 is [`VERSION_SQL`], as every read
/// and version check sends.
pub fn missing_store<E: BackendError>(error: &Error<E>) -> bool {
    crate::store::sql_statement(error)
        .is_some_and(|(statement, code, message)| names_missing_store(statement, code, message))
}

/// Whether SQLite refusing `statement` with `code` and `message` means the
/// store's tables are not there. SQLite's message decides when there is
/// one, so a meta table that exists but is broken is not "uninitialised".
/// Without it (a proxy dropped the header, and the message is the code), a
/// generic error on the version check, statement 0, can only be the
/// missing table.
fn names_missing_store(statement: usize, code: &str, message: &str) -> bool {
    if message != code {
        return message.contains("no such table: memoire_memory_meta");
    }
    statement == 0 && code == "SQLITE_ERROR"
}

/// The version a [`VERSION_SQL`] result holds.
pub fn version_of(result: &StatementResult) -> Result<i64, Lacks> {
    match result.rows.first().and_then(|row| row.first()) {
        None => Ok(0),
        Some(SqlValue::Text(value)) => value
            .parse()
            .map_err(|_| crate::store::invalid_response("a numeric schema_version")),
        Some(_) => Err(crate::store::invalid_response("a text schema_version")),
    }
}

/// Refuses a store of another version than this engine's.
pub fn require_current(version: i64) -> Result<(), Refusal> {
    match version {
        v if v > SCHEMA_VERSION => Err(Refusal::SchemaNewer {
            store_version: v,
            supported_version: SCHEMA_VERSION,
        }),
        v if v < SCHEMA_VERSION => Err(Refusal::SchemaOlder {
            store_version: v,
            supported_version: SCHEMA_VERSION,
        }),
        _ => Ok(()),
    }
}

/// The batch that takes a store to `version`: the migration's statements,
/// then its bookkeeping in `memoire_memory_meta`. Each batch's first
/// statement fails when another client already applied the version.
fn migration_batch(version: i64, store_id: &str) -> Batch {
    let index = usize::try_from(version - 1).expect("versions start at 1");
    let mut batch = Batch::new();
    let migration = format!("migration:{version}");
    if version > 1 {
        batch.push(
            format!("INSERT INTO memoire_memory_meta (key, value) VALUES (?1, {NOW})"),
            vec![text(&migration)],
        );
    }
    for sql in MIGRATIONS[index] {
        batch.push(*sql, vec![]);
    }
    if version == 1 {
        batch.push(
            format!(
                "INSERT INTO memoire_memory_meta (key, value) VALUES \
                 (?1, {NOW}), ('store_id', ?2), ('schema_version', '1')"
            ),
            vec![text(&migration), text(store_id)],
        );
    } else {
        batch.push(
            "UPDATE memoire_memory_meta SET value = ?1 WHERE key = 'schema_version'",
            vec![text(&version.to_string())],
        );
    }
    batch
}

/// Brings the store to [`SCHEMA_VERSION`]; the version it started from.
pub async fn migrate<B: Backend>(backend: &B) -> Result<i64, Error<B::Error>> {
    let store = Store { backend };
    let from = store.version().await?;
    if from > SCHEMA_VERSION {
        require_current(from)?;
    }
    let store_id = format!(
        "mst_{}",
        crate::model::fresh_ulid().map_err(Error::NoRandomness)?
    );
    for version in from + 1..=SCHEMA_VERSION {
        if let Err(e) = store.write(migration_batch(version, &store_id)).await {
            // Another client may have applied it meanwhile.
            if store.version().await? < version {
                return Err(e);
            }
        }
    }
    Ok(from)
}

/// The store's id, fixed when version 1 was applied.
pub async fn store_id<B: Backend>(backend: &B) -> Result<String, Error<B::Error>> {
    let mut batch = Batch::new();
    let index = crate::store::select_store_id(&mut batch);
    Ok(crate::store::store_id_of(
        &Store { backend }.read(batch).await?[index],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_is_the_number_of_migrations() {
        assert_eq!(SCHEMA_VERSION, 1);
        assert_eq!(usize::try_from(SCHEMA_VERSION).unwrap(), MIGRATIONS.len());
        assert!(MIGRATIONS[0][0].starts_with("CREATE TABLE memoire_memory_meta "));
        for migration in MIGRATIONS {
            assert!(!migration.is_empty());
            for sql in *migration {
                assert!(sql.contains("memoire_memory_"), "{sql}");
                assert!(!sql.to_ascii_uppercase().contains("IF NOT EXISTS"), "{sql}");
                assert!(!sql.trim_end().ends_with(';'), "{sql}");
            }
        }
    }

    #[test]
    fn migration_batches_record_what_they_applied() {
        for version in 1..=SCHEMA_VERSION {
            let batch = migration_batch(version, "mst_x");
            let own = MIGRATIONS[usize::try_from(version - 1).unwrap()].len();
            // Version 1 claims itself with its CREATE TABLE, later ones
            // with an INSERT of `migration:<n>`; all end with bookkeeping.
            assert_eq!(batch.len(), own + if version == 1 { 1 } else { 2 });
            assert!(batch.len() <= 100, "the gateway takes 100 statements");
        }
    }

    #[test]
    fn a_missing_store_is_recognised_with_or_without_the_message() {
        // With SQLite's message, from the Memoire-Sql-Message header.
        assert!(names_missing_store(
            0,
            "SQLITE_ERROR",
            "no such table: memoire_memory_meta"
        ));
        // Without it, the message is the code.
        assert!(names_missing_store(0, "SQLITE_ERROR", "SQLITE_ERROR"));
        // The message alone, wherever the batch stopped.
        assert!(names_missing_store(
            3,
            "SQLITE_ERROR",
            "no such table: memoire_memory_meta"
        ));
        // Anything else is another failure.
        assert!(!names_missing_store(0, "SQLITE_BUSY", "SQLITE_BUSY"));
        // A message about something else decides alone: a broken meta
        // table is not a missing store.
        assert!(!names_missing_store(
            0,
            "SQLITE_ERROR",
            "no such column: value"
        ));
        assert!(!names_missing_store(1, "SQLITE_ERROR", "SQLITE_ERROR"));
        assert!(!names_missing_store(
            2,
            "SQLITE_ERROR",
            "no such table: memoire_memory_fts"
        ));
        assert!(!names_missing_store(
            0,
            "SQLITE_CONSTRAINT",
            "SQLITE_CONSTRAINT"
        ));
    }

    #[test]
    fn other_versions_are_refused_with_both_numbers() {
        assert!(require_current(SCHEMA_VERSION).is_ok());
        let newer = require_current(99).unwrap_err();
        assert_eq!(newer.code(), "memory_schema_newer");
        assert_eq!(newer.class(), crate::RefusalClass::Conflict);
        assert_eq!(newer.data()["store_version"], 99);
        assert_eq!(newer.data()["supported_version"], SCHEMA_VERSION);
        assert_eq!(
            require_current(0).unwrap_err().code(),
            "memory_schema_older"
        );
    }
}
