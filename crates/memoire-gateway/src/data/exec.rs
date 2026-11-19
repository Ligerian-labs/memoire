//! Running a batch on one connection (docs/api.md, `POST …/query`).
use super::{
    connection::{Guard, vacuum, vacuum_into},
    request::{Batch, Bound, Statement},
};
use memoire_api::{
    Column, QueryMode, SqlValue, StatementResult,
    limits::{MAX_RESULT_BYTES, MAX_RESULT_ROWS},
};
use rusqlite::{Connection, ErrorCode, types::ValueRef};
use std::time::Instant;

/// Why a batch stopped.
#[derive(Debug, PartialEq)]
pub enum Refused {
    /// A field problem only binding finds (a count or name mismatch).
    Invalid { path: String, code: &'static str },
    /// The contract's own codes.
    Code(&'static str),
    /// `sql_error`: SQLite refused statement `statement`.
    Sql {
        statement: usize,
        sqlite_code: String,
        message: String,
    },
}

/// Runs `batch` on `connection`. A failing statement leaves no change of
/// its own: each runs in a savepoint without `transaction`, the batch in
/// one transaction with it.
pub fn run(
    connection: &Connection,
    guard: &Guard,
    batch: &Batch,
) -> Result<Vec<StatementResult>, Refused> {
    let write = batch.mode == QueryMode::Write;
    // `{"results":[…]}` and the separators between results.
    let mut budget = MAX_RESULT_BYTES - ENVELOPE - batch.statements.len();
    if batch.transaction {
        let begin = if write { "BEGIN IMMEDIATE" } else { "BEGIN" };
        connection
            .execute_batch(begin)
            .map_err(|e| refusal(0, &e))?;
        let mut results = Vec::with_capacity(batch.statements.len());
        for (i, statement) in batch.statements.iter().enumerate() {
            match one(connection, guard, i, statement, write, batch, &mut budget) {
                Ok(result) => results.push(result),
                Err(refused) => {
                    // SQLite may have rolled back already (`OR ROLLBACK`).
                    if !connection.is_autocommit() {
                        let _ = connection.execute_batch("ROLLBACK");
                    }
                    return Err(refused);
                }
            }
        }
        if let Err(error) = connection.execute_batch("COMMIT") {
            let _ = connection.execute_batch("ROLLBACK");
            return Err(refusal(batch.statements.len().saturating_sub(1), &error));
        }
        return Ok(results);
    }
    let mut results = Vec::with_capacity(batch.statements.len());
    for (i, statement) in batch.statements.iter().enumerate() {
        // VACUUM is atomic and refuses to run inside a savepoint.
        let savepoint = write && !vacuum(&statement.sql);
        if savepoint {
            connection
                .execute_batch("SAVEPOINT memoire_statement")
                .map_err(|e| refusal(i, &e))?;
        }
        let outcome = one(connection, guard, i, statement, write, batch, &mut budget);
        // A conflict clause of `ROLLBACK` ends every savepoint itself; the
        // statement's own refusal is the answer then.
        if savepoint && !connection.is_autocommit() {
            let end = if outcome.is_ok() {
                "RELEASE memoire_statement"
            } else {
                "ROLLBACK TO memoire_statement; RELEASE memoire_statement"
            };
            if let Err(error) = connection.execute_batch(end) {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(refusal(i, &error));
            }
        }
        results.push(outcome?);
    }
    Ok(results)
}

fn one(
    connection: &Connection,
    guard: &Guard,
    i: usize,
    statement: &Statement,
    write: bool,
    batch: &Batch,
    budget: &mut usize,
) -> Result<StatementResult, Refused> {
    if vacuum_into(&statement.sql) {
        return Err(Refused::Code("statement_denied"));
    }
    let deadline = Instant::now() + batch.timeout;
    connection
        .progress_handler(1000, Some(move || Instant::now() >= deadline))
        .map_err(|e| refusal(i, &e))?;
    let result = guarded(connection, guard, i, statement, write, budget);
    finish(connection, result)
}

/// One statement, the authorizer on from its preparation to its last step.
/// A plain `VACUUM` steps with it off: SQLite attaches a database of its own
/// to rebuild the file.
fn guarded(
    connection: &Connection,
    guard: &Guard,
    i: usize,
    statement: &Statement,
    write: bool,
    budget: &mut usize,
) -> Result<StatementResult, Refused> {
    let mut scope = Some(guard.activate());
    let denied = |scope: &Option<_>| {
        scope
            .as_ref()
            .is_some_and(|s: &crate::data::connection::Active<'_>| s.denied())
    };
    let mut prepared = match connection.prepare(&statement.sql) {
        Ok(prepared) => prepared,
        Err(_) if denied(&scope) => return Err(Refused::Code("statement_denied")),
        Err(error) => return Err(refusal(i, &error)),
    };
    if !write && !prepared.readonly() {
        return Err(Refused::Code("read_only"));
    }
    bind(&mut prepared, i, &statement.params)?;
    if vacuum(&statement.sql) {
        scope = None;
    }
    let columns: Vec<Column> = prepared
        .columns()
        .iter()
        .map(|c| Column {
            name: c.name().to_owned(),
            decltype: c.decl_type().map(str::to_owned),
        })
        .collect();
    let count = columns.len();
    // The result's own fields and its columns, before any row.
    let overhead = RESULT
        + columns
            .iter()
            .map(|c| text_size(&c.name) + c.decltype.as_deref().map_or(4, text_size) + COLUMN)
            .sum::<usize>();
    *budget = budget
        .checked_sub(overhead)
        .ok_or(Refused::Code("result_too_large"))?;
    let changes_before = connection.total_changes();
    // SQLite's value is left alone, so `last_insert_rowid()` keeps its
    // meaning across the batch; a statement inserted a rowid row when the
    // value moved.
    let rowid_before = connection.last_insert_rowid();
    let mut rows = Vec::new();
    let mut stepped = prepared.raw_query();
    let outcome = loop {
        match stepped.next() {
            Ok(Some(row)) => {
                if rows.len() == MAX_RESULT_ROWS {
                    break Err(Refused::Code("result_too_large"));
                }
                let mut values = Vec::with_capacity(count);
                let mut cost = ROW;
                for c in 0..count {
                    let value = match row.get_ref(c) {
                        Ok(value) => to_value(value),
                        Err(error) => return Err(refusal(i, &error)),
                    };
                    cost += size(&value) + 1;
                    values.push(value);
                }
                match budget.checked_sub(cost) {
                    Some(left) => *budget = left,
                    None => break Err(Refused::Code("result_too_large")),
                }
                rows.push(values);
            }
            Ok(None) => break Ok(()),
            Err(_) if denied(&scope) => break Err(Refused::Code("statement_denied")),
            Err(error) => break Err(refusal(i, &error)),
        }
    };
    drop(stepped);
    drop(scope);
    outcome.map(|()| {
        let changed = connection.total_changes() != changes_before;
        let rowid = connection.last_insert_rowid();
        StatementResult {
            columns,
            rows,
            rows_affected: if changed { connection.changes() } else { 0 },
            last_insert_rowid: (rowid != rowid_before).then_some(SqlValue::Integer(rowid)),
        }
    })
}

/// Bytes of `{"results":[]}`.
const ENVELOPE: usize = 14;
/// Upper bound of a result's fields without its columns and rows:
/// `{"columns":[],"rows":[],"rows_affected":…,"last_insert_rowid":…}`
/// with the largest numbers.
const RESULT: usize = 128;
/// Upper bound of a column's `{"name":,"decltype":},` around its texts.
const COLUMN: usize = 24;
/// A row's brackets and the separator before it.
const ROW: usize = 3;

/// [`finish`] for the browse routes.
pub(crate) fn finish_browse<T, E>(connection: &Connection, result: Result<T, E>) -> Result<T, E> {
    let _ = connection.progress_handler(0, None::<fn() -> bool>);
    result
}

/// Removes the statement's deadline, then returns `result`.
pub(crate) fn finish<T>(connection: &Connection, result: Result<T, Refused>) -> Result<T, Refused> {
    let _ = connection.progress_handler(0, None::<fn() -> bool>);
    result
}

/// Binds `params` by SQLite's numbering (docs/api.md, "Binding").
fn bind(statement: &mut rusqlite::Statement<'_>, i: usize, params: &Bound) -> Result<(), Refused> {
    let invalid = || Refused::Invalid {
        path: format!("/statements/{i}/params"),
        code: "invalid",
    };
    let count = statement.parameter_count();
    match params {
        Bound::Positional(values) => {
            if values.len() != count {
                return Err(invalid());
            }
            for (n, value) in values.iter().enumerate() {
                statement
                    .raw_bind_parameter(n + 1, to_sql(value))
                    .map_err(|_| invalid())?;
            }
        }
        Bound::Named(values) => {
            let mut used = std::collections::BTreeSet::new();
            for n in 1..=count {
                let name = statement.parameter_name(n).ok_or_else(invalid)?.to_owned();
                let (sigil, bare) = name.split_at(1);
                if !matches!(sigil, ":" | "@" | "$") {
                    return Err(invalid());
                }
                let (key, value) = match (values.get_key_value(&name), values.get_key_value(bare)) {
                    (Some(found), None) | (None, Some(found)) => found,
                    _ => return Err(invalid()),
                };
                used.insert(key.as_str());
                statement
                    .raw_bind_parameter(n, to_sql(value))
                    .map_err(|_| invalid())?;
            }
            if used.len() != values.len() {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

fn to_sql(value: &SqlValue) -> rusqlite::types::Value {
    use rusqlite::types::Value;
    match value {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(n) => Value::Integer(*n),
        SqlValue::Real(x) => Value::Real(*x),
        SqlValue::Text(s) => Value::Text(s.clone()),
        SqlValue::Blob(b) => Value::Blob(b.clone()),
    }
}

/// A stored value in the contract's encoding; TEXT that is not UTF-8 is
/// read lossily.
pub fn to_value(value: ValueRef<'_>) -> SqlValue {
    match value {
        ValueRef::Null => SqlValue::Null,
        ValueRef::Integer(n) => SqlValue::Integer(n),
        ValueRef::Real(x) => SqlValue::Real(x),
        ValueRef::Text(t) => SqlValue::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => SqlValue::Blob(b.to_vec()),
    }
}

/// An upper bound of the value's encoded JSON size.
pub fn size(value: &SqlValue) -> usize {
    match value {
        SqlValue::Null => 4,
        // `{"type":"integer","value":"-9223372036854775808"}`, 49 bytes, at
        // most.
        SqlValue::Integer(_) => 56,
        // The shortest round-trip form is at most 24 bytes; infinity is
        // `{"type":"real","value":"-Infinity"}`.
        SqlValue::Real(_) => 40,
        SqlValue::Text(s) => text_size(s),
        // `{"type":"blob","value":"…"}` around standard base64.
        SqlValue::Blob(b) => b.len().div_ceil(3) * 4 + 26,
    }
}

/// The encoded size of a JSON string: quotes, escapes of `"`, `\\` and
/// control characters.
pub fn text_size(text: &str) -> usize {
    2 + text
        .bytes()
        .map(|b| match b {
            b'"' | b'\\' => 2,
            0..=0x1f => 6,
            _ => 1,
        })
        .sum::<usize>()
}

/// The contract's refusal for a SQLite error.
fn refusal(i: usize, error: &rusqlite::Error) -> Refused {
    match error {
        rusqlite::Error::MultipleStatement => Refused::Invalid {
            path: format!("/statements/{i}/sql"),
            code: "multiple_statements",
        },
        rusqlite::Error::SqliteFailure(failure, message) => {
            sqlite_refusal(i, failure, message.as_deref().unwrap_or(""))
        }
        rusqlite::Error::SqlInputError { error, msg, .. } => sqlite_refusal(i, error, msg),
        _ => Refused::Code("internal"),
    }
}

fn sqlite_refusal(i: usize, failure: &rusqlite::ffi::Error, message: &str) -> Refused {
    match failure.code {
        ErrorCode::AuthorizationForStatementDenied => Refused::Code("statement_denied"),
        ErrorCode::OperationInterrupted => Refused::Code("statement_timeout"),
        ErrorCode::DiskFull => Refused::Code("database_too_large"),
        ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => Refused::Code("sqlite_busy"),
        ErrorCode::ReadOnly => Refused::Code("read_only"),
        _ => Refused::Sql {
            statement: i,
            sqlite_code: code_name(failure.extended_code).to_owned(),
            message: truncate(message, memoire_api::error::MAX_SQL_MESSAGE),
        },
    }
}

/// The primary result code's name.
fn code_name(extended: i32) -> &'static str {
    match extended & 0xff {
        1 => "SQLITE_ERROR",
        2 => "SQLITE_INTERNAL",
        3 => "SQLITE_PERM",
        4 => "SQLITE_ABORT",
        5 => "SQLITE_BUSY",
        6 => "SQLITE_LOCKED",
        7 => "SQLITE_NOMEM",
        8 => "SQLITE_READONLY",
        9 => "SQLITE_INTERRUPT",
        10 => "SQLITE_IOERR",
        11 => "SQLITE_CORRUPT",
        12 => "SQLITE_NOTFOUND",
        13 => "SQLITE_FULL",
        14 => "SQLITE_CANTOPEN",
        15 => "SQLITE_PROTOCOL",
        17 => "SQLITE_SCHEMA",
        18 => "SQLITE_TOOBIG",
        19 => "SQLITE_CONSTRAINT",
        20 => "SQLITE_MISMATCH",
        21 => "SQLITE_MISUSE",
        23 => "SQLITE_AUTH",
        25 => "SQLITE_RANGE",
        26 => "SQLITE_NOTADB",
        _ => "SQLITE_ERROR",
    }
}

/// At most `max` bytes, cut at a character boundary.
fn truncate(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{
        connection::{self, Opened},
        request,
    };
    use serde_json::{Value, json};

    struct Db {
        _dir: tempfile::TempDir,
        writer: Opened,
        reader: Opened,
    }

    fn db(max_bytes: u64) -> Db {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db_0.sqlite");
        connection::create(&path).unwrap();
        Db {
            writer: connection::open(&path, true, max_bytes).unwrap(),
            reader: connection::open(&path, false, max_bytes).unwrap(),
            _dir: dir,
        }
    }

    impl Db {
        fn run(&self, body: Value) -> Result<Vec<StatementResult>, Refused> {
            let batch = request::parse(body.to_string().as_bytes()).unwrap();
            let opened = if batch.mode == QueryMode::Write {
                &self.writer
            } else {
                &self.reader
            };
            run(&opened.connection, &opened.guard, &batch)
        }

        fn write(&self, sql: &[&str]) -> Result<Vec<StatementResult>, Refused> {
            let statements: Vec<_> = sql.iter().map(|s| json!({"sql": s})).collect();
            self.run(json!({"mode": "write", "statements": statements}))
        }

        fn count(&self, table: &str) -> i64 {
            let rows = self
                .run(json!({"statements": [{"sql": format!("SELECT count(*) FROM {table}")}]}))
                .unwrap();
            match rows[0].rows[0][0] {
                SqlValue::Integer(n) => n,
                _ => panic!(),
            }
        }
    }

    fn code(result: Result<Vec<StatementResult>, Refused>) -> &'static str {
        match result {
            Err(Refused::Code(code)) => code,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_authorizer_refuses_files_extensions_and_transaction_control() {
        let db = db(1 << 30);
        for sql in [
            "ATTACH DATABASE '/tmp/other.db' AS other",
            "DETACH DATABASE main",
            "SELECT load_extension('x')",
            "VACUUM INTO '/tmp/copy.db'",
            "PRAGMA journal_mode = DELETE",
            "PRAGMA max_page_count = 999999999",
            "PRAGMA writable_schema = 1",
            "PRAGMA database_list",
            "BEGIN",
            "COMMIT",
            "SAVEPOINT s",
            "RELEASE s",
            "ROLLBACK",
        ] {
            assert_eq!(code(db.write(&[sql])), "statement_denied", "{sql}");
        }
        for sql in [
            "SELECT * FROM pragma_database_list",
            "SELECT * FROM pragma_journal_mode",
            "SELECT * FROM pragma_page_count JOIN pragma_database_list",
        ] {
            assert_eq!(
                code(db.run(json!({"statements": [{"sql": sql}]}))),
                "statement_denied",
                "{sql}"
            );
        }
        // Harmless pragmas and plain VACUUM pass.
        db.write(&["CREATE TABLE t (x)", "PRAGMA foreign_keys = ON", "VACUUM"])
            .unwrap();
        db.run(json!({"statements": [{"sql": "PRAGMA table_info(t)"}]}))
            .unwrap();
    }

    #[test]
    fn read_mode_cannot_write() {
        let db = db(1 << 30);
        db.write(&["CREATE TABLE t (x)"]).unwrap();
        for sql in [
            "INSERT INTO t VALUES (1)",
            "CREATE TABLE u (x)",
            "CREATE TEMP TABLE v (x)",
        ] {
            assert_eq!(
                code(db.run(json!({"statements": [{"sql": sql}]}))),
                "read_only",
                "{sql}"
            );
        }
        assert_eq!(db.count("t"), 0);
    }

    #[test]
    fn a_failing_statement_leaves_no_change_of_its_own() {
        let db = db(1 << 30);
        db.write(&["CREATE TABLE t (x UNIQUE)"]).unwrap();
        // Without a transaction, the statements before the failure stay.
        let failed = db.write(&[
            "INSERT INTO t VALUES (1)",
            "INSERT INTO t VALUES (2), (1)",
            "INSERT INTO t VALUES (3)",
        ]);
        match failed {
            Err(Refused::Sql {
                statement,
                sqlite_code,
                message,
            }) => {
                assert_eq!((statement, sqlite_code.as_str()), (1, "SQLITE_CONSTRAINT"));
                assert!(message.contains("UNIQUE"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            db.count("t"),
            1,
            "statement 0 stays, 1 rolled back, 2 never ran"
        );
        // With one, nothing applies.
        let failed = db.run(json!({"mode": "write", "transaction": true, "statements": [
            {"sql": "INSERT INTO t VALUES (10)"},
            {"sql": "INSERT INTO t VALUES (1)"},
        ]}));
        assert!(matches!(failed, Err(Refused::Sql { statement: 1, .. })));
        assert_eq!(db.count("t"), 1);
        db.run(json!({"mode": "write", "transaction": true, "statements": [
            {"sql": "INSERT INTO t VALUES (10)"}, {"sql": "INSERT INTO t VALUES (11)"},
        ]}))
        .unwrap();
        assert_eq!(db.count("t"), 3);
        // SQLite's own rollback (`OR ROLLBACK`) keeps the statement's error.
        let rolled = db.write(&["INSERT OR ROLLBACK INTO t VALUES (1)"]);
        assert!(
            matches!(&rolled, Err(Refused::Sql { sqlite_code, .. }) if sqlite_code == "SQLITE_CONSTRAINT"),
            "{rolled:?}"
        );
        let rolled = db.run(json!({"mode": "write", "transaction": true, "statements": [
            {"sql": "INSERT INTO t VALUES (20)"}, {"sql": "INSERT OR ROLLBACK INTO t VALUES (1)"},
        ]}));
        assert!(
            matches!(rolled, Err(Refused::Sql { statement: 1, .. })),
            "{rolled:?}"
        );
        assert_eq!(db.count("t"), 3);
        // A result limit rolls its statement back too.
        db.write(&["CREATE TABLE many (n)"]).unwrap();
        let too_many = db.write(&[&format!(
            "INSERT INTO many WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < {}) SELECT n FROM c RETURNING n",
            MAX_RESULT_ROWS + 1
        )]);
        assert_eq!(code(too_many), "result_too_large");
        assert_eq!(db.count("many"), 0);
    }

    #[test]
    fn results_report_columns_changes_and_inserted_rowids() {
        let db = db(1 << 30);
        let results = db
            .run(json!({"mode": "write", "statements": [
                {"sql": "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT, data BLOB, n REAL)"},
                {"sql": "INSERT INTO notes (body, data, n) VALUES (?, ?, ?)",
                 "params": ["hello", {"type": "blob", "value": "AAE="}, 1.0]},
                {"sql": "INSERT INTO notes (id, body) VALUES (9007199254740993, :b)", "params": {"b": "big"}},
                {"sql": "UPDATE notes SET body = upper(body)"},
                {"sql": "SELECT id, body, data, n, 1 + 1 AS two FROM notes ORDER BY id"},
                {"sql": "INSERT OR IGNORE INTO notes (id) VALUES (1)"},
                {"sql": "CREATE TABLE w (k PRIMARY KEY) WITHOUT ROWID"},
                {"sql": "INSERT INTO w VALUES ('a')"},
            ]}))
            .unwrap();
        assert_eq!(
            (results[0].rows_affected, &results[0].last_insert_rowid),
            (0, &None)
        );
        assert_eq!(
            (results[1].rows_affected, &results[1].last_insert_rowid),
            (1, &Some(SqlValue::Integer(1)))
        );
        assert_eq!(
            serde_json::to_value(&results[2].last_insert_rowid).unwrap(),
            json!({"type": "integer", "value": "9007199254740993"})
        );
        assert_eq!(
            (results[3].rows_affected, &results[3].last_insert_rowid),
            (2, &None)
        );
        let select = &results[4];
        assert_eq!(select.rows_affected, 0);
        let decltypes: Vec<_> = select
            .columns
            .iter()
            .map(|c| c.decltype.as_deref())
            .collect();
        assert_eq!(
            decltypes,
            [
                Some("INTEGER"),
                Some("TEXT"),
                Some("BLOB"),
                Some("REAL"),
                None
            ]
        );
        assert_eq!(
            serde_json::to_value(&select.rows[0]).unwrap(),
            json!([1, "HELLO", {"type": "blob", "value": "AAE="}, 1.0, 2])
        );
        assert_eq!(
            (results[5].rows_affected, &results[5].last_insert_rowid),
            (0, &None),
            "OR IGNORE skipped the row"
        );
        assert_eq!(results[7].last_insert_rowid, None, "WITHOUT ROWID");
        // SQL's own last_insert_rowid() keeps its meaning across a batch.
        let results = db
            .run(json!({"mode": "write", "statements": [
                {"sql": "CREATE TABLE child (parent INTEGER)"},
                {"sql": "INSERT INTO notes (body) VALUES ('parent')"},
                {"sql": "INSERT INTO child VALUES (last_insert_rowid())"},
                {"sql": "SELECT parent FROM child"},
            ]}))
            .unwrap();
        assert_eq!(
            results[1].last_insert_rowid,
            Some(SqlValue::Integer(9007199254740994))
        );
        assert_eq!(results[3].rows[0][0], SqlValue::Integer(9007199254740994));
    }

    #[test]
    fn parameters_bind_by_sqlite_numbering() {
        let db = db(1 << 30);
        let select = |sql: &str, params: Value| {
            db.run(json!({"statements": [{"sql": sql, "params": params}]}))
        };
        let value = |r: Result<Vec<StatementResult>, Refused>| r.unwrap()[0].rows[0].clone();
        assert_eq!(
            value(select("SELECT ?2, ?1", json!(["a", "b"]))),
            vec![SqlValue::Text("b".into()), SqlValue::Text("a".into())]
        );
        assert_eq!(
            value(select("SELECT :a, @a, $b", json!({"a": 1, "b": 3}))),
            vec![
                SqlValue::Integer(1),
                SqlValue::Integer(1),
                SqlValue::Integer(3)
            ]
        );
        assert_eq!(
            value(select("SELECT :a, @b", json!({":a": 1, "@b": 2}))),
            vec![SqlValue::Integer(1), SqlValue::Integer(2)]
        );
        assert_eq!(
            value(select("SELECT :a, :a", json!({":a": true}))),
            vec![SqlValue::Integer(1), SqlValue::Integer(1)]
        );
        for (sql, params) in [
            ("SELECT ?", json!([])),
            ("SELECT ?", json!([1, 2])),
            ("SELECT ?", json!({"a": 1})),
            ("SELECT :a", json!({})),
            ("SELECT :a", json!({"a": 1, "b": 2})),
            ("SELECT :a", json!({"a": 1, ":a": 2})),
            ("SELECT :a, @a", json!({"a": 1, "@a": 2})),
            ("SELECT ?1, :a", json!({"a": 1})),
        ] {
            assert_eq!(
                select(sql, params.clone()).unwrap_err(),
                Refused::Invalid {
                    path: "/statements/0/params".into(),
                    code: "invalid"
                },
                "{sql} {params}"
            );
        }
        assert_eq!(
            select("SELECT 1; SELECT 2", json!([])).unwrap_err(),
            Refused::Invalid {
                path: "/statements/0/sql".into(),
                code: "multiple_statements"
            }
        );
        assert!(select("SELECT 1;  -- trailing", json!([])).is_ok());
    }

    #[test]
    fn limits_hold_size_time_and_results() {
        let db = db(64 * 1024);
        db.write(&["CREATE TABLE big (b)"]).unwrap();
        assert_eq!(
            code(db.write(&["INSERT INTO big WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 64) SELECT randomblob(4096) FROM c"])),
            "database_too_large"
        );
        assert_eq!(db.count("big"), 0);
        let slow = db.run(json!({"timeout_ms": 50, "statements": [{"sql":
            "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c) SELECT count(*) FROM c"}]}));
        assert_eq!(code(slow), "statement_timeout");
        // Many small values count by their encoding, separators included.
        let nulls = db.run(json!({"statements": [{"sql": format!(
            "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 10000) SELECT {} FROM c",
            vec!["NULL"; 400].join(", ")
        )}]}));
        assert_eq!(code(nulls), "result_too_large");
        let wide = db.run(json!({"statements": [{"sql":
            "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 20) SELECT randomblob(1000000) FROM c"}]}));
        assert_eq!(code(wide), "result_too_large");
        // The connection still serves after each refusal.
        assert_eq!(db.count("big"), 0);
    }
}
