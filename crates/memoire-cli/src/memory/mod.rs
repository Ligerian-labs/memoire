//! `memoire memory`: a shared memory for a fleet of agents, kept as
//! versioned SQL in an ordinary memoire database.
//!
//! The memory itself, its model, SQL, guards and verbs, is the
//! `memoire-memory` engine. The gateway runs it behind its memory routes
//! ([`api`]); against a gateway without them, it runs here over the query
//! route ([`backend`]); [`store`] picks the path. This module reads the
//! verbs' input (a body from a file or standard input, the default scope
//! from the git checkout), creates the database for `init`, keeps the
//! `log --unseen` cursors, writes the export file and prints what the
//! verbs return.
pub mod api;
pub mod backend;
pub mod cursor;
pub mod render;
pub mod scope;
pub mod store;

use crate::cli::{LogArgs, MemoryArgs, MemoryCommand, MemoryInitArgs, RecallArgs};
use crate::config::{Env, io_error};
use crate::error::{Error, Result, exit};
use crate::output::Out;
use backend::Gateway;
use cursor::{Cursors, FIRST_LOOK_SECONDS};
use memoire_api::{CreateDatabase, DatabaseKind, DatabaseState};
use memoire_memory::model::{self, Event, Memory, Scored};
use memoire_memory::verbs::{Recall, Remember, Revise};
use memoire_memory::{Labels, schema};
use serde_json::json;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use store::Store;

/// What every verb works with.
pub struct Context<'a> {
    pub out: &'a Out,
    /// Where this machine keeps its files (the `log --unseen` cursors).
    pub env: &'a Env,
    /// The database holding the store, and the path to it.
    pub store: Store,
    /// `--agent` and `--session`, recorded with writes; empty when not
    /// given.
    pub labels: Labels,
}

pub fn run(out: &Out, env: &Env, args: MemoryArgs) -> Result<()> {
    let (server, key) = env.login()?;
    let ctx = Context {
        out,
        env,
        store: Store::new(
            Gateway {
                client: crate::client::Client::new(&server, Some(&key))?,
                db: args.db,
            },
            env.memory_sql,
        ),
        labels: Labels {
            agent: args.agent.unwrap_or_default(),
            session: args.session.unwrap_or_default(),
        },
    };
    let (store, labels) = (&ctx.store, &ctx.labels);
    match args.command {
        MemoryCommand::Init(args) => init(&ctx, &args),
        MemoryCommand::Remember(args) => {
            // Checked before the body is read, so a bad title is reported
            // first; the engine checks it again.
            model::title(&args.title).map_err(|r| store.gateway.refused(r))?;
            let body = read_body(args.body, args.body_file.as_deref())?.unwrap_or_default();
            let input = Remember {
                title: args.title,
                body,
                kind: args.kind,
                scope: args.scope,
                tags: args.tags,
                evidence: args.evidence,
                force: args.force,
                supersedes: args.supersedes,
                reason: args.reason,
            };
            let memory = store.remember(labels, input, project_scope)?;
            print_memory(&ctx, &memory)
        }
        MemoryCommand::Show { id, history } => show(&ctx, &id, history),
        MemoryCommand::Revise(args) => {
            // As for `remember`: the title first.
            if let Some(title) = &args.title {
                model::title(title).map_err(|r| store.gateway.refused(r))?;
            }
            let body = read_body(args.body, args.body_file.as_deref())?;
            let input = Revise {
                id: args.id,
                title: args.title,
                body,
                kind: args.kind,
                scope: args.scope,
                tags: args.tags,
                add_tags: args.add_tags,
                evidence: args.evidence,
                add_evidence: args.add_evidence,
                reason: args.reason,
            };
            print_memory(&ctx, &store.revise(labels, input)?)
        }
        MemoryCommand::Recall(args) => recall(&ctx, &args),
        MemoryCommand::Confirm { id, evidence, rev } => {
            let memory = store.confirm(labels, &id, evidence, rev)?;
            print_memory(&ctx, &memory)
        }
        MemoryCommand::Dispute {
            id,
            reason,
            evidence,
            rev,
        } => {
            let memory = store.dispute(labels, &id, &reason, evidence, rev)?;
            print_memory(&ctx, &memory)
        }
        MemoryCommand::Resolve { id, reason } => {
            let memory = store.resolve(labels, &id, &reason)?;
            print_memory(&ctx, &memory)
        }
        MemoryCommand::Supersede { id, by, reason } => {
            let (old, new) = store.supersede(labels, &id, &by, &reason)?;
            if ctx.out.json {
                return ctx.out.json(&json!({"superseded": old, "by": new}));
            }
            ctx.out.line(&render::superseded(&old, &new))
        }
        MemoryCommand::Forget { id, reason, purge } => {
            if purge {
                store.purge(labels, &id, &reason)?;
                if ctx.out.json {
                    return ctx.out.json(&json!({"id": id, "purged": true}));
                }
                ctx.out.line(&format!("Purged {id} and its history"))
            } else {
                let memory = store.forget(labels, &id, &reason)?;
                print_memory(&ctx, &memory)
            }
        }
        MemoryCommand::Log(args) => log(&ctx, &args),
        MemoryCommand::Export { scopes, output } => export(&ctx, &scopes, &output),
    }
}

/// `init`: the database, persistent unless `--burstable`, then the schema.
/// A key that may not read the catalog (a memory-only key) skips the
/// database step on the API path: the store's routes find the database.
fn init(ctx: &Context<'_>, args: &MemoryInitArgs) -> Result<()> {
    let (client, db) = (&ctx.store.gateway.client, &ctx.store.gateway.db);
    let kind = if args.burstable {
        DatabaseKind::Burstable
    } else {
        DatabaseKind::Persistent
    };
    let create = || {
        client.create_database(&CreateDatabase {
            name: db.clone(),
            kind,
            idle_ttl_seconds: args.ttl,
        })
    };
    // Without the permission to read or create it, the database is left to
    // the store's routes, which find it or answer `database_not_found`.
    let unseen = |e: &Error| e.code() == "unauthorized" && !ctx.env.memory_sql;
    let existing = match client.database(db) {
        Ok(db) if db.state == DatabaseState::Active => Some(Some(db)),
        Ok(_) => None,
        Err(e) if e.code() == "database_not_found" => None,
        Err(e) if unseen(&e) => Some(None),
        Err(e) => return Err(e),
    };
    let (database, created) = match existing {
        Some(db) => (db, false),
        None => match create() {
            Ok(db) => (Some(db), true),
            // Another init created it in between: use that one.
            Err(e) if e.code() == "database_exists" => (Some(client.database(db)?), false),
            Err(e) if unseen(&e) => (None, false),
            Err(e) => return Err(e),
        },
    };
    if let Some(database) = &database
        && !created
        && (args.burstable || args.persistent)
        && database.kind != kind
    {
        ctx.out.note(&format!(
            "note: `{db}` already exists as a {} database; keeping it",
            crate::output::kind(database)
        ));
    }
    let (migrated_from, store_id) = ctx.store.init().map_err(|e| match e {
        e if database.is_none() && e.code() == "database_not_found" => Error::local(
            "database_not_found",
            exit::NOT_FOUND,
            format!(
                "no database `{db}` in the team, and this key may not create one; run `memoire memory init` once with a key that may create databases"
            ),
        )
        .with("status", 404),
        e => e,
    })?;
    if ctx.out.json {
        return ctx.out.json(&json!({
            "database": database,
            "created": created,
            "schema_version": schema::SCHEMA_VERSION,
            "migrated_from": migrated_from,
            "store_id": store_id,
        }));
    }
    ctx.out.line(&render::init(
        database.as_ref(),
        created,
        migrated_from,
        &store_id,
    ))
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
    scope::project_scope(&dir)
}

fn print_memory(ctx: &Context<'_>, memory: &Memory) -> Result<()> {
    if ctx.out.json {
        return ctx.out.json(memory);
    }
    ctx.out.line(&render::memory(memory))
}

/// `show`.
fn show(ctx: &Context<'_>, id: &str, history: bool) -> Result<()> {
    let shown = ctx.store.show(id, history)?;
    if ctx.out.json {
        let mut json = json!({
            "memory": shown.memory, "confirmations": shown.confirmations,
            "disputes": shown.disputes,
        });
        if let Some(revisions) = &shown.revisions {
            json["revisions"] = json!(revisions);
        }
        return ctx.out.json(&json);
    }
    ctx.out.line(&render::show(
        &shown.memory,
        &shown.confirmations,
        &shown.disputes,
        shown.revisions.as_deref(),
    ))
}

/// `recall`.
fn recall(ctx: &Context<'_>, args: &RecallArgs) -> Result<()> {
    let scopes = if args.all_scopes {
        None
    } else if !args.scopes.is_empty() {
        Some(args.scopes.clone())
    } else {
        let mut scopes: Vec<String> = project_scope().into_iter().collect();
        scopes.push(model::GLOBAL_SCOPE.to_owned());
        Some(scopes)
    };
    let memories = ctx.store.recall(Recall {
        query: args.query.as_deref(),
        scopes: scopes.as_deref(),
        statuses: &args.statuses,
        kinds: &args.kinds,
        tags: &args.tags,
        limit: args.limit,
    })?;
    if ctx.out.json {
        let memories: Vec<Scored<'_>> = memories
            .iter()
            .map(|(memory, score)| Scored {
                memory,
                score: *score,
            })
            .collect();
        return ctx.out.json(&json!({
            "db": ctx.store.gateway.db, "query": args.query, "scopes": scopes, "memories": memories,
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

/// `log`.
fn log(ctx: &Context<'_>, args: &LogArgs) -> Result<()> {
    let scopes = (!args.scopes.is_empty()).then_some(args.scopes.as_slice());
    if args.unseen {
        let reader = crate::cli::reader_name(args.reader.as_deref().unwrap_or_default())
            .map_err(|message| Error::local("invalid_argument", exit::USAGE, message))?;
        return unseen(ctx, scopes, reader.as_deref(), args.limit);
    }
    let events = ctx.store.log(scopes, args.since, args.limit)?;
    if ctx.out.json {
        return ctx.out.json(&json!({ "events": events }));
    }
    print_events(ctx, &events, "No events.")
}

fn print_events(ctx: &Context<'_>, events: &[Event], none: &str) -> Result<()> {
    if events.is_empty() {
        ctx.out.note(none);
        return Ok(());
    }
    ctx.out.line(&render::log(events))
}

/// `log --unseen`: the events after this reader's cursor, or of the
/// last [`FIRST_LOOK_SECONDS`] on its first look, then the cursor moves to
/// the store's newest event, once they are printed.
fn unseen(
    ctx: &Context<'_>,
    scopes: Option<&[String]>,
    reader: Option<&str>,
    limit: i64,
) -> Result<()> {
    let cursors = Cursors::new(ctx.env)?;
    let key = cursor::Key::new(
        ctx.store.gateway.client.server(),
        &ctx.store.gateway.db,
        scopes.unwrap_or_default(),
        cursor::reader(reader, &ctx.labels.session),
    );
    let saved = cursors.saved(&key)?;
    let unseen = ctx.store.unseen(scopes, saved.as_ref(), limit)?;
    let (first, after, through, skipped) =
        (unseen.first, unseen.after, unseen.through, unseen.skipped);
    if ctx.out.json {
        ctx.out.json(&json!({
            "events": unseen.events,
            "unseen": {"reader": key.reader, "first": first, "after": after,
                       "through": through, "skipped": skipped},
        }))?;
    } else {
        if first {
            ctx.out.note(&format!(
                "First look for {}: events of the last {} hours.",
                key.reader,
                FIRST_LOOK_SECONDS / 3600
            ));
        }
        print_events(ctx, &unseen.events, "No new events.")?;
        if skipped > 0 {
            ctx.out.note(&format!(
                "{skipped} older unseen events skipped; raise --limit to see more next time"
            ));
        }
    }
    cursors.advance(&key, unseen.cursor())
}

/// `export`: JSON Lines, one memory object per line, in `seq` order,
/// every status but `forgotten`. Written to a private temporary file
/// beside the target and renamed over it once complete.
fn export(ctx: &Context<'_>, scopes: &[String], path: &Path) -> Result<()> {
    let scopes = (!scopes.is_empty()).then_some(scopes);
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|e| io_error(dir, &e))?;
    let mut count = 0;
    let mut after = 0;
    loop {
        let page = ctx.store.export_page(scopes, after)?;
        let mut lines = String::new();
        for memory in &page.memories {
            lines.push_str(&serde_json::to_string(memory).expect("memories serialize"));
            lines.push('\n');
        }
        file.write_all(lines.as_bytes())
            .map_err(|e| io_error(path, &e))?;
        count += page.memories.len();
        match page.next {
            Some(next) => after = next,
            None => break,
        }
    }
    file.as_file().sync_all().map_err(|e| io_error(path, &e))?;
    file.persist(path).map_err(|e| io_error(path, &e.error))?;
    if ctx.out.json {
        return ctx
            .out
            .json(&json!({"path": path.display().to_string(), "count": count, "scopes": scopes}));
    }
    ctx.out.line(&format!(
        "Exported {count} memor{} to {}",
        if count == 1 { "y" } else { "ies" },
        path.display()
    ))
}
