//! The memory engine over the gateway's query route (the SQL path): the
//! backend it runs its batches through, the executor that drives its
//! verbs, and the errors of either path as the CLI reports them.
use crate::client::Client;
use crate::error::{Error, Result, exit};
use memoire_api::{QueryMode, QueryRequest, StatementResult};
use memoire_memory::store::Batch;
use memoire_memory::{Backend, BackendError, Refusal, RefusalClass, SqlFailure};
use std::pin::pin;
use std::task::{Context, Poll, Waker};

/// The store in one database, reached over the gateway's query route.
pub struct Gateway {
    pub client: Client,
    pub db: String,
}

impl Backend for Gateway {
    type Error = Error;

    /// Sends `batch` as one transaction. The client blocks, so the future
    /// is ready when first polled.
    async fn run(&self, mode: QueryMode, batch: Batch) -> Result<Vec<StatementResult>> {
        let (statements, timeout_ms) = batch.into_parts();
        let response = self
            .client
            .query(
                &self.db,
                &QueryRequest {
                    mode,
                    transaction: true,
                    timeout_ms,
                    statements,
                },
            )
            .map_err(|e| self.not_found_hint(e))?;
        Ok(response.results)
    }

    /// The credential's principal from `whoami`, never from a flag.
    async fn principal(&self) -> Result<String> {
        Ok(self.client.whoami()?.subject)
    }
}

impl Gateway {
    /// A `database_not_found` with the hint to run `init`.
    pub fn not_found_hint(&self, error: Error) -> Error {
        if error.code() != "database_not_found" {
            return error;
        }
        Error::local(
            "database_not_found",
            exit::NOT_FOUND,
            format!(
                "no database `{}` in the team; run `memoire memory init`",
                self.db
            ),
        )
        .with("status", 404)
    }

    /// An engine failure as the CLI reports it.
    pub fn error(&self, error: memoire_memory::Error<Error>) -> Error {
        match error {
            memoire_memory::Error::Refused(refusal) => self.refused(refusal),
            memoire_memory::Error::Backend(e) => e,
            memoire_memory::Error::InvalidResponse(what) => Error::local(
                "invalid_response",
                exit::FAILURE,
                format!("the gateway's answer for the memory store lacks {what}"),
            ),
            memoire_memory::Error::NoRandomness(e) => Error::local(
                "random_failed",
                exit::FAILURE,
                format!("no randomness from the operating system: {e}"),
            ),
        }
    }

    /// A refusal as a local failure: its code, the exit code of its class,
    /// its message and its members.
    pub fn refused(&self, refusal: Refusal) -> Error {
        let exit = match refusal.class() {
            RefusalClass::NotFound => exit::NOT_FOUND,
            RefusalClass::Conflict => exit::CONFLICT,
            RefusalClass::Rejected => exit::REJECTED,
            RefusalClass::Usage => exit::USAGE,
        };
        let message = match refusal {
            Refusal::NotInitialized => format!(
                "the database `{}` holds no memory store; run `memoire memory init`",
                self.db
            ),
            // A key that may not read is not told which memory it is.
            Refusal::Duplicate {
                ref scope,
                ref candidates,
            } if candidates.is_empty() => format!(
                "a memory in {scope} has a similar title; pass --force to record this one too"
            ),
            _ => refusal.message(),
        };
        Error::Local {
            code: refusal.code(),
            message,
            exit,
            extra: refusal.data(),
        }
    }
}

impl BackendError for Error {
    fn sql(&self) -> Option<SqlFailure<'_>> {
        match self {
            Error::Api { sql: Some(sql), .. } => Some(SqlFailure {
                statement: sql.statement,
                code: &sql.sqlite_code,
                message: &sql.message,
            }),
            _ => None,
        }
    }
}

/// Runs an engine future to completion. Its backend never yields (the
/// client blocks), so it is ready on the first poll.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("a memory verb waited on its backend, which never yields"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_on_returns_what_a_ready_future_gives() {
        assert_eq!(block_on(async { 42 }), 42);
        let ready = |n| async move { n };
        assert_eq!(block_on(async { ready(1).await + ready(2).await }), 3);
    }

    #[test]
    #[should_panic(expected = "never yields")]
    fn block_on_refuses_a_future_that_yields() {
        block_on(std::future::pending::<()>());
    }
}
