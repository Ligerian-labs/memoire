//! `memoire shell DB`: a line-based SQL shell.
//!
//! A statement ends with `;` at the end of a line and may span lines. Dot
//! commands work at the start of a statement. On a terminal the shell
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
        // Fails early on a missing database or a refused key.
        client.database(db)?;
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
        let outcome = if buffer.trim().is_empty() && line.trim_start().starts_with('.') {
            buffer.clear();
            match shell.dot(line.trim()) {
                Ok(Flow::Quit) => break,
                other => other.map(drop),
            }
        } else {
            buffer.push_str(&line);
            if !buffer.trim_end().ends_with(';') {
                continue;
            }
            let sql = std::mem::take(&mut buffer);
            shell.statement(sql.trim())
        };
        if let Err(e) = outcome {
            if !interactive {
                return Err(e);
            }
            crate::report(out, &e);
        }
    }
    if !buffer.trim().is_empty() {
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
                let schema = self.client.schema(self.db)?;
                if self.out.json {
                    self.out.json(&schema)?;
                    return Ok(Flow::Continue);
                }
                let mut sql = Vec::new();
                for table in &schema.tables {
                    if name.is_none_or(|n| n == table.name) {
                        sql.push(format!("{};", table.sql));
                        sql.extend(
                            table
                                .indexes
                                .iter()
                                .filter_map(|i| i.sql.as_ref().map(|s| format!("{s};"))),
                        );
                    }
                }
                for view in &schema.views {
                    if name.is_none_or(|n| n == view.name) {
                        sql.push(format!("{};", view.sql));
                    }
                }
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
