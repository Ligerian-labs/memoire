//! The command tree. Parsing validates names and durations so a command
//! that reaches the network is well formed.
use crate::duration::parse_seconds;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

/// Create, query and share SQLite databases through a memoire gateway.
#[derive(Debug, Parser)]
#[command(name = "memoire", version, about)]
pub struct Cli {
    /// Print JSON instead of a table, for scripts and agents.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Store the gateway URL and an API key (read from standard input or
    /// MEMOIRE_API_KEY) in ~/.config/memoire/ with mode 0600.
    Login {
        #[arg(long, value_name = "URL")]
        server: String,
    },
    /// Show the caller and its team.
    Whoami,
    /// Manage databases.
    #[command(subcommand)]
    Db(DbCommand),
    /// Run one SQL statement.
    Query(QueryArgs),
    /// Open an interactive SQL shell.
    Shell {
        #[arg(value_parser = database_name)]
        db: String,
    },
    /// Download a consistent copy of a database.
    Export {
        #[arg(value_parser = database_name)]
        db: String,
        #[arg(short, long, value_name = "FILE")]
        output: PathBuf,
    },
    /// Load a SQLite file into an empty database.
    Import {
        #[arg(value_parser = database_name)]
        db: String,
        file: PathBuf,
    },
    /// List or restore snapshots of a persistent database.
    #[command(subcommand)]
    Snapshots(SnapshotsCommand),
    /// Manage machine API keys.
    #[command(subcommand)]
    Keys(KeysCommand),
}

#[derive(Debug, Subcommand)]
pub enum DbCommand {
    /// Create a database: burstable by default.
    Create(CreateArgs),
    /// List the team's databases.
    Ls {
        /// Include expired databases.
        #[arg(long)]
        all: bool,
    },
    /// Show one database.
    Info {
        #[arg(value_parser = database_name)]
        name: String,
    },
    /// Delete a database and its file.
    Rm {
        #[arg(value_parser = database_name)]
        name: String,
    },
    /// Restart a burstable database's idle clock, optionally with a new TTL.
    Extend {
        #[arg(value_parser = database_name)]
        name: String,
        #[arg(long, value_name = "DURATION", value_parser = parse_seconds)]
        ttl: Option<u64>,
    },
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    #[arg(value_parser = database_name)]
    pub name: String,
    /// Scratch database that expires when idle (the default).
    #[arg(long, conflicts_with = "persistent")]
    pub burstable: bool,
    /// Idle TTL of a burstable database, such as 24h (the default) or 30m.
    #[arg(long, value_name = "DURATION", value_parser = parse_seconds, conflicts_with = "persistent")]
    pub ttl: Option<u64>,
    /// Database that lives until deleted, with snapshots.
    #[arg(long)]
    pub persistent: bool,
}

impl CreateArgs {
    pub fn kind(&self) -> memoire_api::DatabaseKind {
        if self.persistent {
            memoire_api::DatabaseKind::Persistent
        } else {
            memoire_api::DatabaseKind::Burstable
        }
    }
}

#[derive(Debug, Args)]
pub struct QueryArgs {
    #[arg(value_parser = database_name)]
    pub db: String,
    pub sql: String,
    /// A positional parameter, repeatable. Parsed as JSON when it is valid
    /// JSON (`42`, `"text"`, `null`), else bound as text.
    #[arg(short = 'p', long = "param", value_name = "VALUE")]
    pub params: Vec<String>,
    /// Run on the writer connection.
    #[arg(long)]
    pub write: bool,
}

#[derive(Debug, Subcommand)]
pub enum SnapshotsCommand {
    Ls {
        #[arg(value_parser = database_name)]
        db: String,
    },
    Restore {
        #[arg(value_parser = database_name)]
        db: String,
        id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum KeysCommand {
    /// Create a machine (service account) and an API key for it. Signs in
    /// with the member's email and password, read from the terminal.
    Create {
        #[arg(long, value_name = "MACHINE")]
        name: String,
        /// The team (`org_…`); defaults to the login key's team.
        #[arg(long, value_name = "TEAM")]
        team: Option<String>,
        /// Key lifetime, at most 365d.
        #[arg(long, value_name = "DURATION", value_parser = parse_seconds, default_value = "90d")]
        expires: u64,
        /// Grant only `memoire.databases.read`.
        #[arg(long)]
        read_only: bool,
    },
}

fn database_name(text: &str) -> Result<String, String> {
    if memoire_api::valid_name(text) {
        Ok(text.to_owned())
    } else {
        Err("a database name is 2-63 characters: a lowercase letter, then lowercase letters, digits or '-'".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("memoire").chain(args.iter().copied()))
    }

    #[test]
    fn command_tree_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn create_defaults_to_burstable_with_an_optional_ttl() {
        let Command::Db(DbCommand::Create(args)) =
            parse(&["db", "create", "scratch", "--burstable", "--ttl", "30m"])
                .unwrap()
                .command
        else {
            panic!("db create");
        };
        assert_eq!(args.kind(), memoire_api::DatabaseKind::Burstable);
        assert_eq!(args.ttl, Some(1_800));
        let Command::Db(DbCommand::Create(args)) =
            parse(&["db", "create", "notes", "--persistent"])
                .unwrap()
                .command
        else {
            panic!("db create");
        };
        assert_eq!(args.kind(), memoire_api::DatabaseKind::Persistent);
    }

    #[test]
    fn create_rejects_conflicting_kinds_and_bad_names() {
        assert!(parse(&["db", "create", "x1", "--burstable", "--persistent"]).is_err());
        assert!(parse(&["db", "create", "x1", "--persistent", "--ttl", "1h"]).is_err());
        assert!(parse(&["db", "create", "Bad_Name"]).is_err());
        assert!(parse(&["db", "create", "x1", "--ttl", "soon"]).is_err());
    }

    #[test]
    fn query_collects_params_and_global_json() {
        let cli = parse(&[
            "query",
            "scratch",
            "select ?, ?",
            "-p",
            "1",
            "-p",
            "a",
            "--json",
        ])
        .unwrap();
        assert!(cli.json);
        let Command::Query(args) = cli.command else {
            panic!("query");
        };
        assert_eq!(args.params, ["1", "a"]);
        assert!(!args.write);
    }

    #[test]
    fn keys_create_defaults_to_ninety_days() {
        let Command::Keys(KeysCommand::Create {
            expires,
            read_only,
            team,
            ..
        }) = parse(&["keys", "create", "--name", "ci"]).unwrap().command
        else {
            panic!("keys create");
        };
        assert_eq!(expires, 90 * 86_400);
        assert!(!read_only);
        assert_eq!(team, None);
    }

    #[test]
    fn login_takes_no_secret_argument() {
        assert!(parse(&["login", "--server", "https://m.example", "--key", "mzk_x"]).is_err());
    }
}
