//! What commands print: aligned tables for people, JSON for agents.
//!
//! Results go to standard output; notes and errors to standard error. A
//! closed standard output (`memoire db ls | head -1`) ends output quietly.
use crate::error::{Error, Result, exit};
use memoire_api::{Database, SqlValue, StatementResult};
use serde::Serialize;
use std::io::Write as _;

#[derive(Clone, Copy, Debug)]
pub struct Out {
    pub json: bool,
}

impl Out {
    /// Prints `text` and a newline on standard output.
    pub fn line(&self, text: &str) -> Result<()> {
        let mut stdout = std::io::stdout().lock();
        match writeln!(stdout, "{text}").and_then(|()| stdout.flush()) {
            Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(Error::local(
                "output_failed",
                exit::FAILURE,
                format!("standard output: {e}"),
            )),
            _ => Ok(()),
        }
    }

    /// Prints `value` as one line of JSON.
    pub fn json(&self, value: &impl Serialize) -> Result<()> {
        self.line(&serde_json::to_string(value).expect("output serializes"))
    }

    /// A note for people on standard error.
    pub fn note(&self, text: &str) {
        eprintln!("{text}");
    }

    pub fn database(&self, db: &Database) -> Result<()> {
        if self.json {
            return self.json(db);
        }
        self.line(&database_block(db))
    }

    pub fn statement(&self, result: &StatementResult) -> Result<()> {
        if self.json {
            return self.json(result);
        }
        self.line(&statement_text(result))
    }
}

/// Widest cell a table shows; longer ones end in `…` (JSON keeps them
/// whole). Without a cap, one long value pads every row to its width.
const MAX_CELL: usize = 200;

fn clip(cell: &str) -> String {
    if width(cell) <= MAX_CELL {
        return cell.to_owned();
    }
    let mut clipped: String = cell.chars().take(MAX_CELL - 1).collect();
    clipped.push('…');
    clipped
}

/// A table with a header row, columns padded to their widest cell.
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(|c| clip(c)).collect())
        .collect();
    let mut widths: Vec<usize> = headers.iter().map(|h| width(h)).collect();
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(width(cell));
        }
    }
    let render = |cells: Vec<&str>| {
        let padded: Vec<String> = cells
            .iter()
            .zip(&widths)
            .map(|(cell, w)| format!("{cell}{}", " ".repeat(w - width(cell))))
            .collect();
        padded.join("  ").trim_end().to_owned()
    };
    let rule: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    let mut lines = vec![
        render(headers.to_vec()),
        render(rule.iter().map(String::as_str).collect()),
    ];
    lines.extend(
        rows.iter()
            .map(|row| render(row.iter().map(String::as_str).collect())),
    );
    lines.join("\n")
}

fn width(text: &str) -> usize {
    text.chars().count()
}

/// A value as a table cell. Text is shown raw except for control
/// characters, which are escaped so a row stays on one line.
pub fn cell(value: &SqlValue) -> String {
    match value {
        SqlValue::Null => "NULL".into(),
        SqlValue::Integer(n) => n.to_string(),
        SqlValue::Real(x) if x.is_finite() => format!("{x:?}"),
        SqlValue::Real(x) if x.is_nan() => "NaN".into(),
        SqlValue::Real(x) if *x > 0.0 => "Infinity".into(),
        SqlValue::Real(_) => "-Infinity".into(),
        SqlValue::Text(s) => {
            let mut shown = String::with_capacity(s.len());
            for c in s.chars() {
                if c.is_control() {
                    shown.extend(c.escape_default());
                } else {
                    shown.push(c);
                }
            }
            shown
        }
        SqlValue::Blob(b) => {
            const SHOWN: usize = 32;
            let hex: String = b.iter().take(SHOWN).map(|x| format!("{x:02x}")).collect();
            if b.len() > SHOWN {
                format!("x'{hex}…' ({} bytes)", b.len())
            } else {
                format!("x'{hex}'")
            }
        }
    }
}

/// One statement's outcome: its rows, or what it changed.
pub fn statement_text(result: &StatementResult) -> String {
    if result.columns.is_empty() {
        let mut text = format!(
            "{} row{} affected",
            result.rows_affected,
            plural(result.rows_affected)
        );
        if let Some(rowid) = &result.last_insert_rowid {
            text.push_str(&format!(", last insert rowid {}", cell(rowid)));
        }
        return text;
    }
    let headers: Vec<&str> = result.columns.iter().map(|c| c.name.as_str()).collect();
    let rows: Vec<Vec<String>> = result
        .rows
        .iter()
        .map(|row| row.iter().map(cell).collect())
        .collect();
    let count = result.rows.len() as u64;
    format!("{}\n({count} row{})", table(&headers, &rows), plural(count))
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Bytes in binary units: `512 B`, `8.0 KiB`, `1.5 GiB`.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// A duration in its largest whole unit: `90s`, `30m`, `24h`, `7d`.
pub fn duration(seconds: u64) -> String {
    match seconds {
        s if s % 86_400 == 0 && s > 0 => format!("{}d", s / 86_400),
        s if s % 3_600 == 0 && s > 0 => format!("{}h", s / 3_600),
        s if s % 60 == 0 && s > 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

fn or_dash(value: Option<&str>) -> String {
    value.unwrap_or("-").to_owned()
}

pub fn kind(db: &Database) -> &'static str {
    match db.kind {
        memoire_api::DatabaseKind::Burstable => "burstable",
        memoire_api::DatabaseKind::Persistent => "persistent",
    }
}

pub fn state(db: &Database) -> &'static str {
    match db.state {
        memoire_api::DatabaseState::Active => "active",
        memoire_api::DatabaseState::Expired => "expired",
        memoire_api::DatabaseState::Deleted => "deleted",
    }
}

/// `db ls`.
pub fn database_table(databases: &[Database]) -> String {
    let rows: Vec<Vec<String>> = databases
        .iter()
        .map(|db| {
            vec![
                db.name.clone(),
                kind(db).into(),
                state(db).into(),
                db.size_bytes.map_or_else(|| "-".into(), size),
                or_dash(db.last_used_at.as_deref()),
                or_dash(db.expires_at.as_deref()),
            ]
        })
        .collect();
    table(
        &["NAME", "KIND", "STATE", "SIZE", "LAST USED", "EXPIRES"],
        &rows,
    )
}

/// `db info`: one field per line.
pub fn database_block(db: &Database) -> String {
    let mut fields = vec![
        ("name", db.name.clone()),
        ("id", db.id.clone()),
        ("kind", kind(db).into()),
        ("state", state(db).into()),
        ("size", db.size_bytes.map_or_else(|| "-".into(), size)),
        ("created", db.created_at.clone()),
        ("last used", or_dash(db.last_used_at.as_deref())),
    ];
    if let Some(ttl) = db.idle_ttl_seconds {
        fields.push(("idle ttl", duration(ttl)));
    }
    if db.kind == memoire_api::DatabaseKind::Burstable {
        fields.push(("expires", or_dash(db.expires_at.as_deref())));
        fields.push(("expires at most", or_dash(db.expires_at_max.as_deref())));
    }
    // One width for every database, so blocks line up across commands.
    let pad = "expires at most".len();
    fields
        .iter()
        .map(|(k, v)| format!("{k:<pad$}  {v}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use memoire_api::Column;

    #[test]
    fn tables_pad_columns_and_trim_line_ends() {
        let text = table(
            &["id", "body"],
            &[
                vec!["1".into(), "héllo".into()],
                vec!["22".into(), "".into()],
            ],
        );
        assert_eq!(text, "id  body\n--  -----\n1   héllo\n22");
        let long = table(&["a", "b"], &[vec!["x".repeat(1_000), "y".into()]]);
        let first = long.lines().nth(2).unwrap();
        assert_eq!(first.chars().count(), MAX_CELL + 3);
        assert!(first.contains("x…  y"));
    }

    #[test]
    fn cells_follow_sqlite_types() {
        assert_eq!(cell(&SqlValue::Null), "NULL");
        assert_eq!(cell(&SqlValue::Integer(i64::MAX)), "9223372036854775807");
        assert_eq!(cell(&SqlValue::Real(1.0)), "1.0");
        assert_eq!(cell(&SqlValue::Real(2.5e-8)), "2.5e-8");
        assert_eq!(cell(&SqlValue::Real(f64::NEG_INFINITY)), "-Infinity");
        assert_eq!(cell(&SqlValue::Text("a\nb\tc".into())), "a\\nb\\tc");
        assert_eq!(cell(&SqlValue::Blob(vec![0, 255])), "x'00ff'");
        assert_eq!(
            cell(&SqlValue::Blob(vec![1; 40])),
            format!("x'{}…' (40 bytes)", "01".repeat(32))
        );
    }

    #[test]
    fn statements_show_rows_or_changes() {
        let changed = StatementResult {
            columns: vec![],
            rows: vec![],
            rows_affected: 1,
            last_insert_rowid: Some(SqlValue::Integer(7)),
        };
        assert_eq!(
            statement_text(&changed),
            "1 row affected, last insert rowid 7"
        );
        let selected = StatementResult {
            columns: vec![Column {
                name: "n".into(),
                decltype: None,
            }],
            rows: vec![vec![SqlValue::Integer(1)], vec![SqlValue::Null]],
            rows_affected: 0,
            last_insert_rowid: None,
        };
        assert_eq!(statement_text(&selected), "n\n----\n1\nNULL\n(2 rows)");
    }

    #[test]
    fn sizes_and_durations_read_naturally() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(8192), "8.0 KiB");
        assert_eq!(size(3 << 29), "1.5 GiB");
        assert_eq!(duration(86_400), "1d");
        assert_eq!(duration(7_200), "2h");
        assert_eq!(duration(90), "90s");
        assert_eq!(duration(1_800), "30m");
    }
}
