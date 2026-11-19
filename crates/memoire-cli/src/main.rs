//! `memoire`: the command-line client of a memoire gateway.
use clap::Parser as _;
use memoire_cli::{cli::Cli, config::Env, error::exit};
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        // With --json a usage error is JSON too; help and version print as
        // they are.
        Err(e) if e.use_stderr() && std::env::args_os().any(|a| a == "--json") => {
            let rendered = e.render().to_string();
            let message = rendered.lines().next().unwrap_or_default();
            eprintln!(
                "{}",
                serde_json::json!({"error": {
                    "code": "invalid_usage",
                    "classification": "permanent",
                    "message": message.trim_start_matches("error: "),
                }})
            );
            return ExitCode::from(exit::USAGE);
        }
        Err(e) => e.exit(),
    };
    ExitCode::from(memoire_cli::main(cli, &Env::from_process()))
}
