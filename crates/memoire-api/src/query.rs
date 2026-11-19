//! `POST /v1/memoire/databases/{name}/query`.
use crate::SqlValue;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Which connection runs the batch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryMode {
    /// A read-only connection: any write fails with `read_only`.
    #[default]
    Read,
    /// The database's single writer connection.
    Write,
}

/// A batch of statements.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryRequest {
    #[serde(default)]
    pub mode: QueryMode,
    /// Run the batch as one transaction: all statements apply or none.
    #[serde(default)]
    pub transaction: bool,
    /// Per-statement deadline, at most 30 000; defaults to 5 000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// One to 100 statements, run in order.
    pub statements: Vec<Statement>,
}

/// One SQL statement and its parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Statement {
    /// Exactly one SQL statement; a trailing `;` is allowed.
    pub sql: String,
    #[serde(default, skip_serializing_if = "Params::is_empty")]
    pub params: Params,
}

/// Positional (`?`, `?NNN`) or named (`:name`, `@name`, `$name`) parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Params {
    Positional(Vec<SqlValue>),
    /// Keys without a sigil match `:key`, `@key` and `$key`; a key with a
    /// sigil matches only that parameter.
    Named(BTreeMap<String, SqlValue>),
}

impl Default for Params {
    fn default() -> Self {
        Self::Positional(Vec::new())
    }
}

impl Params {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Positional(p) => p.is_empty(),
            Self::Named(p) => p.is_empty(),
        }
    }
}

/// A result column.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Column {
    pub name: String,
    /// The declared type as written in the schema (`INTEGER`, `TEXT`,
    /// `varchar(20)`…), `null` for an expression or an untyped column.
    pub decltype: Option<String>,
}

/// The outcome of one statement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatementResult {
    /// Empty for a statement that returns no rows.
    pub columns: Vec<Column>,
    /// One array per row, in column order.
    pub rows: Vec<Vec<SqlValue>>,
    /// `sqlite3_changes()` for INSERT, UPDATE and DELETE; 0 otherwise.
    pub rows_affected: u64,
    /// The rowid of the last insert by this statement, `null` when it
    /// inserted nothing.
    pub last_insert_rowid: Option<SqlValue>,
}

/// The successful reply: one result per statement, in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryResponse {
    pub results: Vec<StatementResult>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_defaults_to_read_without_transaction() {
        let request: QueryRequest =
            serde_json::from_value(json!({"statements": [{"sql": "select 1"}]})).unwrap();
        assert_eq!(request.mode, QueryMode::Read);
        assert!(!request.transaction);
        assert_eq!(request.statements[0].params, Params::Positional(Vec::new()));
    }

    #[test]
    fn params_are_positional_or_named() {
        let request: QueryRequest = serde_json::from_value(json!({
            "mode": "write", "transaction": true,
            "statements": [
                {"sql": "insert into t values (?, ?)", "params": [1, "a"]},
                {"sql": "update t set b = :b", "params": {"b": {"type": "blob", "value": "AA=="}}},
            ],
        }))
        .unwrap();
        assert_eq!(
            request.statements[0].params,
            Params::Positional(vec![SqlValue::Integer(1), SqlValue::Text("a".into())])
        );
        let Params::Named(named) = &request.statements[1].params else {
            panic!("named params");
        };
        assert_eq!(named["b"], SqlValue::Blob(vec![0]));
    }

    #[test]
    fn request_rejects_unknown_fields() {
        assert!(
            serde_json::from_value::<QueryRequest>(
                json!({"statements": [], "mode": "read", "readonly": true})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<QueryRequest>(json!({"statements": [], "mode": "admin"}))
                .is_err()
        );
    }

    #[test]
    fn result_encodes_values() {
        let result = StatementResult {
            columns: vec![Column {
                name: "id".into(),
                decltype: Some("INTEGER".into()),
            }],
            rows: vec![vec![SqlValue::Integer(i64::MAX)]],
            rows_affected: 0,
            last_insert_rowid: None,
        };
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({
                "columns": [{"name": "id", "decltype": "INTEGER"}],
                "rows": [[{"type": "integer", "value": "9223372036854775807"}]],
                "rows_affected": 0, "last_insert_rowid": null,
            })
        );
    }
}
