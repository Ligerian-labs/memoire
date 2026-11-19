//! `memoire`: the command-line client of a memoire gateway.
use clap::Parser as _;
use memoire_cli::{cli::Cli, config::Env};
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    ExitCode::from(memoire_cli::main(cli, &Env::from_process()))
}
