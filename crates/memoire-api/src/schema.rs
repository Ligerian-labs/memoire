//! `GET …/{name}/schema` and `GET …/{name}/tables/{table}/rows`.
use crate::{Column, SqlValue};
use serde::{Deserialize, Serialize};

/// Every user table and view, sorted by name; SQLite's own `sqlite_*`
/// objects are left out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schema {
    pub tables: Vec<Table>,
    pub views: Vec<View>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    pub name: String,
    /// The `CREATE TABLE` text SQLite stored.
    pub sql: String,
    pub without_rowid: bool,
    pub columns: Vec<SchemaColumn>,
    pub indexes: Vec<Index>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    pub name: String,
    pub sql: String,
    pub columns: Vec<SchemaColumn>,
}

/// A column as `PRAGMA table_xinfo` reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaColumn {
    pub name: String,
    /// Declared type, `""` when none was declared.
    #[serde(rename = "type")]
    pub decltype: String,
    pub not_null: bool,
    /// The default expression as SQL text, `null` when none.
    pub default: Option<String>,
    /// 1-based position in the primary key, 0 when not part of it.
    pub primary_key: u32,
    /// `true` for a generated column.
    pub generated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub name: String,
    pub unique: bool,
    /// Indexed column names in key order; `null` entries are expressions.
    pub columns: Vec<Option<String>>,
    /// The `CREATE INDEX` text, `null` for automatic indexes (a UNIQUE or
    /// PRIMARY KEY constraint).
    pub sql: Option<String>,
}

/// A page of the row browser.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rows {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<SqlValue>>,
    /// Pass back as `?after=` for the next page; `null` on the last page.
    pub next_after: Option<String>,
}
