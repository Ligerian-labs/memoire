//! The memoire memory engine: a shared memory for a fleet of agents, kept
//! as versioned SQL in an ordinary memoire database.
//!
//! A memory has a stable id and an append-only history of revisions. The
//! store is a set of `memoire_memory_*` tables that [`schema`] creates and
//! migrates, and every verb is a read batch, then at most one write batch
//! that applies whole or not at all.
//!
//! The engine reaches no network and no file: it reads the clock and the
//! operating system's randomness for new ids, builds the batches and runs
//! them through a [`Backend`], which runs each one as a transaction (the
//! CLI over the gateway's query route). The verbs are `async fn`s generic
//! over the backend; they return values, and the caller prints them.
pub mod history;
pub mod model;
mod refusal;
pub mod retire;
pub mod schema;
pub mod scope;
pub mod search;
pub mod secrets;
pub mod store;
pub mod trust;
pub mod verbs;

use memoire_api::{QueryMode, StatementResult};
use model::Author;
pub use refusal::{Candidate, Invalid, Refusal, RefusalClass};
use std::future::Future;
use store::Batch;

/// Where the engine runs its batches.
pub trait Backend {
    type Error: BackendError;

    /// Runs `batch` as one transaction in `mode`; one result per statement.
    fn run(
        &self,
        mode: QueryMode,
        batch: Batch,
    ) -> impl Future<Output = Result<Vec<StatementResult>, Self::Error>>;

    /// The authenticated principal that writes are recorded under. Only
    /// verbs that write ask for it, after the checks that can refuse
    /// without writing.
    fn principal(&self) -> impl Future<Output = Result<String, Self::Error>>;
}

/// What the engine needs to know of a backend's failure.
pub trait BackendError {
    /// The statement SQLite refused, when the failure is an `sql_error`.
    fn sql(&self) -> Option<SqlFailure<'_>>;
}

/// A statement SQLite refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SqlFailure<'a> {
    /// Its index in the batch.
    pub statement: usize,
    /// SQLite's code, such as `SQLITE_CONSTRAINT_PRIMARYKEY`.
    pub code: &'a str,
    /// SQLite's message; the code when the message did not travel.
    pub message: &'a str,
}

/// A failed verb.
#[derive(Debug)]
pub enum Error<E> {
    /// The memory refused it.
    Refused(Refusal),
    /// The backend failed.
    Backend(E),
    /// The backend's answer lacks what the engine reads in it: the text
    /// names what, as in "a result per statement".
    InvalidResponse(String),
    /// The operating system gave no randomness for an id; its error.
    NoRandomness(String),
}

impl<E> From<Refusal> for Error<E> {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

impl<E: BackendError> Error<E> {
    /// The statement an `sql_error` names, and its SQLite code and message.
    pub fn sql(&self) -> Option<SqlFailure<'_>> {
        match self {
            Self::Backend(e) => e.sql(),
            _ => None,
        }
    }
}

/// The client-reported author fields every write records: `--agent` and
/// `--session` on the CLI, empty when not given.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Labels {
    pub agent: String,
    pub session: String,
}

impl Labels {
    /// The agent and session, which every write stores, for the secret
    /// guard.
    pub fn fields(&self) -> [(&'static str, &str); 2] {
        [("agent", &self.agent), ("session", &self.session)]
    }

    /// The writer: the backend's principal, never a client-reported one,
    /// with the agent and session it gave.
    pub async fn actor<B: Backend>(&self, backend: &B) -> Result<Author, Error<B::Error>> {
        Ok(Author {
            principal: backend.principal().await.map_err(Error::Backend)?,
            agent: self.agent.clone(),
            session: self.session.clone(),
        })
    }
}
