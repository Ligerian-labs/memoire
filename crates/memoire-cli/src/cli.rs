//! The command tree. Parsing validates names and durations so a command
//! that reaches the network is well formed.
use crate::duration::parse_seconds;
use crate::memory::model;
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
    /// Check an API key against a gateway, then store both in
    /// ~/.config/memoire/credentials.json with mode 0600. The key comes from
    /// --key-file, else MEMOIRE_API_KEY, else standard input (a prompt
    /// without echo on a terminal); never from an argument.
    Login {
        #[arg(long, value_name = "URL")]
        server: String,
        /// Read the API key from this file instead.
        #[arg(long, value_name = "FILE")]
        key_file: Option<PathBuf>,
    },
    /// Show the caller and its team.
    Whoami,
    /// Manage databases.
    #[command(subcommand)]
    Db(DbCommand),
    /// Run one SQL statement in read mode, or on the writer with --write.
    Query(QueryArgs),
    /// Open an interactive SQL shell. Statements end with `;`; `.help`
    /// lists the dot commands. Reading a script from standard input, it
    /// stops at the first error.
    Shell {
        #[arg(value_parser = database_name)]
        db: String,
        /// Start on the writer connection (`.mode write` in the shell).
        #[arg(long)]
        write: bool,
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
    /// A shared memory for agents, stored as versioned SQL in a database.
    Memory(Box<MemoryArgs>),
    /// Install the memoire skill for coding agents.
    #[command(subcommand)]
    Skill(SkillCommand),
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

impl QueryArgs {
    pub fn mode(&self) -> memoire_api::QueryMode {
        if self.write {
            memoire_api::QueryMode::Write
        } else {
            memoire_api::QueryMode::Read
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum SnapshotsCommand {
    /// List a persistent database's snapshots, newest first.
    Ls {
        #[arg(value_parser = database_name)]
        db: String,
    },
    /// Replace a persistent database with one of its snapshots, after
    /// taking a `pre_restore` snapshot of the current contents.
    Restore {
        #[arg(value_parser = database_name)]
        db: String,
        /// The snapshot id (`snap_…`).
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
        #[arg(long, value_name = "DURATION", value_parser = key_lifetime, default_value = "90d")]
        expires: u64,
        /// Grant only `memoire.databases.read`.
        #[arg(long)]
        read_only: bool,
    },
}

#[derive(Debug, Args)]
pub struct MemoryArgs {
    /// The database holding the memory store.
    #[arg(long, global = true, value_name = "NAME", env = "MEMOIRE_MEMORY_DB", default_value = "memory", value_parser = database_name)]
    pub db: String,
    /// Who is writing, such as the agent's model; recorded with each write.
    #[arg(long, global = true, value_name = "NAME", env = "MEMOIRE_AGENT", value_parser = model::parse_agent)]
    pub agent: Option<String>,
    /// The agent's session or run; a confirmation counts only from another
    /// principal or session than the text's author.
    #[arg(long, global = true, value_name = "ID", env = "MEMOIRE_SESSION", value_parser = model::parse_session)]
    pub session: Option<String>,
    #[command(subcommand)]
    pub command: MemoryCommand,
}

#[derive(Debug, Subcommand)]
pub enum MemoryCommand {
    /// Create the database (persistent by default) and the store's tables,
    /// or bring an existing store to this version. Safe to repeat.
    Init(MemoryInitArgs),
    /// Record a new memory.
    Remember(RememberArgs),
    /// Show one memory, its confirmations and disputes.
    Show {
        #[arg(value_parser = model::parse_memory_id)]
        id: String,
        /// Every revision too, oldest first.
        #[arg(long)]
        history: bool,
    },
    /// Write a new revision of a memory: only the given fields change.
    Revise(ReviseArgs),
    /// Search memories, best first; without QUERY, the newest.
    Recall(RecallArgs),
    /// Confirm a memory's current text, with what checked it. Only counts
    /// from another principal or session than the text's author.
    Confirm {
        #[arg(value_parser = model::parse_memory_id)]
        id: String,
        /// What checked it: a command, a run, a path. Repeatable.
        #[arg(long, value_name = "TEXT", required = true)]
        evidence: Vec<String>,
        /// The revision you read (recall and show print `rev`). Refused
        /// when the text changed after it; default: the current one.
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(i64).range(1..))]
        rev: Option<i64>,
    },
    /// Dispute a memory: it stays in recall, flagged, until resolved.
    Dispute {
        #[arg(value_parser = model::parse_memory_id)]
        id: String,
        #[arg(long)]
        reason: String,
        /// Repeatable.
        #[arg(long, value_name = "TEXT")]
        evidence: Vec<String>,
        /// The revision you read (recall and show print `rev`). Refused
        /// when the text changed after it; default: the current one.
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(i64).range(1..))]
        rev: Option<i64>,
    },
    /// Close every open dispute of a memory.
    Resolve {
        #[arg(value_parser = model::parse_memory_id)]
        id: String,
        #[arg(long)]
        reason: String,
    },
    /// Replace a memory by another; the old one leaves recall.
    Supersede {
        #[arg(value_parser = model::parse_memory_id)]
        id: String,
        /// The memory that replaces it.
        #[arg(long, value_name = "NEW_ID", value_parser = model::parse_memory_id)]
        by: String,
        #[arg(long)]
        reason: String,
    },
    /// Forget a memory: a tombstone that recall hides. With --purge, erase
    /// it and its history, for a leak.
    Forget {
        #[arg(value_parser = model::parse_memory_id)]
        id: String,
        /// Why. Kept in the history, except with --purge, where it is
        /// checked but not stored.
        #[arg(long)]
        reason: String,
        /// Delete every row about it instead of keeping a tombstone, and
        /// what other memories' revisions and events say about linking to
        /// it. One `purge` event stays, without the reason.
        #[arg(long)]
        purge: bool,
    },
    /// What happened in the store, newest first.
    Log(LogArgs),
    /// Write the memories to a JSON Lines file, one memory per line.
    Export {
        /// Only this scope; repeatable.
        #[arg(long = "scope", value_name = "SCOPE", value_parser = model::parse_scope)]
        scopes: Vec<String>,
        #[arg(short, long, value_name = "FILE")]
        output: PathBuf,
    },
}

#[derive(Debug, Args)]
pub struct LogArgs {
    /// Only events this recent, such as 24h.
    #[arg(long, value_name = "DURATION", value_parser = parse_seconds, conflicts_with = "unseen")]
    pub since: Option<u64>,
    /// Only events this reader has not seen yet (on a first look, those of
    /// the last 24 hours), then remember them as seen.
    #[arg(long)]
    pub unseen: bool,
    /// Whose cursor `--unseen` follows: a role that runs as many sessions,
    /// such as a nightly reviewer. Without it, the session's, else the
    /// machine's. Ignored without `--unseen`.
    #[arg(long, value_name = "NAME", env = "MEMOIRE_READER", value_parser = reader_name)]
    pub reader: Option<String>,
    /// Only events of this scope; repeatable.
    #[arg(long = "scope", value_name = "SCOPE", value_parser = model::parse_scope)]
    pub scopes: Vec<String>,
    /// At most this many, 1 to 1000.
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(i64).range(1..=1000))]
    pub limit: i64,
}

#[derive(Debug, Subcommand)]
pub enum SkillCommand {
    /// Write the skill where Claude Code, Codex or another agent loads it.
    Install(SkillInstallArgs),
}

#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("targets").required(true).multiple(true).args(["claude", "codex", "dir"])))]
pub struct SkillInstallArgs {
    /// Into $CLAUDE_CONFIG_DIR/skills, else ~/.claude/skills.
    #[arg(long)]
    pub claude: bool,
    /// Into $CODEX_HOME/skills, else ~/.codex/skills.
    #[arg(long)]
    pub codex: bool,
    /// Into PATH/memoire/SKILL.md.
    #[arg(long, value_name = "PATH")]
    pub dir: Option<PathBuf>,
    /// Replace a different SKILL.md that is already there.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct MemoryInitArgs {
    /// Create a scratch database that expires when idle.
    #[arg(long, conflicts_with = "persistent")]
    pub burstable: bool,
    /// Idle TTL of a burstable database, such as 24h.
    #[arg(long, value_name = "DURATION", value_parser = parse_seconds, requires = "burstable")]
    pub ttl: Option<u64>,
    /// Create a database that lives until deleted (the default).
    #[arg(long)]
    pub persistent: bool,
}

#[derive(Debug, Args)]
pub struct RememberArgs {
    /// One line, at most 120 characters.
    #[arg(long)]
    pub title: String,
    /// At most 8 KiB.
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    /// Read the body from FILE; `-` reads standard input.
    #[arg(long, value_name = "FILE")]
    pub body_file: Option<PathBuf>,
    /// Such as fact (the default), decision, procedure, preference, pitfall
    /// or handoff. With --supersedes, the old memory's kind.
    #[arg(long, value_parser = model::parse_kind)]
    pub kind: Option<String>,
    /// Defaults to the git checkout's project scope, else `global`. With
    /// --supersedes, the old memory's scope.
    #[arg(long, value_parser = model::parse_scope)]
    pub scope: Option<String>,
    /// Repeatable; at most 10. With --supersedes, any --tag replaces the
    /// old memory's tags, which it keeps otherwise.
    #[arg(long = "tag", value_name = "TAG", value_parser = model::parse_tag)]
    pub tags: Vec<String>,
    /// Where this comes from: a path, a URL, a command. Repeatable.
    #[arg(long, value_name = "TEXT")]
    pub evidence: Vec<String>,
    /// Record it even when a memory in the scope has a similar title.
    #[arg(long)]
    pub force: bool,
    /// Replace this memory in the same write: it becomes superseded by the
    /// new one. Needs --reason.
    #[arg(long, value_name = "OLD_ID", value_parser = model::parse_memory_id, requires = "reason")]
    pub supersedes: Option<String>,
    /// Why the new memory replaces the old one, for both histories.
    #[arg(long, requires = "supersedes")]
    pub reason: Option<String>,
}

#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("fields").required(true).multiple(true).args([
    "title", "body", "body_file", "kind", "scope", "tags", "add_tags", "evidence", "add_evidence",
])))]
pub struct ReviseArgs {
    #[arg(value_parser = model::parse_memory_id)]
    pub id: String,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    /// Read the body from FILE; `-` reads standard input.
    #[arg(long, value_name = "FILE")]
    pub body_file: Option<PathBuf>,
    #[arg(long, value_parser = model::parse_kind)]
    pub kind: Option<String>,
    #[arg(long, value_parser = model::parse_scope)]
    pub scope: Option<String>,
    /// Replace the tags. Repeatable.
    #[arg(long = "tag", value_name = "TAG", value_parser = model::parse_tag, conflicts_with = "add_tags")]
    pub tags: Vec<String>,
    /// Add a tag. Repeatable.
    #[arg(long = "add-tag", value_name = "TAG", value_parser = model::parse_tag)]
    pub add_tags: Vec<String>,
    /// Replace the evidence. Repeatable.
    #[arg(long, value_name = "TEXT", conflicts_with = "add_evidence")]
    pub evidence: Vec<String>,
    /// Add an evidence item. Repeatable.
    #[arg(long, value_name = "TEXT")]
    pub add_evidence: Vec<String>,
    /// Why, for the history.
    #[arg(long)]
    pub reason: String,
}

#[derive(Debug, Args)]
pub struct RecallArgs {
    /// Words to search for; their order and any operators do not matter.
    pub query: Option<String>,
    /// Search this scope; repeatable. Default: the project's and `global`.
    #[arg(long = "scope", value_name = "SCOPE", value_parser = model::parse_scope, conflicts_with = "all_scopes")]
    pub scopes: Vec<String>,
    /// Search every scope.
    #[arg(long)]
    pub all_scopes: bool,
    /// Only this kind; repeatable, any of them.
    #[arg(long = "kind", value_name = "KIND", value_parser = model::parse_kind)]
    pub kinds: Vec<String>,
    /// Only memories with this tag; repeatable, all of them.
    #[arg(long = "tag", value_name = "TAG", value_parser = model::parse_tag)]
    pub tags: Vec<String>,
    /// Only this status; repeatable. Default: inferred, confirmed, disputed.
    #[arg(long = "status", value_name = "STATUS", value_enum)]
    pub statuses: Vec<model::Status>,
    /// At most this many, 1 to 100.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(i64).range(1..=100))]
    pub limit: i64,
}

/// Identity refuses keys that live longer than 365 days; refusing here
/// keeps a machine from being created without its key.
fn key_lifetime(text: &str) -> Result<u64, String> {
    let seconds = parse_seconds(text)?;
    if seconds > 365 * 86_400 {
        return Err("a key lives at most 365d".into());
    }
    Ok(seconds)
}

/// `--reader` / `MEMOIRE_READER`: the syntax of a tag. Empty is no
/// reader, so an exported empty `MEMOIRE_READER` is the same as unset.
fn reader_name(text: &str) -> Result<String, String> {
    if text.is_empty() {
        return Ok(String::new());
    }
    model::parse_tag(text).map_err(|_| {
        "a reader is 1-48 characters: a lowercase letter or digit, then lowercase letters, digits or . _ -".into()
    })
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
    fn keys_live_at_most_a_year() {
        assert!(parse(&["keys", "create", "--name", "ci", "--expires", "365d"]).is_ok());
        assert!(parse(&["keys", "create", "--name", "ci", "--expires", "366d"]).is_err());
    }

    fn memory(args: &[&str]) -> Result<MemoryArgs, clap::Error> {
        let mut all = vec!["memory"];
        all.extend_from_slice(args);
        match parse(&all)?.command {
            Command::Memory(args) => Ok(*args),
            other => panic!("{other:?}"),
        }
    }

    const ID: &str = "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8";

    #[test]
    fn memory_options_default_and_go_anywhere() {
        let args = memory(&["recall"]).unwrap();
        assert_eq!(args.db, "memory");
        assert_eq!((args.agent, args.session), (None, None));
        let MemoryCommand::Recall(recall) = args.command else {
            panic!("recall");
        };
        assert_eq!(recall.limit, 10);
        assert!(recall.query.is_none() && recall.statuses.is_empty());
        let args = memory(&[
            "show",
            ID,
            "--db",
            "notes",
            "--agent",
            "a1",
            "--session",
            "s1",
        ])
        .unwrap();
        assert_eq!(args.db, "notes");
        assert_eq!(args.agent.as_deref(), Some("a1"));
        assert_eq!(args.session.as_deref(), Some("s1"));
        assert!(memory(&["--db", "Bad_Name", "recall"]).is_err());
        assert!(memory(&["--agent", &"a".repeat(65), "recall"]).is_err());
        assert!(memory(&["--session", "s\u{7}", "recall"]).is_err());
    }

    #[test]
    fn memory_ids_are_checked_and_normalised() {
        let lower = format!("mem_{}", ID[4..].to_ascii_lowercase());
        let MemoryCommand::Show { id, history } =
            memory(&["show", &lower, "--history"]).unwrap().command
        else {
            panic!("show");
        };
        assert_eq!(id, ID);
        assert!(history);
        for bad in [
            "mem_x",
            "01JD0Q7ZQ5Y8W1N3K4M5P6R7S8",
            "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7SU",
        ] {
            assert!(memory(&["show", bad]).is_err(), "{bad}");
        }
    }

    #[test]
    fn remember_validates_fields_at_parse_time() {
        let MemoryCommand::Remember(args) = memory(&[
            "remember",
            "--title",
            "t",
            "--tag",
            "deploy",
            "--tag",
            "ci",
            "--evidence",
            "e",
        ])
        .unwrap()
        .command
        else {
            panic!("remember");
        };
        // No --kind is visible as such; the verb applies the default.
        assert_eq!(args.kind, None);
        assert_eq!(args.tags, ["deploy", "ci"]);
        assert_eq!(args.scope, None);
        assert!(!args.force);
        assert_eq!((args.supersedes, args.reason), (None, None));
        assert!(memory(&["remember"]).is_err());
        for bad in [
            &["--kind", "F"][..],
            &["--scope", "Project:x"],
            &["--tag", "No Tag"],
            &["--body", "b", "--body-file", "f"],
        ] {
            let mut args = vec!["remember", "--title", "t"];
            args.extend_from_slice(bad);
            assert!(memory(&args).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn remember_supersedes_needs_a_reason_and_the_reason_an_old_id() {
        let lower = format!("mem_{}", ID[4..].to_ascii_lowercase());
        let MemoryCommand::Remember(args) = memory(&[
            "remember",
            "--title",
            "t",
            "--supersedes",
            &lower,
            "--reason",
            "newer",
            "--force",
        ])
        .unwrap()
        .command
        else {
            panic!("remember");
        };
        assert_eq!(args.supersedes.as_deref(), Some(ID));
        assert_eq!(args.reason.as_deref(), Some("newer"));
        assert!(args.force);
        assert_eq!(args.kind, None);
        for bad in [
            &["--supersedes", ID][..],
            &["--reason", "newer"],
            &["--supersedes", "mem_x", "--reason", "newer"],
        ] {
            let mut args = vec!["remember", "--title", "t"];
            args.extend_from_slice(bad);
            assert!(memory(&args).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn revise_needs_a_field_and_a_reason() {
        assert!(memory(&["revise", ID, "--title", "t", "--reason", "r"]).is_ok());
        assert!(memory(&["revise", ID, "--add-tag", "x", "--reason", "r"]).is_ok());
        assert!(memory(&["revise", ID, "--reason", "r"]).is_err());
        assert!(memory(&["revise", ID, "--title", "t"]).is_err());
        assert!(
            memory(&[
                "revise",
                ID,
                "--tag",
                "a",
                "--add-tag",
                "b",
                "--reason",
                "r"
            ])
            .is_err()
        );
        assert!(
            memory(&[
                "revise",
                ID,
                "--evidence",
                "a",
                "--add-evidence",
                "b",
                "--reason",
                "r"
            ])
            .is_err()
        );
    }

    #[test]
    fn recall_limits_and_scopes() {
        let MemoryCommand::Recall(args) = memory(&[
            "recall",
            "vpn",
            "--scope",
            "global",
            "--scope",
            "run:1",
            "--status",
            "superseded",
            "--kind",
            "fact",
            "--limit",
            "100",
        ])
        .unwrap()
        .command
        else {
            panic!("recall");
        };
        assert_eq!(args.query.as_deref(), Some("vpn"));
        assert_eq!(args.scopes, ["global", "run:1"]);
        assert_eq!(args.statuses, [model::Status::Superseded]);
        assert_eq!(args.limit, 100);
        assert!(memory(&["recall", "--limit", "0"]).is_err());
        assert!(memory(&["recall", "--limit", "101"]).is_err());
        assert!(memory(&["recall", "--all-scopes", "--scope", "global"]).is_err());
        assert!(memory(&["recall", "--status", "gone"]).is_err());
    }

    #[test]
    fn memory_init_kinds() {
        let MemoryCommand::Init(args) = memory(&["init", "--burstable", "--ttl", "2h"])
            .unwrap()
            .command
        else {
            panic!("init");
        };
        assert!(args.burstable && !args.persistent);
        assert_eq!(args.ttl, Some(7_200));
        assert!(memory(&["init", "--ttl", "2h"]).is_err());
        assert!(memory(&["init", "--burstable", "--persistent"]).is_err());
    }

    #[test]
    fn confirm_needs_evidence_and_dispute_a_reason() {
        let MemoryCommand::Confirm { id, evidence, rev } =
            memory(&["confirm", ID, "--evidence", "ran it", "--evidence", "ci"])
                .unwrap()
                .command
        else {
            panic!("confirm");
        };
        assert_eq!(id, ID);
        assert_eq!(evidence, ["ran it", "ci"]);
        assert_eq!(rev, None);
        let MemoryCommand::Confirm { rev, .. } =
            memory(&["confirm", ID, "--evidence", "e", "--rev", "3"])
                .unwrap()
                .command
        else {
            panic!("confirm");
        };
        assert_eq!(rev, Some(3));
        let MemoryCommand::Dispute { rev, .. } =
            memory(&["dispute", ID, "--reason", "r", "--rev", "1"])
                .unwrap()
                .command
        else {
            panic!("dispute");
        };
        assert_eq!(rev, Some(1));
        for bad in ["0", "-1", "two"] {
            assert!(
                memory(&["confirm", ID, "--evidence", "e", "--rev", bad]).is_err(),
                "{bad}"
            );
            assert!(
                memory(&["dispute", ID, "--reason", "r", "--rev", bad]).is_err(),
                "{bad}"
            );
        }
        assert!(memory(&["confirm", ID]).is_err());
        assert!(memory(&["dispute", ID, "--evidence", "e"]).is_err());
        let MemoryCommand::Dispute { evidence, .. } = memory(&["dispute", ID, "--reason", "wrong"])
            .unwrap()
            .command
        else {
            panic!("dispute");
        };
        assert!(evidence.is_empty());
        assert!(memory(&["resolve", ID]).is_err());
        assert!(memory(&["resolve", ID, "--reason", "fixed"]).is_ok());
    }

    #[test]
    fn supersede_and_forget_take_ids_and_a_reason() {
        let other = "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S9";
        let lower = format!("mem_{}", other[4..].to_ascii_lowercase());
        let MemoryCommand::Supersede { by, .. } =
            memory(&["supersede", ID, "--by", &lower, "--reason", "newer"])
                .unwrap()
                .command
        else {
            panic!("supersede");
        };
        assert_eq!(by, other);
        assert!(memory(&["supersede", ID, "--by", "mem_x", "--reason", "r"]).is_err());
        assert!(memory(&["supersede", ID, "--reason", "r"]).is_err());
        let MemoryCommand::Forget { purge, .. } =
            memory(&["forget", ID, "--reason", "leak", "--purge"])
                .unwrap()
                .command
        else {
            panic!("forget");
        };
        assert!(purge);
        assert!(memory(&["forget", ID]).is_err());
    }

    #[test]
    fn log_and_export_options() {
        let MemoryCommand::Log(args) = memory(&["log"]).unwrap().command else {
            panic!("log");
        };
        assert_eq!((args.limit, args.since, args.unseen), (50, None, false));
        assert_eq!(args.reader, None);
        let MemoryCommand::Log(args) = memory(&[
            "log", "--since", "24h", "--scope", "global", "--limit", "1000",
        ])
        .unwrap()
        .command
        else {
            panic!("log");
        };
        assert_eq!(args.since, Some(86_400));
        assert_eq!(args.scopes, ["global"]);
        assert!(memory(&["log", "--since", "1h", "--unseen"]).is_err());
        assert!(memory(&["log", "--limit", "1001"]).is_err());
        assert!(memory(&["log", "--limit", "0"]).is_err());
        let MemoryCommand::Log(args) = memory(&["log", "--unseen", "--reader", "nightly.review-2"])
            .unwrap()
            .command
        else {
            panic!("log");
        };
        assert!(args.unseen);
        assert_eq!(args.reader.as_deref(), Some("nightly.review-2"));
        // Only --unseen reads it, but it does not refuse a fixed window.
        let MemoryCommand::Log(args) = memory(&["log", "--since", "24h", "--reader", "nightly"])
            .unwrap()
            .command
        else {
            panic!("log");
        };
        assert_eq!(
            (args.since, args.reader.as_deref()),
            (Some(86_400), Some("nightly"))
        );
        let MemoryCommand::Log(args) = memory(&["log", "--unseen", "--reader="]).unwrap().command
        else {
            panic!("log");
        };
        assert_eq!(args.reader.as_deref(), Some(""));
        for bad in ["Nightly", "-x", "night ly", &"n".repeat(49)] {
            let error = memory(&["log", "--unseen", &format!("--reader={bad}")]).unwrap_err();
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{bad}"
            );
            assert!(error.to_string().contains("a reader is"), "{error}");
        }
        assert!(memory(&["log", "--unseen", "--reader", &"n".repeat(48)]).is_ok());
        assert!(memory(&["recall", "--reader", "nightly"]).is_err());
        let MemoryCommand::Export { output, scopes } =
            memory(&["export", "-o", "out.jsonl", "--scope", "run:1"])
                .unwrap()
                .command
        else {
            panic!("export");
        };
        assert_eq!(output, PathBuf::from("out.jsonl"));
        assert_eq!(scopes, ["run:1"]);
        assert!(memory(&["export"]).is_err());
    }

    #[test]
    fn skill_install_needs_a_target() {
        let Command::Skill(SkillCommand::Install(args)) =
            parse(&["skill", "install", "--claude", "--dir", "x", "--force"])
                .unwrap()
                .command
        else {
            panic!("skill install");
        };
        assert!(args.claude && !args.codex && args.force);
        assert_eq!(args.dir, Some(PathBuf::from("x")));
        assert!(parse(&["skill", "install"]).is_err());
        assert!(parse(&["skill", "install", "--force"]).is_err());
        assert!(parse(&["skill", "install", "--codex"]).is_ok());
    }

    #[test]
    fn login_takes_no_secret_argument() {
        assert!(parse(&["login", "--server", "https://m.example", "--key", "mzk_x"]).is_err());
    }
}
