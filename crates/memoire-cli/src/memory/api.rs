//! The memory verbs over the gateway's memory routes (docs/api.md,
//! "Memory routes"). The gateway runs the engine; this side sends the
//! verb's input and turns the answer back into the engine's values, and a
//! refusal back into the engine's [`Refusal`], so the CLI reports it as it
//! reports one the engine made here.
use super::backend::Gateway;
use crate::error::Error;
use memoire_api::memory::{
    ConfirmRequest, DisputeRequest, ForgetRequest, Labels as WireLabels, Memory, Purged,
    RememberRequest, ResolveRequest, ReviseRequest, SupersedeRequest,
};
use memoire_memory::history::{ExportPage, Unseen};
use memoire_memory::model::Event;
use memoire_memory::store::Cursor;
use memoire_memory::verbs::{Recall, Remember, Revise, Shown};
use memoire_memory::{Labels, Refusal};
use serde_json::{Map, Value};

/// What a verb returns, or how it failed, on either path.
pub type Verb<T> = Result<T, memoire_memory::Error<Error>>;

/// The refusal a memory route answered with `code`, its first `details`
/// entry (path and code) and `data`; `None` when they make none.
fn rebuild(code: &str, detail: Option<(&str, &str)>, data: &Map<String, Value>) -> Option<Refusal> {
    Refusal::from_wire(code, detail, data)
}

/// The store of one database, through its memory routes.
pub struct Api<'a> {
    pub gateway: &'a Gateway,
}

impl Api<'_> {
    /// A route's answer as a verb's: a memory refusal rebuilt, any other
    /// error as the query route's would be reported.
    fn answer<T>(&self, result: crate::error::Result<T>) -> Verb<T> {
        result.map_err(|e| {
            if let Error::Api { error, data, .. } = &e {
                let detail = error
                    .details
                    .first()
                    .map(|d| (d.path.as_str(), d.code.as_str()));
                if let Some(refusal) = rebuild(&error.code, detail, data) {
                    return memoire_memory::Error::Refused(refusal);
                }
            }
            memoire_memory::Error::Backend(self.gateway.not_found_hint(e))
        })
    }

    fn db(&self) -> &str {
        &self.gateway.db
    }

    /// `GET …/memory`, then `POST …/memory/init` unless the store is at
    /// the gateway's version: the version the store had and its id. A
    /// current store is not written to, so a read-only key can run this.
    pub fn init(&self) -> Verb<(i64, String)> {
        let client = &self.gateway.client;
        match self.answer(client.memory_store(self.db())) {
            Ok(store) => Ok((store.schema_version, store.store_id)),
            Err(memoire_memory::Error::Refused(
                Refusal::NotInitialized | Refusal::SchemaOlder { .. },
            )) => {
                let done = self.answer(client.memory_init(self.db()))?;
                Ok((done.migrated_from, done.store_id))
            }
            Err(e) => Err(e),
        }
    }

    /// `remember`; `scope` is the one to send: `--scope`, else the default
    /// the caller chose, else none (the gateway's default).
    pub fn remember(
        &self,
        labels: &Labels,
        input: Remember,
        scope: Option<String>,
    ) -> Verb<Memory> {
        let body = RememberRequest {
            title: input.title,
            body: (!input.body.is_empty()).then_some(input.body),
            kind: input.kind,
            scope,
            tags: input.tags,
            evidence: input.evidence,
            force: input.force,
            supersedes: input.supersedes,
            reason: input.reason,
            labels: wire(labels),
        };
        self.answer(self.gateway.client.remember(self.db(), &body))
    }

    pub fn show(&self, id: &str, history: bool) -> Verb<Shown> {
        let shown = self.answer(self.gateway.client.show(self.db(), id, history))?;
        Ok(Shown {
            memory: shown.memory,
            confirmations: shown.confirmations,
            disputes: shown.disputes,
            revisions: shown.revisions,
        })
    }

    pub fn revise(&self, labels: &Labels, input: Revise) -> Verb<Memory> {
        let body = ReviseRequest {
            reason: input.reason,
            title: input.title,
            body: input.body,
            kind: input.kind,
            scope: input.scope,
            tags: input.tags,
            add_tags: input.add_tags,
            evidence: input.evidence,
            add_evidence: input.add_evidence,
            labels: wire(labels),
        };
        self.answer(self.gateway.client.revise(self.db(), &input.id, &body))
    }

    pub fn recall(&self, args: Recall<'_>) -> Verb<Vec<(Memory, Option<f64>)>> {
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(text) = args.query {
            query.push(("query", text.to_owned()));
        }
        push_all(&mut query, "scope", args.scopes.unwrap_or_default());
        push_all(&mut query, "kind", args.kinds);
        push_all(&mut query, "tag", args.tags);
        for status in args.statuses {
            query.push(("status", status.as_str().to_owned()));
        }
        query.push(("limit", args.limit.to_string()));
        let found = self.answer(self.gateway.client.recall(self.db(), &query))?;
        Ok(found
            .memories
            .into_iter()
            .map(|r| (r.memory, r.score))
            .collect())
    }

    pub fn confirm(
        &self,
        labels: &Labels,
        id: &str,
        evidence: Vec<String>,
        rev: Option<i64>,
    ) -> Verb<Memory> {
        let body = ConfirmRequest {
            evidence,
            rev,
            labels: wire(labels),
        };
        self.answer(self.gateway.client.confirm(self.db(), id, &body))
    }

    pub fn dispute(
        &self,
        labels: &Labels,
        id: &str,
        reason: &str,
        evidence: Vec<String>,
        rev: Option<i64>,
    ) -> Verb<Memory> {
        let body = DisputeRequest {
            reason: reason.to_owned(),
            evidence,
            rev,
            labels: wire(labels),
        };
        self.answer(self.gateway.client.dispute(self.db(), id, &body))
    }

    pub fn resolve(&self, labels: &Labels, id: &str, reason: &str) -> Verb<Memory> {
        let body = ResolveRequest {
            reason: reason.to_owned(),
            labels: wire(labels),
        };
        self.answer(self.gateway.client.resolve(self.db(), id, &body))
    }

    pub fn supersede(
        &self,
        labels: &Labels,
        id: &str,
        by: &str,
        reason: &str,
    ) -> Verb<(Memory, Memory)> {
        let body = SupersedeRequest {
            by: by.to_owned(),
            reason: reason.to_owned(),
            labels: wire(labels),
        };
        let pair = self.answer(self.gateway.client.supersede(self.db(), id, &body))?;
        Ok((pair.superseded, pair.by))
    }

    pub fn forget(&self, labels: &Labels, id: &str, reason: &str) -> Verb<Memory> {
        let body = ForgetRequest {
            reason: reason.to_owned(),
            purge: false,
            labels: wire(labels),
        };
        self.answer(self.gateway.client.forget(self.db(), id, &body))
    }

    pub fn purge(&self, labels: &Labels, id: &str, reason: &str) -> Verb<()> {
        let body = ForgetRequest {
            reason: reason.to_owned(),
            purge: true,
            labels: wire(labels),
        };
        let _: Purged = self.answer(self.gateway.client.forget(self.db(), id, &body))?;
        Ok(())
    }

    pub fn log(
        &self,
        scopes: Option<&[String]>,
        since_seconds: Option<u64>,
        limit: i64,
    ) -> Verb<Vec<Event>> {
        let mut query = Vec::new();
        push_all(&mut query, "scope", scopes.unwrap_or_default());
        if let Some(seconds) = since_seconds {
            query.push(("since_seconds", seconds.to_string()));
        }
        query.push(("limit", limit.to_string()));
        Ok(self
            .answer(self.gateway.client.events(self.db(), &query))?
            .events)
    }

    /// `log --unseen`: the events after `saved`, or a first look; what
    /// they make of the cursor is the engine's rule ([`Unseen::of`]).
    pub fn unseen(
        &self,
        scopes: Option<&[String]>,
        saved: Option<&Cursor>,
        limit: i64,
    ) -> Verb<Unseen> {
        let mut query = Vec::new();
        push_all(&mut query, "scope", scopes.unwrap_or_default());
        match saved {
            Some(cursor) => {
                query.push(("after", cursor.seq.to_string()));
                query.push(("store_id", cursor.store_id.clone()));
            }
            None => query.push(("first_look", "true".to_owned())),
        }
        query.push(("limit", limit.to_string()));
        let page = self.answer(self.gateway.client.events(self.db(), &query))?;
        Ok(Unseen::of(
            memoire_memory::history::EventsPage {
                events: page.events,
                store_id: page.store_id,
                after: page.after,
                top: page.top,
                matching: page.matching,
            },
            saved,
        ))
    }

    pub fn export_page(&self, scopes: Option<&[String]>, after: i64) -> Verb<ExportPage> {
        let mut query = Vec::new();
        push_all(&mut query, "scope", scopes.unwrap_or_default());
        query.push(("after", after.to_string()));
        let page = self.answer(self.gateway.client.memory_export(self.db(), &query))?;
        Ok(ExportPage {
            memories: page.memories,
            next: page.next_after,
        })
    }
}

fn push_all<'a>(query: &mut Vec<(&'a str, String)>, name: &'a str, values: &[String]) {
    query.extend(values.iter().map(|v| (name, v.clone())));
}

/// The labels as a body carries them: an empty one is left out.
fn wire(labels: &Labels) -> WireLabels {
    let given = |text: &str| (!text.is_empty()).then(|| text.to_owned());
    WireLabels {
        agent: given(&labels.agent),
        session: given(&labels.session),
    }
}
