//! `memoire`: the command-line client. Commands are wired to the gateway
//! as the HTTP API lands; until then each reports `not_implemented`.
use clap::Parser as _;
use memoire_cli::cli::Cli;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let message = "this command is not implemented yet";
    if cli.json {
        println!(
            "{}",
            serde_json::json!({"error": {"code": "not_implemented", "message": message}})
        );
    } else {
        eprintln!("memoire: {message}");
    }
    ExitCode::from(1)
}
