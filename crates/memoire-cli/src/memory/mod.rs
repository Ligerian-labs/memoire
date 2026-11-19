//! `memoire memory`: a shared memory for a fleet of agents, kept as
//! versioned SQL in an ordinary memoire database.
//!
//! A memory has a stable id and an append-only history of revisions. It
//! travels over the gateway's query route only: the store is a set of
//! `memoire_memory_*` tables that [`schema`] creates and migrates, and
//! every verb is a read batch, then at most one write batch that applies
//! whole or not at all.
pub mod cursor;
pub mod history;
pub mod model;
pub mod render;
pub mod retire;
pub mod schema;
pub mod scope;
pub mod search;
pub mod secrets;
pub mod store;
pub mod trust;
pub mod verbs;

use crate::cli::{MemoryArgs, MemoryCommand};
use crate::client::Client;
use crate::config::Env;
use crate::error::Result;
use crate::output::Out;
use model::Author;
use store::Store;

/// What every verb works with.
pub struct Context<'a> {
    pub out: &'a Out,
    /// Where this machine keeps its files (the `log --unseen` cursors).
    pub env: &'a Env,
    pub client: Client,
    pub db: String,
    /// `--agent`, recorded with writes; empty when not given.
    pub agent: String,
    /// `--session`, recorded with writes; empty when not given.
    pub session: String,
}

impl Context<'_> {
    pub fn store(&self) -> Store<'_> {
        Store {
            client: &self.client,
            db: &self.db,
        }
    }

    /// The writer: the credential's principal from `whoami`, never from a
    /// flag, with the agent and session it gave.
    pub fn actor(&self) -> Result<Author> {
        Ok(Author {
            principal: self.client.whoami()?.subject,
            agent: self.agent.clone(),
            session: self.session.clone(),
        })
    }

    /// The agent and session, which every write stores, for the secret
    /// guard.
    pub fn labels(&self) -> [(&'static str, &str); 2] {
        [("agent", &self.agent), ("session", &self.session)]
    }
}

pub fn run(out: &Out, env: &Env, args: MemoryArgs) -> Result<()> {
    let (server, key) = env.login()?;
    let ctx = Context {
        out,
        env,
        client: Client::new(&server, Some(&key))?,
        db: args.db,
        agent: args.agent.unwrap_or_default(),
        session: args.session.unwrap_or_default(),
    };
    match args.command {
        MemoryCommand::Init(args) => verbs::init(&ctx, &args),
        MemoryCommand::Remember(args) => verbs::remember(&ctx, args),
        MemoryCommand::Show { id, history } => verbs::show(&ctx, &id, history),
        MemoryCommand::Revise(args) => verbs::revise(&ctx, args),
        MemoryCommand::Recall(args) => verbs::recall(&ctx, &args),
        MemoryCommand::Confirm { id, evidence, rev } => trust::confirm(&ctx, &id, evidence, rev),
        MemoryCommand::Dispute {
            id,
            reason,
            evidence,
            rev,
        } => trust::dispute(&ctx, &id, &reason, evidence, rev),
        MemoryCommand::Resolve { id, reason } => trust::resolve(&ctx, &id, &reason),
        MemoryCommand::Supersede { id, by, reason } => retire::supersede(&ctx, &id, &by, &reason),
        MemoryCommand::Forget { id, reason, purge } => {
            if purge {
                retire::purge(&ctx, &id, &reason)
            } else {
                retire::forget(&ctx, &id, &reason)
            }
        }
        MemoryCommand::Log(args) => history::log(&ctx, &args),
        MemoryCommand::Export { scopes, output } => history::export(&ctx, &scopes, &output),
    }
}
