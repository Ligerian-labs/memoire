//! The `memoire` command line.
//!
//! The commands talk to a gateway over the HTTP API in `docs/api.md`, with
//! the wire types of `memoire-api`. Output is a table for people and JSON
//! with `--json`; errors go to standard error, in JSON with `--json`, and
//! the exit code tells their class (see [`error::exit`]). Secrets never
//! travel in arguments: keys and passwords come from a prompt, standard
//! input, a file or the environment.
pub mod cli;
pub mod client;
pub mod commands;
pub mod config;
pub mod duration;
pub mod error;
pub mod input;
pub mod memory;
pub mod output;
pub mod shell;
pub mod skill;

/// Runs a parsed command line; the process exit code.
pub fn main(cli: cli::Cli, env: &config::Env) -> u8 {
    let out = output::Out { json: cli.json };
    match commands::run(cli, env) {
        Ok(()) => error::exit::OK,
        Err(e) => {
            report(&out, &e);
            e.exit_code()
        }
    }
}

/// Prints a failure on standard error.
pub fn report(out: &output::Out, error: &error::Error) {
    if out.json {
        eprintln!("{}", error.to_json());
    } else {
        eprintln!("memoire: {}", error.message());
    }
}
