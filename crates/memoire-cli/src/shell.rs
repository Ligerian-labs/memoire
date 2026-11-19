//! `memoire shell DB`: a line-based SQL shell.
//!
//! A statement ends with `;` outside quotes, comments and trigger bodies,
//! and may span lines; a line may hold several. Dot commands work at the
//! start of a statement. On a terminal the shell
//! prompts and reports errors as it goes; reading a script from standard
//! input it prints no prompts and stops at the first error, whose exit code
//! it returns.
use crate::client::Client;
use crate::commands::run_statement;
use crate::error::{Error, Result, exit};
use crate::output::Out;
use memoire_api::QueryMode;
use std::io::{BufRead as _, IsTerminal as _, Write as _};

const HELP: &str = "\
Statements end with ';' and may span lines. Dot commands:
  .tables          list tables and views
  .schema [NAME]   show the CREATE statements, or one object's
  .mode read|write run on a read-only connection or on the writer
  .help            show this
  .quit            leave (end of input works too)";

pub fn run(out: &Out, client: &Client, db: &str, mode: QueryMode) -> Result<()> {
    let interactive = std::io::stdin().is_terminal();
    let mut shell = Shell {
        out,
        client,
        db,
        mode,
    };
    if interactive {
        out.note(&format!(
            "memoire shell on {db} ({} mode). End statements with ';'; .help for more.",
            mode_name(mode)
        ));
    }
    let mut buffer = String::new();
    let mut stdin = std::io::stdin().lock();
    loop {
        if interactive {
            eprint!(
                "{}",
                if buffer.is_empty() {
                    "memoire> "
                } else {
                    "    ...> "
                }
            );
            let _ = std::io::stderr().flush();
        }
        let mut line = String::new();
        let read = stdin.read_line(&mut line).map_err(|e| {
            Error::local(
                "input_failed",
                exit::FAILURE,
                format!("standard input: {e}"),
            )
        })?;
        if read == 0 {
            break;
        }
        if !split(&buffer).pending && line.trim_start().starts_with('.') {
            buffer.clear();
            match shell.dot(line.trim()) {
                Ok(Flow::Quit) => break,
                Ok(Flow::Continue) => {}
                Err(e) if interactive => crate::report(out, &e),
                Err(e) => return Err(e),
            }
            continue;
        }
        buffer.push_str(&line);
        let Split {
            statements,
            rest,
            pending,
        } = split(&buffer);
        buffer = if pending { rest } else { String::new() };
        for sql in statements {
            match shell.statement(&sql) {
                Ok(()) => {}
                Err(e) if interactive => crate::report(out, &e),
                Err(e) => return Err(e),
            }
        }
    }
    if split(&buffer).pending {
        let e = Error::local(
            "incomplete_statement",
            exit::USAGE,
            "the input ended inside a statement; end it with ';'",
        );
        if !interactive {
            return Err(e);
        }
        crate::report(out, &e);
    }
    Ok(())
}

enum Flow {
    Continue,
    Quit,
}

struct Shell<'a> {
    out: &'a Out,
    client: &'a Client,
    db: &'a str,
    mode: QueryMode,
}

impl Shell<'_> {
    fn statement(&self, sql: &str) -> Result<()> {
        let result = run_statement(self.client, self.db, sql, Vec::new(), self.mode)?;
        self.out.statement(&result)
    }

    fn dot(&mut self, line: &str) -> Result<Flow> {
        let mut words = line.split_whitespace();
        let command = words.next().unwrap_or_default();
        let argument = words.next();
        match (command, argument) {
            (".quit" | ".exit", None) => return Ok(Flow::Quit),
            (".help", None) => self.out.line(HELP)?,
            (".mode", Some("read")) => self.mode = QueryMode::Read,
            (".mode", Some("write")) => self.mode = QueryMode::Write,
            (".mode", None) if self.out.json => {
                self.out
                    .json(&serde_json::json!({"mode": mode_name(self.mode)}))?;
            }
            (".mode", None) => self.out.line(mode_name(self.mode))?,
            (".tables", None) => {
                let schema = self.client.schema(self.db)?;
                let names: Vec<&str> = schema
                    .tables
                    .iter()
                    .map(|t| t.name.as_str())
                    .chain(schema.views.iter().map(|v| v.name.as_str()))
                    .collect();
                if self.out.json {
                    self.out.json(&names)?;
                } else if !names.is_empty() {
                    self.out.line(&names.join("\n"))?;
                }
            }
            (".schema", name) => {
                let mut schema = self.client.schema(self.db)?;
                if let Some(name) = name {
                    schema.tables.retain(|t| t.name == name);
                    schema.views.retain(|v| v.name == name);
                }
                if self.out.json {
                    self.out.json(&schema)?;
                    return Ok(Flow::Continue);
                }
                let mut sql = Vec::new();
                for table in &schema.tables {
                    sql.push(format!("{};", table.sql));
                    sql.extend(
                        table
                            .indexes
                            .iter()
                            .filter_map(|i| i.sql.as_ref().map(|s| format!("{s};"))),
                    );
                }
                sql.extend(schema.views.iter().map(|v| format!("{};", v.sql)));
                if !sql.is_empty() {
                    self.out.line(&sql.join("\n"))?;
                }
            }
            _ => {
                return Err(Error::local(
                    "unknown_command",
                    exit::USAGE,
                    format!("unknown dot command `{line}`; .help lists them"),
                ));
            }
        }
        Ok(Flow::Continue)
    }
}

fn mode_name(mode: QueryMode) -> &'static str {
    match mode {
        QueryMode::Read => "read",
        QueryMode::Write => "write",
    }
}

/// Complete statements found in shell input, and what follows them.
#[derive(Debug, PartialEq, Eq)]
struct Split {
    statements: Vec<String>,
    /// The text after the last complete statement.
    rest: String,
    /// Whether `rest` holds SQL (not only blanks and comments).
    pending: bool,
}

/// Splits `input` at each `;` that ends a statement: outside quotes,
/// identifiers in `"…"`, `` `…` `` or `[…]`, comments, and the body of a
/// `CREATE TRIGGER`, which ends at `END;`.
fn split(input: &str) -> Split {
    #[derive(Clone, Copy, PartialEq)]
    enum In {
        Code,
        Quote(char),
        LineComment,
        BlockComment,
    }
    let mut statements = Vec::new();
    let mut state = In::Code;
    let mut start = 0;
    let mut content = false;
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut chars = input.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if state == In::Code && (c.is_alphanumeric() || c == '_') {
            word.push(c.to_ascii_uppercase());
            content = true;
            continue;
        }
        if !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        let next = chars.peek().map(|(_, n)| *n);
        match (state, c) {
            (In::Code, '\'' | '"' | '`') => {
                state = In::Quote(c);
                content = true;
            }
            (In::Code, '[') => {
                state = In::Quote(']');
                content = true;
            }
            (In::Code, '-') if next == Some('-') => state = In::LineComment,
            (In::Code, '/') if next == Some('*') => {
                chars.next();
                state = In::BlockComment;
            }
            (In::Code, ';') => {
                let trigger = matches!(
                    words.iter().map(String::as_str).take(3).collect::<Vec<_>>()[..],
                    ["CREATE", "TRIGGER", ..] | ["CREATE", "TEMP" | "TEMPORARY", "TRIGGER"]
                );
                if trigger && words.last().map(String::as_str) != Some("END") {
                    continue;
                }
                if content {
                    statements.push(input[start..=i].trim().to_owned());
                }
                start = i + 1;
                content = false;
                words.clear();
            }
            (In::Code, c) if !c.is_whitespace() => content = true,
            (In::Quote(close), c) if c == close => state = In::Code,
            (In::LineComment, '\n') => state = In::Code,
            (In::BlockComment, '*') if next == Some('/') => {
                chars.next();
                state = In::Code;
            }
            _ => {}
        }
    }
    Split {
        statements,
        rest: input[start..].to_owned(),
        pending: content || matches!(state, In::Quote(_) | In::BlockComment),
    }
}

#[cfg(test)]
mod tests {
    use super::split;

    fn statements(input: &str) -> (Vec<String>, bool) {
        let split = split(input);
        (split.statements, split.pending)
    }

    #[test]
    fn statements_end_at_semicolons_outside_quotes_and_comments() {
        assert_eq!(
            statements("SELECT 1; SELECT 'a;b', \"c;\", [d;]; -- done;\n"),
            (
                vec![
                    "SELECT 1;".to_owned(),
                    "SELECT 'a;b', \"c;\", [d;];".to_owned()
                ],
                false
            )
        );
        assert_eq!(
            statements("SELECT 'it''s;\n"),
            (vec![], true),
            "an open string waits for more lines"
        );
        assert_eq!(statements("SELECT /* ; */ 2"), (vec![], true));
        assert_eq!(
            statements("SELECT 1 -- ;\n;"),
            (vec!["SELECT 1 -- ;\n;".to_owned()], false)
        );
        assert_eq!(statements("  ;  -- nothing\n"), (vec![], false));
    }

    #[test]
    fn trigger_bodies_end_at_end() {
        let body = "CREATE TRIGGER t AFTER INSERT ON a BEGIN\n  INSERT INTO b VALUES (1);\n";
        assert_eq!(statements(body), (vec![], true));
        let whole = format!("{body}END;");
        assert_eq!(statements(&whole), (vec![whole.clone()], false));
        assert_eq!(
            statements(
                "create temp trigger t after delete on a begin delete from b; end; select 1;"
            )
            .0
            .len(),
            2
        );
    }
}
