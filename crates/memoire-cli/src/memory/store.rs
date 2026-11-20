//! Which way a verb reaches the store: the gateway's memory API, or the
//! engine here over the query route (the SQL convention) when the gateway
//! has no memory routes or `MEMOIRE_MEMORY_SQL=1` asks for it. Both return
//! the same values and fail with the same errors, so the caller prints
//! without knowing which ran.
use super::api::{Api, Verb};
use super::backend::{Gateway, block_on};
use crate::error::{Error, Result};
use memoire_api::{QueryMode, StatementResult};
use memoire_memory::history::{self, ExportPage, Unseen};
use memoire_memory::model::{Event, Memory};
use memoire_memory::store::{Batch, Cursor};
use memoire_memory::verbs::{self, Recall, Remember, Revise, Shown};
use memoire_memory::{Backend, BackendError, Labels, SqlFailure, retire, schema, trust};
use std::cell::Cell;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Path {
    /// The API, until the first call says whether the gateway has it.
    Ask,
    Api,
    Sql,
}

/// The store in one database, and the path this command takes to it.
pub struct Store {
    pub gateway: Gateway,
    path: Cell<Path>,
}

/// Whether the gateway answered that it has no such route: an older
/// gateway, without the memory API.
fn no_route(error: &memoire_memory::Error<Error>) -> bool {
    matches!(error, memoire_memory::Error::Backend(Error::Api { status: 404, error, .. })
        if error.code == "not_found")
}

impl Store {
    /// `sql`: take the SQL path without asking.
    pub fn new(gateway: Gateway, sql: bool) -> Self {
        Self {
            gateway,
            path: Cell::new(if sql { Path::Sql } else { Path::Ask }),
        }
    }

    /// Runs one verb: `api` through the memory API, unless this command
    /// takes the SQL path; `sql`, the engine over the query route, when it
    /// does, or when the command's first API call finds no route (nothing
    /// was written then).
    fn either<T, F>(
        &self,
        api: impl FnOnce(&Api<'_>) -> Verb<T>,
        sql: impl FnOnce() -> F,
    ) -> Result<T>
    where
        F: Future<Output = Verb<T>>,
    {
        let result = if self.path.get() == Path::Sql {
            block_on(sql())
        } else {
            match api(&Api {
                gateway: &self.gateway,
            }) {
                Err(e) if self.path.get() == Path::Ask && no_route(&e) => {
                    self.path.set(Path::Sql);
                    block_on(sql())
                }
                result => {
                    self.path.set(Path::Api);
                    result
                }
            }
        };
        result.map_err(|e| self.gateway.error(e))
    }

    /// `init` on a database that exists: the version the store had and its
    /// id.
    pub fn init(&self) -> Result<(i64, String)> {
        self.either(
            |api| api.init(),
            || async {
                let from = schema::migrate(&self.gateway).await?;
                Ok((from, schema::store_id(&self.gateway).await?))
            },
        )
    }

    /// `remember`; `default_scope` is asked only when the memory gets no
    /// scope otherwise, on the API path when neither `--scope` nor
    /// `--supersedes` is given.
    pub fn remember(
        &self,
        labels: &Labels,
        input: Remember,
        default_scope: fn() -> Option<String>,
    ) -> Result<Memory> {
        self.either(
            |api| {
                local(verbs::remember(&Local, labels, input.clone(), || None))?;
                let scope = match (&input.scope, &input.supersedes) {
                    (Some(scope), _) => Some(scope.clone()),
                    (None, None) => default_scope(),
                    (None, Some(_)) => None,
                };
                api.remember(labels, input.clone(), scope)
            },
            || verbs::remember(&self.gateway, labels, input.clone(), default_scope),
        )
    }

    pub fn show(&self, id: &str, history: bool) -> Result<Shown> {
        self.either(
            |api| {
                local(verbs::show(&Local, id, history))?;
                api.show(id, history)
            },
            || verbs::show(&self.gateway, id, history),
        )
    }

    pub fn revise(&self, labels: &Labels, input: Revise) -> Result<Memory> {
        self.either(
            |api| {
                local(verbs::revise(&Local, labels, input.clone()))?;
                api.revise(labels, input.clone())
            },
            || verbs::revise(&self.gateway, labels, input.clone()),
        )
    }

    pub fn recall(&self, args: Recall<'_>) -> Result<Vec<(Memory, Option<f64>)>> {
        self.either(
            |api| {
                local(verbs::recall(&Local, args))?;
                api.recall(args)
            },
            || verbs::recall(&self.gateway, args),
        )
    }

    pub fn confirm(
        &self,
        labels: &Labels,
        id: &str,
        evidence: Vec<String>,
        rev: Option<i64>,
    ) -> Result<Memory> {
        self.either(
            |api| {
                local(trust::confirm(&Local, labels, id, evidence.clone(), rev))?;
                api.confirm(labels, id, evidence.clone(), rev)
            },
            || trust::confirm(&self.gateway, labels, id, evidence.clone(), rev),
        )
    }

    pub fn dispute(
        &self,
        labels: &Labels,
        id: &str,
        reason: &str,
        evidence: Vec<String>,
        rev: Option<i64>,
    ) -> Result<Memory> {
        self.either(
            |api| {
                local(trust::dispute(
                    &Local,
                    labels,
                    id,
                    reason,
                    evidence.clone(),
                    rev,
                ))?;
                api.dispute(labels, id, reason, evidence.clone(), rev)
            },
            || trust::dispute(&self.gateway, labels, id, reason, evidence.clone(), rev),
        )
    }

    pub fn resolve(&self, labels: &Labels, id: &str, reason: &str) -> Result<Memory> {
        self.either(
            |api| {
                local(trust::resolve(&Local, labels, id, reason))?;
                api.resolve(labels, id, reason)
            },
            || trust::resolve(&self.gateway, labels, id, reason),
        )
    }

    pub fn supersede(
        &self,
        labels: &Labels,
        id: &str,
        by: &str,
        reason: &str,
    ) -> Result<(Memory, Memory)> {
        self.either(
            |api| {
                local(retire::supersede(&Local, labels, id, by, reason))?;
                api.supersede(labels, id, by, reason)
            },
            || retire::supersede(&self.gateway, labels, id, by, reason),
        )
    }

    pub fn forget(&self, labels: &Labels, id: &str, reason: &str) -> Result<Memory> {
        self.either(
            |api| {
                local(retire::forget(&Local, labels, id, reason))?;
                api.forget(labels, id, reason)
            },
            || retire::forget(&self.gateway, labels, id, reason),
        )
    }

    pub fn purge(&self, labels: &Labels, id: &str, reason: &str) -> Result<()> {
        self.either(
            |api| {
                local(retire::purge(&Local, labels, id, reason))?;
                api.purge(labels, id, reason)
            },
            || retire::purge(&self.gateway, labels, id, reason),
        )
    }

    pub fn log(
        &self,
        scopes: Option<&[String]>,
        since_seconds: Option<u64>,
        limit: i64,
    ) -> Result<Vec<Event>> {
        self.either(
            |api| {
                local(history::log(&Local, scopes, since_seconds, limit))?;
                api.log(scopes, since_seconds, limit)
            },
            || history::log(&self.gateway, scopes, since_seconds, limit),
        )
    }

    pub fn unseen(
        &self,
        scopes: Option<&[String]>,
        saved: Option<&Cursor>,
        limit: i64,
    ) -> Result<Unseen> {
        self.either(
            |api| {
                local(history::unseen(&Local, scopes, saved, limit))?;
                api.unseen(scopes, saved, limit)
            },
            || history::unseen(&self.gateway, scopes, saved, limit),
        )
    }

    pub fn export_page(&self, scopes: Option<&[String]>, after: i64) -> Result<ExportPage> {
        self.either(
            |api| {
                local(history::export_page(&Local, scopes, after))?;
                api.export_page(scopes, after)
            },
            || history::export_page(&self.gateway, scopes, after),
        )
    }
}

/// A backend that stops a verb at its first read: running a verb on it
/// runs every check the engine makes of the input before it reads, in the
/// engine's order, and sends nothing.
struct Local;

/// A verb that passed its input checks.
#[derive(Debug)]
struct Checked;

impl BackendError for Checked {
    fn sql(&self) -> Option<SqlFailure<'_>> {
        None
    }
}

impl Backend for Local {
    type Error = Checked;

    async fn run(&self, _: QueryMode, _: Batch) -> Result<Vec<StatementResult>, Checked> {
        Err(Checked)
    }

    async fn principal(&self) -> Result<String, Checked> {
        Err(Checked)
    }
}

/// Runs a verb on [`Local`]: its refusal of the input, if any. A malformed
/// input is reported as the SQL path reports it, and an input the guards
/// refuse (a secret, a field too large) never leaves this machine.
fn local<T>(verb: impl Future<Output = Result<T, memoire_memory::Error<Checked>>>) -> Verb<()> {
    match block_on(verb) {
        Err(memoire_memory::Error::Refused(refusal)) => Err(refusal.into()),
        _ => Ok(()),
    }
}
