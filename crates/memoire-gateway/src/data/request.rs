//! The body of `POST …/{name}/query`, read with a field-level detail for
//! each problem. Values are read from their JSON text, so a number keeps
//! its token (`1` binds INTEGER, `1.0` REAL) and an out-of-range one is
//! reported at its own path.
use memoire_api::{
    Params, QueryMode, SqlValue,
    limits::{DEFAULT_TIMEOUT_MS, MAX_PARAMS, MAX_SQL_BYTES, MAX_STATEMENTS, MAX_TIMEOUT_MS},
};
use serde_json::value::RawValue;
use std::{collections::BTreeMap, time::Duration};

/// A validated batch.
#[derive(Debug)]
pub struct Batch {
    pub mode: QueryMode,
    pub transaction: bool,
    pub timeout: Duration,
    pub statements: Vec<Statement>,
}

/// One statement and its parameters, as values.
#[derive(Debug)]
pub struct Statement {
    pub sql: String,
    pub params: Bound,
}

/// Parameters as the client sent them: by position or by name.
#[derive(Debug, PartialEq)]
pub enum Bound {
    Positional(Vec<SqlValue>),
    Named(BTreeMap<String, SqlValue>),
}

/// A field-level problem: a JSON Pointer and a code.
pub type Detail = (String, &'static str);

type Object = BTreeMap<String, Box<RawValue>>;

/// The `mode` field alone, so the route can check the permission it needs
/// before it reports field problems. `write` only when it says so.
/// It reads the body as [`parse`] does, so a repeated `mode` key resolves
/// the same way for the permission and for the run.
pub fn mode_of(body: &[u8]) -> QueryMode {
    serde_json::from_slice::<Object>(body)
        .ok()
        .and_then(|root| root.get("mode").map(|raw| raw.get().to_owned()))
        .and_then(|raw| serde_json::from_str::<Option<QueryMode>>(&raw).ok())
        .flatten()
        .unwrap_or_default()
}

/// Reads the body.
///
/// # Errors
///
/// Every problem found, as `(path, code)`: `""` `invalid` for a body that
/// is not a JSON object, `unknown_field`, `required`, `invalid`,
/// `out_of_range` (timeout, statement count), `too_long` (SQL text) and
/// `too_many` (parameters).
pub fn parse(body: &[u8]) -> Result<Batch, Vec<Detail>> {
    let root: Object =
        serde_json::from_slice(body).map_err(|_| vec![(String::new(), "invalid")])?;
    let mut details = Vec::new();
    for key in root.keys() {
        if !["mode", "transaction", "timeout_ms", "statements"].contains(&key.as_str()) {
            details.push((pointer(&[key]), "unknown_field"));
        }
    }
    let present = |key: &str| root.get(key).filter(|v| v.get() != "null");
    let mode = match present("mode") {
        None => QueryMode::Read,
        Some(raw) => serde_json::from_str(raw.get()).unwrap_or_else(|_| {
            details.push(("/mode".into(), "invalid"));
            QueryMode::Read
        }),
    };
    let transaction = match present("transaction") {
        None => false,
        Some(raw) => serde_json::from_str(raw.get()).unwrap_or_else(|_| {
            details.push(("/transaction".into(), "invalid"));
            false
        }),
    };
    let timeout_ms = match present("timeout_ms") {
        None => DEFAULT_TIMEOUT_MS,
        Some(raw) => match serde_json::from_str::<u64>(raw.get()) {
            Ok(ms) if (1..=MAX_TIMEOUT_MS).contains(&ms) => ms,
            _ => {
                details.push(("/timeout_ms".into(), "out_of_range"));
                DEFAULT_TIMEOUT_MS
            }
        },
    };
    let mut statements = Vec::new();
    match present("statements").map(|raw| serde_json::from_str::<Vec<Box<RawValue>>>(raw.get())) {
        None => details.push(("/statements".into(), "required")),
        Some(Err(_)) => details.push(("/statements".into(), "invalid")),
        Some(Ok(list)) if list.is_empty() || list.len() > MAX_STATEMENTS => {
            details.push(("/statements".into(), "out_of_range"));
        }
        Some(Ok(list)) => {
            for (i, raw) in list.iter().enumerate() {
                if let Some(statement) = parse_statement(i, raw, &mut details) {
                    statements.push(statement);
                }
            }
        }
    }
    if details.is_empty() {
        Ok(Batch {
            mode,
            transaction,
            timeout: Duration::from_millis(timeout_ms),
            statements,
        })
    } else {
        Err(details)
    }
}

fn parse_statement(i: usize, raw: &RawValue, details: &mut Vec<Detail>) -> Option<Statement> {
    let index = i.to_string();
    let at = |field: &str| pointer(&["statements", &index, field]);
    let Ok(object) = serde_json::from_str::<Object>(raw.get()) else {
        details.push((pointer(&["statements", &index]), "invalid"));
        return None;
    };
    let before = details.len();
    for key in object.keys() {
        if key != "sql" && key != "params" {
            details.push((at(key), "unknown_field"));
        }
    }
    let sql = match object
        .get("sql")
        .map(|raw| serde_json::from_str::<String>(raw.get()))
    {
        None => {
            details.push((at("sql"), "required"));
            None
        }
        Some(Ok(sql)) if sql.len() > MAX_SQL_BYTES => {
            details.push((at("sql"), "too_long"));
            None
        }
        Some(Ok(sql)) => Some(sql),
        Some(Err(_)) => {
            details.push((at("sql"), "invalid"));
            None
        }
    };
    let params = match object.get("params").filter(|v| v.get() != "null") {
        None => Some(Bound::Positional(Vec::new())),
        Some(raw) => parse_params(&at("params"), raw, details),
    };
    if details.len() > before {
        return None;
    }
    Some(Statement {
        sql: sql?,
        params: params?,
    })
}

fn parse_params(path: &str, raw: &RawValue, details: &mut Vec<Detail>) -> Option<Bound> {
    let text = raw.get().trim_start();
    if text.starts_with('[') {
        let Ok(items) = serde_json::from_str::<Vec<Box<RawValue>>>(text) else {
            details.push((path.into(), "invalid"));
            return None;
        };
        if items.len() > MAX_PARAMS {
            details.push((path.into(), "too_many"));
            return None;
        }
        let mut values = Vec::with_capacity(items.len());
        for (j, item) in items.iter().enumerate() {
            match SqlValue::from_json_text(item.get()) {
                Ok(value) => values.push(value),
                Err(_) => details.push((format!("{path}/{j}"), "invalid")),
            }
        }
        Some(Bound::Positional(values))
    } else if text.starts_with('{') {
        let Ok(items) = serde_json::from_str::<Object>(text) else {
            details.push((path.into(), "invalid"));
            return None;
        };
        if items.len() > MAX_PARAMS {
            details.push((path.into(), "too_many"));
            return None;
        }
        let mut values = BTreeMap::new();
        let before = details.len();
        for (key, item) in &items {
            match SqlValue::from_json_text(item.get()) {
                Ok(value) => {
                    values.insert(key.clone(), value);
                }
                Err(_) => details.push((format!("{path}{}", pointer(&[key])), "invalid")),
            }
        }
        // A repeated key: the map kept one, the contract's reader refuses.
        if details.len() == before && serde_json::from_str::<Params>(text).is_err() {
            details.push((path.into(), "invalid"));
        }
        Some(Bound::Named(values))
    } else {
        details.push((path.into(), "invalid"));
        None
    }
}

/// A JSON Pointer from segments, escaped per RFC 6901.
pub fn pointer(segments: &[&str]) -> String {
    segments
        .iter()
        .map(|s| format!("/{}", s.replace('~', "~0").replace('/', "~1")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn details(body: &str) -> Vec<Detail> {
        let mut found = parse(body.as_bytes()).unwrap_err();
        found.sort();
        found
    }

    #[test]
    fn a_batch_reads_with_its_defaults() {
        let batch =
            parse(br#"{"statements": [{"sql": "select ?, ?", "params": [1, 1.0]}]}"#).unwrap();
        assert_eq!(batch.mode, QueryMode::Read);
        assert!(!batch.transaction);
        assert_eq!(batch.timeout, Duration::from_millis(5000));
        assert_eq!(
            batch.statements[0].params,
            Bound::Positional(vec![SqlValue::Integer(1), SqlValue::Real(1.0)])
        );
        let batch = parse(
            br#"{"mode": "write", "transaction": true, "timeout_ms": 30000,
                 "statements": [{"sql": "select :a", "params": {"a": null}}]}"#,
        )
        .unwrap();
        assert_eq!(batch.mode, QueryMode::Write);
        assert!(batch.transaction);
        assert!(matches!(&batch.statements[0].params, Bound::Named(m) if m["a"] == SqlValue::Null));
    }

    #[test]
    fn every_problem_has_its_path() {
        assert_eq!(
            details(r#"{"mode": "admin", "timeout_ms": 0, "x": 1, "statements": []}"#),
            vec![
                ("/mode".into(), "invalid"),
                ("/statements".into(), "out_of_range"),
                ("/timeout_ms".into(), "out_of_range"),
                ("/x".into(), "unknown_field"),
            ]
        );
        assert_eq!(
            details(
                r#"{"statements": [
                    {"sql": "select ?", "params": [1e400]},
                    {"sql": 1},
                    {"params": {"a": [1]}},
                    {"sql": "select :a", "params": {"a": 1, "a": 2}},
                    {"sql": "select 1", "extra": true},
                    {"sql": "select ?", "params": [99999999999999999999]}
                ]}"#
            ),
            vec![
                ("/statements/0/params/0".into(), "invalid"),
                ("/statements/1/sql".into(), "invalid"),
                ("/statements/2/params/a".into(), "invalid"),
                ("/statements/2/sql".into(), "required"),
                ("/statements/3/params".into(), "invalid"),
                ("/statements/4/extra".into(), "unknown_field"),
                ("/statements/5/params/0".into(), "invalid"),
            ]
        );
        assert_eq!(details("[]"), vec![(String::new(), "invalid")]);
        let long = format!(
            r#"{{"statements": [{{"sql": "{}"}}]}}"#,
            "x".repeat(MAX_SQL_BYTES + 1)
        );
        assert_eq!(
            details(&long),
            vec![("/statements/0/sql".into(), "too_long")]
        );
    }

    #[test]
    fn the_mode_is_read_unless_it_says_write() {
        assert_eq!(mode_of(br#"{"mode": "write"}"#), QueryMode::Write);
        assert_eq!(mode_of(br#"{"mode": "read"}"#), QueryMode::Read);
        assert_eq!(mode_of(b"not json"), QueryMode::Read);
        // A repeated key: the permission and the run see the same, last one.
        let twice = br#"{"mode": "read", "mode": "write", "statements": [{"sql": "select 1"}]}"#;
        assert_eq!(mode_of(twice), QueryMode::Write);
        assert_eq!(parse(twice).unwrap().mode, QueryMode::Write);
    }
}
