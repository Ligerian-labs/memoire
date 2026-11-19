//! `POST /v1/memoire/databases/{name}/query`.
use crate::SqlValue;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::value::RawValue;
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
    #[serde(default, deserialize_with = "null_as_default")]
    pub mode: QueryMode,
    /// Run the batch as one transaction: all statements apply or none.
    #[serde(default, deserialize_with = "null_as_default")]
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
    #[serde(
        default,
        deserialize_with = "null_as_default",
        skip_serializing_if = "Params::is_empty"
    )]
    pub params: Params,
}

/// Positional (`?`, `?NNN`) or named (`:name`, `@name`, `$name`) parameters.
#[derive(Clone, Debug, PartialEq, Serialize)]
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

/// `null` reads as the field's default, the same as omitting it.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Option::<T>::deserialize(deserializer).map(Option::unwrap_or_default)
}

/// A named-parameter object that refuses a repeated key instead of keeping
/// the last value.
struct UniqueNames(BTreeMap<String, SqlValue>);

impl<'de> Deserialize<'de> for UniqueNames {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Names;
        impl<'de> serde::de::Visitor<'de> for Names {
            type Value = UniqueNames;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an object of named parameters")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<UniqueNames, A::Error> {
                let mut names = BTreeMap::new();
                while let Some((name, value)) = map.next_entry::<String, SqlValue>()? {
                    if names.insert(name, value).is_some() {
                        return Err(A::Error::custom("a parameter name appears twice"));
                    }
                }
                Ok(UniqueNames(names))
            }
        }
        deserializer.deserialize_map(Names)
    }
}

impl<'de> Deserialize<'de> for Params {
    // Not `untagged`: serde buffers untagged input, which loses the number
    // tokens `SqlValue` reads.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        let text = raw.get().trim_start();
        if text.starts_with('[') {
            serde_json::from_str(text)
                .map(Self::Positional)
                .map_err(D::Error::custom)
        } else if text.starts_with('{') {
            serde_json::from_str(text)
                .map(|UniqueNames(names)| Self::Named(names))
                .map_err(D::Error::custom)
        } else {
            Err(D::Error::custom("params is an array or an object"))
        }
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

    /// From JSON text, as the gateway receives it.
    fn request(value: serde_json::Value) -> Result<QueryRequest, serde_json::Error> {
        serde_json::from_str(&value.to_string())
    }

    #[test]
    fn request_defaults_to_read_without_transaction() {
        let request = request(json!({"statements": [{"sql": "select 1"}]})).unwrap();
        assert_eq!(request.mode, QueryMode::Read);
        assert!(!request.transaction);
        assert_eq!(request.statements[0].params, Params::Positional(Vec::new()));
    }

    #[test]
    fn params_are_positional_or_named() {
        let request = request(json!({
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
    fn params_keep_integer_overflow_an_error() {
        assert!(
            serde_json::from_str::<Statement>(
                r#"{"sql": "select ?", "params": [-9223372036854775809]}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<Statement>(r#"{"sql": "select :a", "params": {"a": 1e999}}"#)
                .is_err()
        );
        assert!(serde_json::from_str::<Statement>(r#"{"sql": "select 1", "params": 1}"#).is_err());
    }

    #[test]
    fn null_is_the_same_as_omitting() {
        let request = request(json!({
            "mode": null, "transaction": null, "timeout_ms": null,
            "statements": [{"sql": "select 1", "params": null}],
        }))
        .unwrap();
        assert_eq!(request.mode, QueryMode::Read);
        assert!(!request.transaction);
        assert_eq!(request.timeout_ms, None);
        assert!(request.statements[0].params.is_empty());
    }

    #[test]
    fn named_params_refuse_a_repeated_key() {
        assert!(
            serde_json::from_str::<Statement>(
                r#"{"sql": "select :a", "params": {"a": 1, "a": 2}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn request_rejects_unknown_fields() {
        assert!(request(json!({"statements": [], "mode": "read", "readonly": true})).is_err());
        assert!(request(json!({"statements": [], "mode": "admin"})).is_err());
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
