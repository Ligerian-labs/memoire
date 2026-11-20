//! One function per `memoire memory` verb.
//!
//! A verb reads what it needs (the version check first), runs the guards,
//! then sends at most one write batch. A write that a concurrent writer
//! made stale fails its guard and is refused with `memory_conflict`; the
//! agent re-reads and retries. The engine itself takes no lock and never
//! retries.
use crate::model::{self, Confirmation, Dispute, Memory, Revision, Status};
use crate::search::{self, Filter};
use crate::store::{self, Batch, Lacks, NewRevision, RevisionStatus, Store};
use crate::{Backend, Candidate, Error, Labels, Refusal};

pub(crate) fn not_found(id: &str, purged_at: Option<String>) -> Refusal {
    Refusal::NotFound {
        id: id.to_owned(),
        purged_at,
    }
}

/// The memory `id` as a read finds it, with the time it was purged when
/// it is gone.
pub(crate) async fn read_memory<B: Backend>(
    backend: &B,
    id: &str,
) -> Result<Memory, Error<B::Error>> {
    let mut batch = Batch::new();
    let memory = store::select_memory(&mut batch, id);
    let purge = store::select_purge(&mut batch, id);
    let results = Store { backend }.read(batch).await?;
    match store::one_memory(&results[memory])? {
        Some(memory) => Ok(memory),
        None => Err(not_found(id, purged_at(&results[purge])?).into()),
    }
}

pub(crate) fn purged_at(result: &memoire_api::StatementResult) -> Result<Option<String>, Lacks> {
    store::rows(result)
        .next()
        .map(|row| row.text("created_at"))
        .transpose()
}

/// The memory a write batch selected last.
pub(crate) fn written(results: &[memoire_api::StatementResult]) -> Result<Memory, Lacks> {
    results
        .last()
        .map(store::one_memory)
        .transpose()?
        .flatten()
        .ok_or_else(|| store::invalid_response("the written memory"))
}

/// A refusal on a memory that was superseded or forgotten.
pub(crate) fn not_current(memory: &Memory) -> Refusal {
    Refusal::NotCurrent {
        id: memory.id.clone(),
        status: memory.status,
        superseded_by: memory.superseded_by.clone(),
    }
}

/// What `remember` records.
#[derive(Clone, Debug, Default)]
pub struct Remember {
    /// One line, at most 120 characters, trimmed.
    pub title: String,
    /// At most 8 KiB, possibly empty.
    pub body: String,
    /// Default: the replaced memory's, else [`model::DEFAULT_KIND`].
    pub kind: Option<String>,
    /// Default: the replaced memory's, else the caller's default scope,
    /// else [`model::GLOBAL_SCOPE`].
    pub scope: Option<String>,
    /// Empty with `supersedes`: the replaced memory's.
    pub tags: Vec<String>,
    pub evidence: Vec<String>,
    /// Record it even when a memory in the scope has a similar title.
    pub force: bool,
    /// The memory it replaces in the same write.
    pub supersedes: Option<String>,
    /// Why it replaces `supersedes`.
    pub reason: Option<String>,
}

/// `remember`. With `supersedes`, the new memory also replaces a current
/// one in the same write, and takes its scope, kind and tags unless the
/// input gives them. `default_scope` is asked only when neither the input
/// nor the replaced memory gives a scope.
///
/// The fields given are scanned for secrets before anything is read, so a
/// secret is refused even when the replaced memory is missing or retired;
/// the complete record is scanned again once the inherited values are
/// known.
pub async fn remember<B: Backend>(
    backend: &B,
    labels: &Labels,
    input: Remember,
    default_scope: impl FnOnce() -> Option<String>,
) -> Result<Memory, Error<B::Error>> {
    labels.check()?;
    let supersedes = input
        .supersedes
        .as_deref()
        .map(|id| model::memory_id("supersedes", id))
        .transpose()?;
    match (&supersedes, &input.reason) {
        (Some(_), None) => return Err(model::invalid_field("/reason", "required").into()),
        (None, Some(_)) => return Err(model::invalid_field("/supersedes", "required").into()),
        _ => {}
    }
    if let Some(kind) = &input.kind {
        model::check_kind("kind", kind)?;
    }
    if let Some(scope) = &input.scope {
        model::check_scope("scope", scope)?;
    }
    model::check_items("tags", &input.tags, model::parse_tag)?;
    let title = model::title(&input.title)?;
    let body = model::body(input.body)?;
    let tags = model::tags(input.tags)?;
    let evidence = model::evidence(input.evidence)?;
    let reason = input.reason.as_deref().map(model::reason).transpose()?;
    let scan = |kind: Option<&str>, scope: Option<&str>, tags: &[String]| {
        model::guard_secrets(
            [("title", title.as_str()), ("body", body.as_str())]
                .into_iter()
                .chain(kind.map(|k| ("kind", k)))
                .chain(scope.map(|s| ("scope", s)))
                .chain(tags.iter().map(|t| ("tags", t.as_str())))
                .chain(evidence.iter().map(|e| ("evidence", e.as_str())))
                .chain(reason.iter().map(|r| ("reason", r.as_str())))
                .chain(labels.fields()),
        )
    };
    scan(input.kind.as_deref(), input.scope.as_deref(), &tags)?;
    let old = match supersedes.as_deref() {
        Some(id) => Some(read_memory(backend, id).await?),
        None => None,
    };
    if let Some(old) = &old
        && !old.status.is_current()
    {
        return Err(not_current(old).into());
    }
    let kind = input
        .kind
        .or_else(|| old.as_ref().map(|o| o.kind.clone()))
        .unwrap_or_else(|| model::DEFAULT_KIND.to_owned());
    let scope = input
        .scope
        .or_else(|| old.as_ref().map(|o| o.scope.clone()))
        .or_else(default_scope)
        .unwrap_or_else(|| model::GLOBAL_SCOPE.to_owned());
    let tags = match &old {
        Some(old) if tags.is_empty() => old.tags.clone(),
        _ => tags,
    };
    // The scope as stored, a git remote's included: a remote can name a
    // repository after a token. Inherited values are scanned again.
    scan(Some(&kind), Some(&scope), &tags)?;
    refuse_duplicates(
        backend,
        &title,
        &scope,
        old.as_ref().map(|o| o.id.as_str()),
        input.force,
    )
    .await?;

    let author = labels.actor(backend).await?;
    let id = model::new_memory_id().map_err(Error::NoRandomness)?;
    let replace = old.as_ref().zip(reason.as_deref());
    let mut batch = Batch::new();
    // First, the guard: OLD's next revision, links as its row has them at
    // write time, as `supersede` writes it. It needs no cycle condition:
    // NEW's id is generated by this command, so no row links to or from it
    // before this transaction.
    let guarded = replace.map(|(old, reason)| {
        let index = store::insert_revision(
            &mut batch,
            &NewRevision {
                superseded_by: Some(&id),
                ..carried(old, &author, reason)
            },
            RevisionStatus::Set(Status::Superseded),
            Some(old.rev),
        );
        (index, old.id.as_str())
    });
    store::insert_revision(
        &mut batch,
        &NewRevision {
            id: &id,
            rev: 1,
            kind: &kind,
            scope: &scope,
            title: &title,
            body: &body,
            tags: &tags,
            evidence: &evidence,
            supersedes: replace.map(|(old, _)| old.id.as_str()),
            superseded_by: None,
            author: &author,
            // A replacement's first revision says why it replaced, so a
            // purge of the old memory blanks it with the link.
            reason: reason.as_deref().unwrap_or("remembered"),
        },
        RevisionStatus::Set(Status::Inferred),
        None,
    );
    store::insert_memory(&mut batch, &id);
    store::insert_search(&mut batch, &id);
    if let Some((old, _)) = replace {
        store::update_memory(&mut batch, &old.id, old.rev, false);
        store::update_status(&mut batch, &old.id);
    }
    store::append_event(&mut batch, &id, "remember", &author, &title);
    if let Some((old, reason)) = replace {
        store::append_event(
            &mut batch,
            &old.id,
            "supersede",
            &author,
            &format!("by {id}: {reason}"),
        );
    }
    store::select_memory(&mut batch, &id);
    let results = match (Store { backend }).write(batch).await {
        Ok(results) => results,
        Err(e) => {
            return Err(
                match guarded.filter(|&(index, _)| store::guard_failed(&e, index)) {
                    Some((_, old)) => replaced_meanwhile(backend, old).await,
                    None => e,
                },
            );
        }
    };
    Ok(written(&results)?)
}

/// The error for a replace whose guard failed: what is true of the old
/// memory now. Gone, no longer current, else moved to another rev.
async fn replaced_meanwhile<B: Backend>(backend: &B, old: &str) -> Error<B::Error> {
    match read_memory(backend, old).await {
        Ok(memory) if !memory.status.is_current() => not_current(&memory).into(),
        Ok(memory) => moved(&memory).into(),
        Err(e) => e,
    }
}

/// Refuses a title close to one of a current memory of `scope`, other
/// than `except`, the memory being replaced. The read runs even when
/// nothing is compared (`--force`, a title of stopwords): it carries the
/// schema version check.
async fn refuse_duplicates<B: Backend>(
    backend: &B,
    title: &str,
    scope: &str,
    except: Option<&str>,
    force: bool,
) -> Result<(), Error<B::Error>> {
    let tokens = search::title_tokens(title);
    let mut batch = Batch::new();
    let candidates = (!force && !tokens.is_empty())
        .then(|| search::duplicate_candidates(&mut batch, &tokens, scope, except));
    let results = Store { backend }.read(batch).await?;
    let Some(index) = candidates else {
        return Ok(());
    };
    let mut hits = Vec::new();
    for row in store::rows(&results[index]) {
        let similarity = search::similarity(&tokens, &search::title_tokens(&row.text("title")?));
        if similarity >= search::DUPLICATE_THRESHOLD {
            hits.push((
                row.text("id")?,
                row.text("title")?,
                row.text("status")?,
                similarity,
            ));
        }
    }
    hits.sort_by(|a, b| b.3.total_cmp(&a.3).then_with(|| a.0.cmp(&b.0)));
    if hits.is_empty() {
        return Ok(());
    }
    let candidates: Vec<_> = hits
        .into_iter()
        .map(|(id, title, status, similarity)| Candidate {
            id,
            title,
            status,
            similarity: (similarity * 1000.0).round() / 1000.0,
        })
        .collect();
    Err(Refusal::Duplicate {
        scope: scope.to_owned(),
        candidates,
    }
    .into())
}

/// What `show` finds of a memory.
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub memory: Memory,
    pub confirmations: Vec<Confirmation>,
    pub disputes: Vec<Dispute>,
    /// Every revision, oldest first, when asked for.
    pub revisions: Option<Vec<Revision>>,
}

/// `show`.
pub async fn show<B: Backend>(
    backend: &B,
    id: &str,
    history: bool,
) -> Result<Shown, Error<B::Error>> {
    let id = &model::memory_id("id", id)?;
    let mut batch = Batch::new();
    let memory = store::select_memory(&mut batch, id);
    let purge = store::select_purge(&mut batch, id);
    let confirmations = store::select_confirmations(&mut batch, id);
    let disputes = store::select_disputes(&mut batch, id);
    let revisions = history.then(|| store::select_revisions(&mut batch, id));
    let results = Store { backend }.read(batch).await?;
    let Some(memory) = store::one_memory(&results[memory])? else {
        return Err(not_found(id, purged_at(&results[purge])?).into());
    };
    let confirmations = store::all(&results[confirmations], |r| r.confirmation())?;
    let disputes = store::all(&results[disputes], |r| r.dispute())?;
    let revisions = revisions
        .map(|i| store::all(&results[i], |r| r.revision()))
        .transpose()?;
    Ok(Shown {
        memory,
        confirmations,
        disputes,
        revisions,
    })
}

/// What `revise` changes: only the fields given.
#[derive(Clone, Debug, Default)]
pub struct Revise {
    pub id: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub kind: Option<String>,
    pub scope: Option<String>,
    /// Replace the tags.
    pub tags: Vec<String>,
    /// Add tags.
    pub add_tags: Vec<String>,
    /// Replace the evidence.
    pub evidence: Vec<String>,
    /// Add evidence items.
    pub add_evidence: Vec<String>,
    /// Why, for the history.
    pub reason: String,
}

/// `revise`.
pub async fn revise<B: Backend>(
    backend: &B,
    labels: &Labels,
    args: Revise,
) -> Result<Memory, Error<B::Error>> {
    labels.check()?;
    let id = model::memory_id("id", &args.id)?;
    if let Some(kind) = &args.kind {
        model::check_kind("kind", kind)?;
    }
    if let Some(scope) = &args.scope {
        model::check_scope("scope", scope)?;
    }
    model::check_items("tags", &args.tags, model::parse_tag)?;
    model::check_items("add_tags", &args.add_tags, model::parse_tag)?;
    if !args.tags.is_empty() && !args.add_tags.is_empty() {
        return Err(model::invalid_field("/add_tags", "not_allowed").into());
    }
    if !args.evidence.is_empty() && !args.add_evidence.is_empty() {
        return Err(model::invalid_field("/add_evidence", "not_allowed").into());
    }
    let nothing = args.title.is_none()
        && args.body.is_none()
        && args.kind.is_none()
        && args.scope.is_none()
        && args.tags.is_empty()
        && args.add_tags.is_empty()
        && args.evidence.is_empty()
        && args.add_evidence.is_empty();
    if nothing {
        return Err(model::invalid_field("", "required").into());
    }
    let title = args.title.as_deref().map(model::title).transpose()?;
    let body = args.body.map(model::body).transpose()?;
    let reason = model::reason(&args.reason)?;
    let tags = model::tags(args.tags)?;
    let add_tags = model::tags(args.add_tags)?;
    let evidence = model::evidence(args.evidence)?;
    let add_evidence = model::evidence_in("add_evidence", args.add_evidence)?;
    model::guard_secrets(
        title
            .iter()
            .map(|t| ("title", t.as_str()))
            .chain(body.iter().map(|b| ("body", b.as_str())))
            .chain(tags.iter().chain(&add_tags).map(|t| ("tags", t.as_str())))
            .chain(
                evidence
                    .iter()
                    .chain(&add_evidence)
                    .map(|e| ("evidence", e.as_str())),
            )
            .chain([("reason", reason.as_str())])
            .chain(args.kind.iter().map(|k| ("kind", k.as_str())))
            .chain(args.scope.iter().map(|s| ("scope", s.as_str())))
            .chain(labels.fields()),
    )?;

    let current = read_memory(backend, &id).await?;
    if !current.status.is_current() {
        return Err(not_current(&current).into());
    }
    let merge = |replace: Vec<String>, add: Vec<String>, old: &[String]| {
        if !replace.is_empty() {
            replace
        } else {
            old.iter().cloned().chain(add).collect()
        }
    };
    let next = NextState {
        kind: args.kind.unwrap_or_else(|| current.kind.clone()),
        scope: args.scope.unwrap_or_else(|| current.scope.clone()),
        title: title.unwrap_or_else(|| current.title.clone()),
        body: body.unwrap_or_else(|| current.body.clone()),
        tags: model::tags(merge(tags, add_tags, &current.tags))?,
        evidence: model::evidence(merge(evidence, add_evidence, &current.evidence))?,
    };
    if next.same_as(&current) {
        return Err(Refusal::Unchanged {
            id: current.id.clone(),
        }
        .into());
    }
    let text_changed =
        next.title != current.title || next.body != current.body || next.kind != current.kind;

    let author = labels.actor(backend).await?;
    let id = current.id.as_str();
    let mut batch = Batch::new();
    let guarded = store::insert_revision(
        &mut batch,
        &NewRevision {
            id,
            rev: current.rev + 1,
            kind: &next.kind,
            scope: &next.scope,
            title: &next.title,
            body: &next.body,
            tags: &next.tags,
            evidence: &next.evidence,
            supersedes: None,
            superseded_by: None,
            author: &author,
            reason: &reason,
        },
        if text_changed {
            RevisionStatus::TextChanged
        } else {
            RevisionStatus::Keep
        },
        Some(current.rev),
    );
    store::update_memory(&mut batch, id, current.rev, text_changed);
    store::update_status(&mut batch, id);
    store::sync_search(&mut batch, id);
    store::append_event(&mut batch, id, "revise", &author, &reason);
    store::select_memory(&mut batch, id);
    let results = match (Store { backend }).write(batch).await {
        Ok(results) => results,
        Err(e) if store::guard_failed(&e, guarded) => return Err(conflict(backend, id).await),
        Err(e) => return Err(e),
    };
    Ok(written(&results)?)
}

/// Revision `memory.rev + 1` of `memory` as it stands, by `author` for
/// `reason`, its links left to the write; verbs that change one field
/// override it.
pub(crate) fn carried<'a>(
    memory: &'a Memory,
    author: &'a model::Author,
    reason: &'a str,
) -> NewRevision<'a> {
    NewRevision {
        id: &memory.id,
        rev: memory.rev + 1,
        kind: &memory.kind,
        scope: &memory.scope,
        title: &memory.title,
        body: &memory.body,
        tags: &memory.tags,
        evidence: &memory.evidence,
        // The links as the row has them when the write runs.
        supersedes: None,
        superseded_by: None,
        author,
        reason,
    }
}

/// What a revise would make of a memory.
struct NextState {
    kind: String,
    scope: String,
    title: String,
    body: String,
    tags: Vec<String>,
    evidence: Vec<String>,
}

impl NextState {
    fn same_as(&self, memory: &Memory) -> bool {
        self.kind == memory.kind
            && self.scope == memory.scope
            && self.title == memory.title
            && self.body == memory.body
            && self.tags == memory.tags
            && self.evidence == memory.evidence
    }
}

/// The error for a write whose guard failed: the memory moved on (or
/// went) after it was read.
pub(crate) async fn conflict<B: Backend>(backend: &B, id: &str) -> Error<B::Error> {
    match read_memory(backend, id).await {
        Ok(memory) => moved(&memory).into(),
        Err(e) => e,
    }
}

/// `memory_conflict` for `memory`, read again after a guard failed.
fn moved(memory: &Memory) -> Refusal {
    Refusal::Conflict {
        id: memory.id.clone(),
        current_rev: memory.rev,
        since_rev: None,
    }
}

/// What `recall` searches.
#[derive(Clone, Copy, Debug)]
pub struct Recall<'a> {
    /// Words to search for; `None`: the newest memories.
    pub query: Option<&'a str>,
    /// `None`: every scope.
    pub scopes: Option<&'a [String]>,
    /// Empty: [`Status::RECALLED`].
    pub statuses: &'a [Status],
    /// Any of them; empty: any kind.
    pub kinds: &'a [String],
    /// All of them; empty: no tag needed.
    pub tags: &'a [String],
    pub limit: i64,
}

/// `recall`: the memories found, best first, with their score when there
/// is a query.
pub async fn recall<B: Backend>(
    backend: &B,
    args: Recall<'_>,
) -> Result<Vec<(Memory, Option<f64>)>, Error<B::Error>> {
    model::check_items("scope", args.scopes.unwrap_or_default(), model::parse_scope)?;
    model::check_items("kind", args.kinds, model::parse_kind)?;
    model::check_items("tag", args.tags, model::parse_tag)?;
    model::check_limit(args.limit, memoire_api::memory::MAX_RECALL_LIMIT)?;
    let terms = match args.query {
        Some(query) => {
            let terms = search::terms(query);
            if terms.is_empty() {
                return Err(Refusal::InvalidQuery.into());
            }
            Some(terms)
        }
        None => None,
    };
    let mut statuses = args.statuses.to_vec();
    if statuses.is_empty() {
        statuses = Status::RECALLED.to_vec();
    }
    let filter = Filter {
        scopes: args.scopes,
        statuses: &statuses,
        kinds: args.kinds,
        tags: args.tags,
        limit: args.limit,
    };
    let mut batch = Batch::new();
    let index = match &terms {
        Some(terms) => search::ranked(&mut batch, terms, &filter),
        None => search::newest(&mut batch, &filter),
    };
    let results = Store { backend }.read(batch).await?;
    let mut memories = Vec::new();
    for row in store::rows(&results[index]) {
        let score = terms.is_some().then(|| row.real("score")).transpose()?;
        memories.push((row.memory()?, score));
    }
    Ok(memories)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BackendError, SqlFailure, history, retire, trust};
    use memoire_api::{QueryMode, StatementResult};
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    /// A backend no verb may reach: these inputs are refused first.
    struct Unreachable;

    #[derive(Debug)]
    struct Reached;

    impl BackendError for Reached {
        fn sql(&self) -> Option<SqlFailure<'_>> {
            None
        }
    }

    impl Backend for Unreachable {
        type Error = Reached;
        async fn run(&self, _: QueryMode, _: Batch) -> Result<Vec<StatementResult>, Reached> {
            Err(Reached)
        }
        async fn principal(&self) -> Result<String, Reached> {
            Err(Reached)
        }
    }

    fn ready<T>(future: impl Future<Output = T>) -> T {
        match pin!(future)
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("the fake backend never waits"),
        }
    }

    /// The pointer and code a verb refused with.
    fn refused<T: std::fmt::Debug>(result: Result<T, Error<Reached>>) -> (String, &'static str) {
        match result {
            Err(Error::Refused(refusal)) => refusal
                .detail()
                .unwrap_or_else(|| panic!("not an input refusal: {refusal:?}")),
            other => panic!("not refused: {other:?}"),
        }
    }

    const ID: &str = "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8";

    fn remember_with(input: Remember) -> (String, &'static str) {
        let input = Remember {
            title: "A title".into(),
            ..input
        };
        refused(ready(remember(
            &Unreachable,
            &Labels::default(),
            input,
            || None,
        )))
    }

    #[test]
    fn a_replacement_is_scanned_before_the_replaced_memory_is_read() {
        let secret = format!("token {}{}", ["mz", "k_"].concat(), "q7Rx".repeat(6));
        let input = Remember {
            title: secret,
            supersedes: Some(ID.into()),
            reason: Some("r".into()),
            ..Remember::default()
        };
        match ready(remember(&Unreachable, &Labels::default(), input, || None)) {
            Err(Error::Refused(Refusal::SecretDetected { field, .. })) => {
                assert_eq!(field, "title");
            }
            other => panic!("not refused before the read: {other:?}"),
        }
    }

    #[test]
    fn verbs_check_what_the_cli_parser_checks() {
        let at = |pointer: &str, code: &'static str| (pointer.to_owned(), code);
        assert_eq!(
            remember_with(Remember {
                tags: vec!["ok".into(), "Bad".into()],
                ..Remember::default()
            }),
            at("/tags/1", "invalid")
        );
        assert_eq!(
            remember_with(Remember {
                kind: Some("K".into()),
                ..Remember::default()
            }),
            at("/kind", "invalid")
        );
        assert_eq!(
            remember_with(Remember {
                scope: Some("a b".into()),
                ..Remember::default()
            }),
            at("/scope", "invalid")
        );
        assert_eq!(
            remember_with(Remember {
                supersedes: Some("mem_x".into()),
                reason: Some("r".into()),
                ..Remember::default()
            }),
            at("/supersedes", "invalid")
        );
        assert_eq!(
            remember_with(Remember {
                supersedes: Some(ID.into()),
                ..Remember::default()
            }),
            at("/reason", "required")
        );
        assert_eq!(
            remember_with(Remember {
                reason: Some("r".into()),
                ..Remember::default()
            }),
            at("/supersedes", "required")
        );
        assert_eq!(
            remember_with(Remember {
                evidence: vec!["e".into(), " ".into()],
                ..Remember::default()
            }),
            at("/evidence/1", "invalid")
        );
        let control = Labels {
            agent: "a\u{7}".into(),
            session: String::new(),
        };
        let input = Remember {
            title: "t".into(),
            ..Remember::default()
        };
        assert_eq!(
            refused(ready(remember(&Unreachable, &control, input, || None))),
            at("/agent", "invalid")
        );

        let labels = Labels::default();
        let revise_with = |revise: Revise| {
            refused(ready(super::revise(
                &Unreachable,
                &labels,
                Revise {
                    id: ID.into(),
                    reason: "r".into(),
                    ..revise
                },
            )))
        };
        assert_eq!(revise_with(Revise::default()), at("", "required"));
        assert_eq!(
            revise_with(Revise {
                tags: vec!["a".into()],
                add_tags: vec!["b".into()],
                ..Revise::default()
            }),
            at("/add_tags", "not_allowed")
        );
        assert_eq!(
            revise_with(Revise {
                add_evidence: vec![String::new()],
                ..Revise::default()
            }),
            at("/add_evidence/0", "invalid")
        );
        assert_eq!(
            refused(ready(super::revise(
                &Unreachable,
                &labels,
                Revise {
                    id: "nope".into(),
                    title: Some("t".into()),
                    reason: "r".into(),
                    ..Revise::default()
                },
            ))),
            at("/id", "invalid")
        );

        assert_eq!(
            refused(ready(trust::confirm(
                &Unreachable,
                &labels,
                ID,
                vec![],
                None
            ))),
            at("/evidence", "required")
        );
        assert_eq!(
            refused(ready(trust::confirm(
                &Unreachable,
                &labels,
                ID,
                vec!["e".into()],
                Some(0)
            ))),
            at("/rev", "out_of_range")
        );
        assert_eq!(
            refused(ready(retire::supersede(
                &Unreachable,
                &labels,
                ID,
                "x",
                "r"
            ))),
            at("/by", "invalid")
        );
        assert_eq!(
            refused(ready(show(&Unreachable, "mem_", false))),
            at("/id", "invalid")
        );

        let recall_with = |kinds: &[String], limit: i64| {
            refused(ready(recall(
                &Unreachable,
                Recall {
                    query: None,
                    scopes: None,
                    statuses: &[],
                    kinds,
                    tags: &[],
                    limit,
                },
            )))
        };
        assert_eq!(
            recall_with(&["fact".into(), "x".into()], 10),
            at("/kind/1", "invalid")
        );
        assert_eq!(recall_with(&[], 101), at("/limit", "out_of_range"));
        assert_eq!(
            refused(ready(history::export(&Unreachable, None, -1, 10))),
            at("/after", "out_of_range")
        );
        let cursor = store::Cursor {
            store_id: "mst_bad".into(),
            seq: 1,
        };
        let events = |after, since_seconds| {
            refused(ready(history::events(
                &Unreachable,
                history::Events {
                    scopes: None,
                    since_seconds,
                    after,
                    limit: 50,
                },
            )))
        };
        assert_eq!(
            events(store::After::Cursor(&cursor), None),
            at("/store_id", "invalid")
        );
        assert_eq!(
            events(store::After::FirstLook, Some(60)),
            at("/since_seconds", "not_allowed")
        );
    }
}
