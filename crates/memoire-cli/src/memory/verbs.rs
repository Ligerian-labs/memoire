//! One function per `memoire memory` verb.
//!
//! A verb reads what it needs (the version check first), runs the guards,
//! then sends at most one write batch. A write that a concurrent writer
//! made stale fails its guard and exits with `memory_conflict`; the agent
//! re-reads and retries. The CLI itself takes no lock and never retries.
use super::model::{self, Memory, Scored, Status};
use super::search::{self, Filter};
use super::store::{self, Batch, NewRevision, RevisionStatus};
use super::{Context, render, schema};
use crate::cli::{MemoryInitArgs, RecallArgs, RememberArgs, ReviseArgs};
use crate::config::io_error;
use crate::error::{Error, Result, exit};
use memoire_api::{CreateDatabase, DatabaseKind, DatabaseState};
use serde_json::json;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// `init`: the database, persistent unless `--burstable`, then the schema.
pub fn init(ctx: &Context<'_>, args: &MemoryInitArgs) -> Result<()> {
    let kind = if args.burstable {
        DatabaseKind::Burstable
    } else {
        DatabaseKind::Persistent
    };
    let create = || {
        ctx.client.create_database(&CreateDatabase {
            name: ctx.db.clone(),
            kind,
            idle_ttl_seconds: args.ttl,
        })
    };
    let existing = match ctx.client.database(&ctx.db) {
        Ok(db) if db.state == DatabaseState::Active => Some(db),
        Ok(_) => None,
        Err(e) if e.code() == "database_not_found" => None,
        Err(e) => return Err(e),
    };
    let (database, created) = match existing {
        Some(db) => (db, false),
        None => match create() {
            Ok(db) => (db, true),
            // Another init created it in between: use that one.
            Err(e) if e.code() == "database_exists" => (ctx.client.database(&ctx.db)?, false),
            Err(e) => return Err(e),
        },
    };
    if !created && (args.burstable || args.persistent) && database.kind != kind {
        ctx.out.note(&format!(
            "note: `{}` already exists as a {} database; keeping it",
            ctx.db,
            crate::output::kind(&database)
        ));
    }
    let store = ctx.store();
    let migrated_from = schema::migrate(&store)?;
    let store_id = schema::store_id(&store)?;
    if ctx.out.json {
        return ctx.out.json(&json!({
            "database": database,
            "created": created,
            "schema_version": schema::SCHEMA_VERSION,
            "migrated_from": migrated_from,
            "store_id": store_id,
        }));
    }
    ctx.out
        .line(&render::init(&database, created, migrated_from, &store_id))
}

/// The body from `--body`, `--body-file FILE` or `--body-file -`.
fn read_body(body: Option<String>, file: Option<&Path>) -> Result<Option<String>> {
    let Some(path) = file else {
        return Ok(body);
    };
    if path == Path::new("-") {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text).map_err(|e| {
            Error::local("missing_input", exit::USAGE, format!("standard input: {e}"))
        })?;
        return Ok(Some(text));
    }
    std::fs::read_to_string(path)
        .map(Some)
        .map_err(|e| io_error(path, &e))
}

/// The scope a memory gets without `--scope`: the git checkout's project
/// scope, else `global`.
fn project_scope() -> Option<String> {
    let dir: PathBuf = std::env::current_dir().ok()?;
    super::scope::project_scope(&dir)
}

pub(super) fn not_found(id: &str, purged_at: Option<String>) -> Error {
    let message = match purged_at {
        Some(at) => format!("memory {id} was purged at {at}"),
        None => format!("no memory {id} in this store"),
    };
    Error::local("memory_not_found", exit::NOT_FOUND, message).with("id", id)
}

/// The memory `id` as a read finds it, with the time it was purged when
/// it is gone.
pub(super) fn read_memory(ctx: &Context<'_>, id: &str) -> Result<Memory> {
    let mut batch = Batch::new();
    let memory = store::select_memory(&mut batch, id);
    let purge = store::select_purge(&mut batch, id);
    let results = ctx.store().read(batch)?;
    match store::one_memory(&results[memory])? {
        Some(memory) => Ok(memory),
        None => Err(not_found(id, purged_at(&results[purge])?)),
    }
}

pub(super) fn purged_at(result: &memoire_api::StatementResult) -> Result<Option<String>> {
    store::rows(result)
        .next()
        .map(|row| row.text("created_at"))
        .transpose()
}

/// The memory a write batch selected last.
pub(super) fn written(results: &[memoire_api::StatementResult]) -> Result<Memory> {
    results
        .last()
        .map(store::one_memory)
        .transpose()?
        .flatten()
        .ok_or_else(|| store::invalid_response("the written memory"))
}

pub(super) fn print_memory(ctx: &Context<'_>, memory: &Memory) -> Result<()> {
    if ctx.out.json {
        return ctx.out.json(memory);
    }
    ctx.out.line(&render::memory(memory))
}

/// A refusal on a memory that was superseded or forgotten.
pub(super) fn not_current(memory: &Memory) -> Error {
    let mut message = format!("memory {} is {}", memory.id, memory.status.as_str());
    if let Some(by) = &memory.superseded_by {
        message.push_str(&format!(" by {by}"));
    }
    Error::local("memory_not_current", exit::CONFLICT, message)
        .with("id", memory.id.clone())
        .with("status", memory.status.as_str())
        .with("superseded_by", memory.superseded_by.clone())
}

/// `remember`. With `--supersedes`, the new memory also replaces a current
/// one in the same write, and takes its scope, kind and tags unless the
/// flags give them.
pub fn remember(ctx: &Context<'_>, args: RememberArgs) -> Result<()> {
    let title = model::title(&args.title)?;
    let body = model::body(read_body(args.body, args.body_file.as_deref())?.unwrap_or_default())?;
    let tags = model::tags(args.tags)?;
    let evidence = model::evidence(args.evidence)?;
    let reason = args.reason.as_deref().map(model::reason).transpose()?;
    let old = args
        .supersedes
        .as_deref()
        .map(|id| read_memory(ctx, id))
        .transpose()?;
    if let Some(old) = &old
        && !old.status.is_current()
    {
        return Err(not_current(old));
    }
    let kind = args
        .kind
        .or_else(|| old.as_ref().map(|o| o.kind.clone()))
        .unwrap_or_else(|| model::DEFAULT_KIND.to_owned());
    let scope = args
        .scope
        .or_else(|| old.as_ref().map(|o| o.scope.clone()))
        .or_else(project_scope)
        .unwrap_or_else(|| model::GLOBAL_SCOPE.to_owned());
    let tags = match &old {
        Some(old) if tags.is_empty() => old.tags.clone(),
        _ => tags,
    };
    // The scope as stored, a git remote's included: a remote can name a
    // repository after a token. Inherited values are scanned again.
    model::guard_secrets(
        [
            ("title", title.as_str()),
            ("body", body.as_str()),
            ("kind", kind.as_str()),
            ("scope", scope.as_str()),
        ]
        .into_iter()
        .chain(tags.iter().map(|t| ("tags", t.as_str())))
        .chain(evidence.iter().map(|e| ("evidence", e.as_str())))
        .chain(reason.iter().map(|r| ("reason", r.as_str())))
        .chain(ctx.labels()),
    )?;
    refuse_duplicates(
        ctx,
        &title,
        &scope,
        old.as_ref().map(|o| o.id.as_str()),
        args.force,
    )?;

    let author = ctx.actor()?;
    let id = model::new_memory_id()?;
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
    let results = match ctx.store().write(batch) {
        Ok(results) => results,
        Err(e) => {
            return Err(
                match guarded.filter(|&(index, _)| store::guard_failed(&e, index)) {
                    Some((_, old)) => replaced_meanwhile(ctx, old),
                    None => e,
                },
            );
        }
    };
    print_memory(ctx, &written(&results)?)
}

/// The error for a replace whose guard failed: what is true of the old
/// memory now. Gone, no longer current, else moved to another rev.
fn replaced_meanwhile(ctx: &Context<'_>, old: &str) -> Error {
    match read_memory(ctx, old) {
        Ok(memory) if !memory.status.is_current() => not_current(&memory),
        Ok(memory) => moved(&memory),
        Err(e) => e,
    }
}

/// Refuses a title close to one of a current memory of `scope`, other
/// than `except`, the memory being replaced. The read runs even when
/// nothing is compared (`--force`, a title of stopwords): it carries the
/// schema version check.
fn refuse_duplicates(
    ctx: &Context<'_>,
    title: &str,
    scope: &str,
    except: Option<&str>,
    force: bool,
) -> Result<()> {
    let tokens = search::title_tokens(title);
    let mut batch = Batch::new();
    let candidates = (!force && !tokens.is_empty())
        .then(|| search::duplicate_candidates(&mut batch, &tokens, scope, except));
    let results = ctx.store().read(batch)?;
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
    let Some((id, ..)) = hits.first() else {
        return Ok(());
    };
    let message = format!(
        "a memory in {scope} has a similar title ({id}); revise it, or pass --force to record this one too"
    );
    let candidates: Vec<_> = hits
        .iter()
        .map(|(id, title, status, similarity)| {
            json!({"id": id, "title": title, "status": status,
                   "similarity": (similarity * 1000.0).round() / 1000.0})
        })
        .collect();
    Err(Error::local("memory_duplicate", exit::CONFLICT, message).with("candidates", candidates))
}

/// `show`.
pub fn show(ctx: &Context<'_>, id: &str, history: bool) -> Result<()> {
    let mut batch = Batch::new();
    let memory = store::select_memory(&mut batch, id);
    let purge = store::select_purge(&mut batch, id);
    let confirmations = store::select_confirmations(&mut batch, id);
    let disputes = store::select_disputes(&mut batch, id);
    let revisions = history.then(|| store::select_revisions(&mut batch, id));
    let results = ctx.store().read(batch)?;
    let Some(memory) = store::one_memory(&results[memory])? else {
        return Err(not_found(id, purged_at(&results[purge])?));
    };
    let confirmations = store::all(&results[confirmations], |r| r.confirmation())?;
    let disputes = store::all(&results[disputes], |r| r.dispute())?;
    let revisions = revisions
        .map(|i| store::all(&results[i], |r| r.revision()))
        .transpose()?;
    if ctx.out.json {
        let mut shown = json!({
            "memory": memory, "confirmations": confirmations, "disputes": disputes,
        });
        if let Some(revisions) = &revisions {
            shown["revisions"] = json!(revisions);
        }
        return ctx.out.json(&shown);
    }
    ctx.out.line(&render::show(
        &memory,
        &confirmations,
        &disputes,
        revisions.as_deref(),
    ))
}

/// `revise`.
pub fn revise(ctx: &Context<'_>, args: ReviseArgs) -> Result<()> {
    let title = args.title.as_deref().map(model::title).transpose()?;
    let body = read_body(args.body, args.body_file.as_deref())?
        .map(model::body)
        .transpose()?;
    let reason = model::reason(&args.reason)?;
    let tags = model::tags(args.tags)?;
    let add_tags = model::tags(args.add_tags)?;
    let evidence = model::evidence(args.evidence)?;
    let add_evidence = model::evidence(args.add_evidence)?;
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
            .chain(ctx.labels()),
    )?;

    let current = read_memory(ctx, &args.id)?;
    if !current.status.is_current() {
        return Err(not_current(&current));
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
        return Err(Error::local(
            "memory_unchanged",
            exit::REJECTED,
            format!("the revision would leave memory {} as it is", current.id),
        ));
    }
    let text_changed =
        next.title != current.title || next.body != current.body || next.kind != current.kind;

    let author = ctx.actor()?;
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
    let results = match ctx.store().write(batch) {
        Ok(results) => results,
        Err(e) if store::guard_failed(&e, guarded) => return Err(conflict(ctx, id)),
        Err(e) => return Err(e),
    };
    print_memory(ctx, &written(&results)?)
}

/// Revision `memory.rev + 1` of `memory` as it stands, by `author` for
/// `reason`, its links left to the write; verbs that change one field
/// override it.
pub(super) fn carried<'a>(
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
pub(super) fn conflict(ctx: &Context<'_>, id: &str) -> Error {
    match read_memory(ctx, id) {
        Ok(memory) => moved(&memory),
        Err(e) => e,
    }
}

/// `memory_conflict` for `memory`, read again after a guard failed.
fn moved(memory: &Memory) -> Error {
    let id = memory.id.as_str();
    Error::local(
        "memory_conflict",
        exit::CONFLICT,
        format!(
            "memory {id} changed while this ran: it is at rev {}; read it again and retry",
            memory.rev
        ),
    )
    .with("id", id)
    .with("current_rev", memory.rev)
}

/// `recall`.
pub fn recall(ctx: &Context<'_>, args: &RecallArgs) -> Result<()> {
    let terms = match &args.query {
        Some(query) => {
            let terms = search::terms(query);
            if terms.is_empty() {
                return Err(Error::local(
                    "invalid_query",
                    exit::USAGE,
                    "the query has no word to search for",
                ));
            }
            Some(terms)
        }
        None => None,
    };
    let scopes = if args.all_scopes {
        None
    } else if !args.scopes.is_empty() {
        Some(args.scopes.clone())
    } else {
        let mut scopes: Vec<String> = project_scope().into_iter().collect();
        scopes.push(model::GLOBAL_SCOPE.to_owned());
        Some(scopes)
    };
    let mut statuses = args.statuses.clone();
    if statuses.is_empty() {
        statuses = Status::RECALLED.to_vec();
    }
    let filter = Filter {
        scopes: scopes.as_deref(),
        statuses: &statuses,
        kinds: &args.kinds,
        tags: &args.tags,
        limit: args.limit,
    };
    let mut batch = Batch::new();
    let index = match &terms {
        Some(terms) => search::ranked(&mut batch, terms, &filter),
        None => search::newest(&mut batch, &filter),
    };
    let results = ctx.store().read(batch)?;
    let mut memories = Vec::new();
    for row in store::rows(&results[index]) {
        let score = terms.is_some().then(|| row.real("score")).transpose()?;
        memories.push((row.memory()?, score));
    }
    if ctx.out.json {
        let memories: Vec<Scored<'_>> = memories
            .iter()
            .map(|(memory, score)| Scored {
                memory,
                score: *score,
            })
            .collect();
        return ctx.out.json(&json!({
            "db": ctx.db, "query": args.query, "scopes": scopes, "memories": memories,
        }));
    }
    let searched = scopes
        .as_ref()
        .map_or_else(|| "every scope".to_owned(), |s| s.join(", "));
    if memories.is_empty() {
        ctx.out.note(&format!("No memories in {searched}."));
        return Ok(());
    }
    let memories: Vec<Memory> = memories.into_iter().map(|(m, _)| m).collect();
    ctx.out.line(&render::recall(&memories))?;
    ctx.out.note(&format!("Searched {searched}."));
    Ok(())
}
