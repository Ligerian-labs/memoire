//! `GET …/{name}/schema` and `GET …/{name}/tables/{table}/rows`, on a
//! reader connection. These run the gateway's own statements, so the
//! authorizer is off.
use super::exec::{size, text_size, to_value};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use memoire_api::{
    Column, Index, Rows, Schema, SchemaColumn, SqlValue, Table, View,
    limits::{DEFAULT_TIMEOUT_MS, MAX_RESULT_BYTES},
};
use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Value, json};

/// Why a page was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    TableNotFound,
    InvalidPage,
    /// One row alone passes the response cap.
    TooLarge,
    /// The default statement deadline passed (a slow view).
    Timeout,
    Sqlite,
}

impl From<rusqlite::Error> for Refused {
    fn from(error: rusqlite::Error) -> Self {
        match error.sqlite_error_code() {
            Some(rusqlite::ErrorCode::OperationInterrupted) => Self::Timeout,
            _ => Self::Sqlite,
        }
    }
}

/// Runs `work` under the contract's default statement deadline.
pub fn bounded<T>(
    connection: &Connection,
    work: impl FnOnce() -> Result<T, Refused>,
) -> Result<T, Refused> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(DEFAULT_TIMEOUT_MS);
    connection.progress_handler(1000, Some(move || std::time::Instant::now() >= deadline))?;
    super::exec::finish_browse(connection, work())
}

const USER_OBJECTS: &str = "name NOT LIKE 'sqlite\\_%' ESCAPE '\\'";

/// Every user table and view, by name.
pub fn schema(connection: &Connection) -> rusqlite::Result<Schema> {
    let objects = |kind: &str| -> rusqlite::Result<Vec<(String, String)>> {
        let mut statement = connection.prepare(&format!(
            "SELECT name, coalesce(sql, '') FROM sqlite_schema WHERE type = ?1 AND {USER_OBJECTS} ORDER BY name"
        ))?;
        statement
            .query_map([kind], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect()
    };
    let mut tables = Vec::new();
    for (name, sql) in objects("table")? {
        let without_rowid: bool = connection
            .query_row(
                "SELECT wr FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
                [&name],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(false);
        tables.push(Table {
            columns: columns(connection, &name)?,
            indexes: indexes(connection, &name)?,
            name,
            sql,
            without_rowid,
        });
    }
    let mut views = Vec::new();
    for (name, sql) in objects("view")? {
        views.push(View {
            columns: columns(connection, &name)?,
            name,
            sql,
        });
    }
    Ok(Schema { tables, views })
}

/// `PRAGMA table_xinfo`, hidden columns left out.
fn columns(connection: &Connection, object: &str) -> rusqlite::Result<Vec<SchemaColumn>> {
    let mut statement = connection.prepare(
        "SELECT name, type, \"notnull\", dflt_value, pk, hidden FROM pragma_table_xinfo(?1) WHERE hidden <> 1 ORDER BY cid",
    )?;
    statement
        .query_map([object], |row| {
            let hidden: i64 = row.get(5)?;
            Ok(SchemaColumn {
                name: row.get(0)?,
                decltype: row.get(1)?,
                not_null: row.get(2)?,
                default: row.get(3)?,
                primary_key: row.get(4)?,
                generated: hidden == 2 || hidden == 3,
            })
        })?
        .collect()
}

fn indexes(connection: &Connection, table: &str) -> rusqlite::Result<Vec<Index>> {
    let mut list = connection.prepare(
        "SELECT l.name, l.\"unique\", s.sql FROM pragma_index_list(?1) AS l \
         LEFT JOIN sqlite_schema AS s ON s.type = 'index' AND s.name = l.name ORDER BY l.name",
    )?;
    let found: Vec<(String, bool, Option<String>)> = list
        .query_map([table], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut keys = connection.prepare(
        "SELECT CASE WHEN cid >= 0 THEN name END FROM pragma_index_xinfo(?1) WHERE key = 1 ORDER BY seqno",
    )?;
    let mut indexes = Vec::with_capacity(found.len());
    for (name, unique, sql) in found {
        let columns = keys
            .query_map([&name], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        indexes.push(Index {
            name,
            unique,
            columns,
            sql,
        });
    }
    Ok(indexes)
}

/// How an object pages.
enum Order {
    /// By a rowid alias no column shadows.
    Rowid(&'static str),
    /// A `WITHOUT ROWID` table, by its primary key columns and the
    /// collation each uses in the key.
    Key(Vec<(String, String)>),
    /// A view, or a table whose columns shadow every rowid alias.
    Offset,
}

/// One page of `object`'s rows after `after`, at most `limit` rows and the
/// response byte cap.
pub fn rows(
    connection: &Connection,
    database: &str,
    object: &str,
    limit: usize,
    after: Option<&str>,
) -> Result<Rows, Refused> {
    let kind: Option<String> = connection
        .query_row(
            &format!(
                "SELECT type FROM sqlite_schema WHERE name = ?1 AND type IN ('table', 'view') AND {USER_OBJECTS}"
            ),
            [object],
            |row| row.get(0),
        )
        .optional()?;
    let kind = kind.ok_or(Refused::TableNotFound)?;
    let order = order(connection, object, &kind)?;
    let position = match after {
        None => None,
        Some(text) => Some(uncursor(text, database, object).ok_or(Refused::InvalidPage)?),
    };
    let quoted = quote(object);
    let fetch = i64::try_from(limit + 1).unwrap_or(i64::MAX);
    let (sql, bound, keys): (String, Vec<rusqlite::types::Value>, usize) = match (&order, &position)
    {
        (Order::Rowid(alias), None) => (
            format!("SELECT {alias}, * FROM {quoted} ORDER BY {alias} LIMIT ?1"),
            vec![fetch.into()],
            1,
        ),
        (Order::Rowid(alias), Some(Value::Array(key))) if key.len() == 1 => {
            let Ok(SqlValue::Integer(last)) = SqlValue::from_json(&key[0]) else {
                return Err(Refused::InvalidPage);
            };
            (
                format!(
                    "SELECT {alias}, * FROM {quoted} WHERE {alias} > ?2 ORDER BY {alias} LIMIT ?1"
                ),
                vec![fetch.into(), last.into()],
                1,
            )
        }
        (Order::Key(columns), position) => {
            let list = columns
                .iter()
                .map(|(c, _)| quote(c))
                .collect::<Vec<_>>()
                .join(", ");
            // The key's own collations, so the order and the continuation
            // are the index's.
            let keyed = columns
                .iter()
                .map(|(c, collation)| format!("{} COLLATE {}", quote(c), quote(collation)))
                .collect::<Vec<_>>()
                .join(", ");
            let mut bound = vec![fetch.into()];
            let filter = match position {
                None => String::new(),
                Some(Value::Array(key)) if key.len() == columns.len() => {
                    for value in key {
                        let value = SqlValue::from_json(value).map_err(|_| Refused::InvalidPage)?;
                        bound.push(to_sql(value));
                    }
                    let marks = (0..columns.len())
                        .map(|n| format!("?{}", n + 2))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("WHERE ({keyed}) > ({marks})")
                }
                Some(_) => return Err(Refused::InvalidPage),
            };
            (
                format!("SELECT {list}, * FROM {quoted} {filter} ORDER BY {keyed} LIMIT ?1"),
                bound,
                columns.len(),
            )
        }
        (Order::Offset, position) => {
            let offset = match position {
                None => 0,
                Some(Value::Number(n)) => {
                    n.as_i64().filter(|n| *n >= 0).ok_or(Refused::InvalidPage)?
                }
                Some(_) => return Err(Refused::InvalidPage),
            };
            (
                format!("SELECT * FROM {quoted} LIMIT ?1 OFFSET ?2"),
                vec![fetch.into(), offset.into()],
                0,
            )
        }
        (Order::Rowid(_), Some(_)) => return Err(Refused::InvalidPage),
    };
    let mut statement = connection.prepare(&sql)?;
    let columns: Vec<Column> = statement
        .columns()
        .iter()
        .skip(keys)
        .map(|c| Column {
            name: c.name().to_owned(),
            decltype: c.decl_type().map(str::to_owned),
        })
        .collect();
    let width = columns.len();
    let mut found = statement.query(rusqlite::params_from_iter(bound.iter()))?;
    let mut rows = Vec::new();
    let mut last_key = None;
    let mut more = false;
    // `{"columns":[…],"rows":[…],"next_after":"…"}` around the rows.
    let mut budget = MAX_RESULT_BYTES
        - 256
        - columns
            .iter()
            .map(|c| text_size(&c.name) + c.decltype.as_deref().map_or(4, text_size) + 24)
            .sum::<usize>();
    while let Some(row) = found.next()? {
        if rows.len() == limit {
            more = true;
            break;
        }
        let key: Vec<SqlValue> = (0..keys)
            .map(|c| row.get_ref(c).map(to_value))
            .collect::<rusqlite::Result<_>>()?;
        let values: Vec<SqlValue> = (keys..keys + width)
            .map(|c| row.get_ref(c).map(to_value))
            .collect::<rusqlite::Result<_>>()?;
        let cost: usize = 3 + values.iter().map(|v| size(v) + 1).sum::<usize>();
        // A short page rather than a response past the cap, ending at a row
        // that fits; a row that alone passes it is refused.
        if cost > budget {
            if rows.is_empty() {
                return Err(Refused::TooLarge);
            }
            more = true;
            break;
        }
        budget -= cost;
        last_key = Some(key);
        rows.push(values);
    }
    let next = more.then(|| match &order {
        Order::Offset => {
            let start = match &position {
                Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
                _ => 0,
            };
            json!(start + rows.len() as i64)
        }
        _ => Value::Array(
            last_key
                .unwrap_or_default()
                .iter()
                .map(SqlValue::to_json)
                .collect(),
        ),
    });
    Ok(Rows {
        columns,
        rows,
        next_after: next.map(|position| cursor(database, object, &position)),
    })
}

fn order(connection: &Connection, object: &str, kind: &str) -> Result<Order, Refused> {
    if kind == "view" {
        return Ok(Order::Offset);
    }
    let without_rowid: bool = connection
        .query_row(
            "SELECT wr FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
            [object],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false);
    let mut statement =
        connection.prepare("SELECT name, pk FROM pragma_table_xinfo(?1) ORDER BY cid")?;
    let columns: Vec<(String, i64)> = statement
        .query_map(params![object], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if without_rowid {
        let index: String = connection.query_row(
            "SELECT name FROM pragma_index_list(?1) WHERE origin = 'pk'",
            [object],
            |row| row.get(0),
        )?;
        let mut keys = connection.prepare(
            "SELECT name, coll FROM pragma_index_xinfo(?1) WHERE key = 1 ORDER BY seqno",
        )?;
        let key = keys
            .query_map([&index], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        return Ok(Order::Key(key));
    }
    Ok(["rowid", "oid", "_rowid_"]
        .into_iter()
        .find(|alias| {
            !columns
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(alias))
        })
        .map_or(Order::Offset, Order::Rowid))
}

fn to_sql(value: SqlValue) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    match value {
        SqlValue::Null => V::Null,
        SqlValue::Integer(n) => V::Integer(n),
        SqlValue::Real(x) => V::Real(x),
        SqlValue::Text(s) => V::Text(s),
        SqlValue::Blob(b) => V::Blob(b),
    }
}

/// An identifier in double quotes.
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The opaque `next_after`: the database, the object and the position.
fn cursor(database: &str, object: &str, position: &Value) -> String {
    URL_SAFE_NO_PAD.encode(json!({"d": database, "o": object, "p": position}).to_string())
}

/// The position of a cursor made for this database and object.
fn uncursor(text: &str, database: &str, object: &str) -> Option<Value> {
    let bytes = URL_SAFE_NO_PAD.decode(text).ok()?;
    let mut value: Value = serde_json::from_slice(&bytes).ok()?;
    (value["d"] == database && value["o"] == object).then(|| value["p"].take())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database(sql: &str) -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(sql).unwrap();
        connection
    }

    /// Every row of `object`, page by page.
    fn all(connection: &Connection, object: &str, limit: usize) -> Vec<Vec<SqlValue>> {
        let mut out = Vec::new();
        let mut after = None;
        loop {
            let page = rows(connection, "db_1", object, limit, after.as_deref()).unwrap();
            assert!(page.rows.len() <= limit);
            out.extend(page.rows);
            match page.next_after {
                Some(next) => after = Some(next),
                None => return out,
            }
        }
    }

    #[test]
    fn tables_page_by_rowid_key_or_offset() {
        let connection = database(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);
             INSERT INTO notes (body) VALUES ('a'), ('b'), ('c'), ('d'), ('e');
             CREATE TABLE kv (k TEXT, n INTEGER, v BLOB, PRIMARY KEY (k, n)) WITHOUT ROWID;
             INSERT INTO kv VALUES ('b', 2, x'00'), ('a', 1, NULL), ('a', 2, NULL), ('c', 0, NULL);
             CREATE TABLE shadow (rowid INT, oid INT, _rowid_ INT);
             INSERT INTO shadow VALUES (1, 1, 1), (2, 2, 2), (3, 3, 3);
             CREATE TABLE odd (rowid TEXT, x INT);
             INSERT INTO odd VALUES ('z', 1), ('y', 2);
             CREATE VIEW recent AS SELECT body FROM notes WHERE id > 2;
             CREATE TABLE ci (k TEXT, PRIMARY KEY (k COLLATE NOCASE)) WITHOUT ROWID;
             INSERT INTO ci VALUES ('b'), ('A'), ('c'), ('B2');
             CREATE TABLE \"we\"\"ird / name\" (x);
             INSERT INTO \"we\"\"ird / name\" VALUES (1);",
        );
        assert_eq!(all(&connection, "notes", 2).len(), 5);
        let first = rows(&connection, "db_1", "notes", 2, None).unwrap();
        assert_eq!(first.columns.len(), 2);
        assert_eq!(first.columns[0].decltype.as_deref(), Some("INTEGER"));
        assert_eq!(
            first.rows[0],
            vec![SqlValue::Integer(1), SqlValue::Text("a".into())]
        );
        let keys: Vec<_> = all(&connection, "kv", 3)
            .into_iter()
            .map(|r| (r[0].clone(), r[1].clone()))
            .collect();
        assert_eq!(
            keys,
            [("a", 1), ("a", 2), ("b", 2), ("c", 0)]
                .map(|(k, n)| (SqlValue::Text(k.into()), SqlValue::Integer(n)))
        );
        assert_eq!(
            all(&connection, "ci", 1).len(),
            4,
            "pages by the key's collation"
        );
        assert_eq!(all(&connection, "shadow", 2).len(), 3);
        assert_eq!(all(&connection, "odd", 1).len(), 2, "pages by oid");
        assert_eq!(all(&connection, "recent", 2).len(), 3);
        assert_eq!(all(&connection, "we\"ird / name", 10).len(), 1);
    }

    #[test]
    fn a_slow_view_meets_the_default_deadline() {
        let connection = database(
            "CREATE VIEW forever AS WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c)
             SELECT n FROM c WHERE n < 0;",
        );
        let started = std::time::Instant::now();
        let refused = bounded(&connection, || {
            rows(&connection, "db_1", "forever", 10, None)
        });
        assert_eq!(refused.unwrap_err(), Refused::Timeout);
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        // The deadline is gone afterwards.
        assert!(schema(&connection).is_ok());
    }

    #[test]
    fn cursors_belong_to_their_database_and_object() {
        let connection = database(
            "CREATE TABLE a (x); INSERT INTO a VALUES (1), (2);
             CREATE TABLE b (x); INSERT INTO b VALUES (1), (2);",
        );
        let next = rows(&connection, "db_1", "a", 1, None)
            .unwrap()
            .next_after
            .unwrap();
        assert!(rows(&connection, "db_1", "a", 1, Some(&next)).is_ok());
        for (db, object) in [("db_1", "b"), ("db_2", "a")] {
            assert_eq!(
                rows(&connection, db, object, 1, Some(&next)).unwrap_err(),
                Refused::InvalidPage
            );
        }
        assert_eq!(
            rows(&connection, "db_1", "a", 1, Some("garbage!")).unwrap_err(),
            Refused::InvalidPage
        );
        assert_eq!(
            rows(&connection, "db_1", "missing", 1, None).unwrap_err(),
            Refused::TableNotFound
        );
        assert_eq!(
            rows(&connection, "db_1", "sqlite_schema", 1, None).unwrap_err(),
            Refused::TableNotFound
        );
    }

    #[test]
    fn the_schema_lists_tables_views_columns_and_indexes() {
        let connection = database(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL DEFAULT '', email TEXT UNIQUE,
                                 n INT GENERATED ALWAYS AS (length(body)) VIRTUAL);
             CREATE INDEX notes_body ON notes (body);
             CREATE INDEX notes_expr ON notes (lower(body), id);
             CREATE TABLE kv (k TEXT PRIMARY KEY, v) WITHOUT ROWID;
             CREATE VIEW recent AS SELECT * FROM notes;",
        );
        let schema = schema(&connection).unwrap();
        let names: Vec<_> = schema.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["kv", "notes"]);
        assert!(schema.tables[0].without_rowid);
        let notes = &schema.tables[1];
        assert!(!notes.without_rowid);
        assert_eq!(notes.columns[0].primary_key, 1);
        assert_eq!(notes.columns[1].default.as_deref(), Some("''"));
        assert!(notes.columns[1].not_null);
        assert!(notes.columns[3].generated);
        let by_name = |n: &str| notes.indexes.iter().find(|i| i.name == n).unwrap();
        assert_eq!(by_name("notes_body").columns, vec![Some("body".to_owned())]);
        assert_eq!(
            by_name("notes_expr").columns,
            vec![None, Some("id".to_owned())]
        );
        let automatic = notes
            .indexes
            .iter()
            .find(|i| i.name.starts_with("sqlite_autoindex"))
            .unwrap();
        assert!(automatic.unique);
        assert_eq!(automatic.sql, None);
        assert_eq!(schema.views[0].name, "recent");
        assert_eq!(schema.views[0].columns.len(), 4);
    }
}
