//! `log` and `export`: what happened in the store, and its memories as a
//! file. Both only read, so a read-only key can use them.
use super::cursor::{self, Cursor, Cursors, FIRST_LOOK_SECONDS};
use super::model::Event;
use super::store::{self, After, Batch, EventFilter};
use super::{Context, render};
use crate::config::io_error;
use crate::error::{Error, Result, exit};
use serde_json::json;
use std::io::Write as _;
use std::path::Path;

/// Memories per export page.
const EXPORT_PAGE: i64 = 500;

/// `log`.
pub fn log(ctx: &Context<'_>, args: &crate::cli::LogArgs) -> Result<()> {
    let scopes = (!args.scopes.is_empty()).then_some(args.scopes.as_slice());
    if args.unseen {
        let reader = crate::cli::reader_name(args.reader.as_deref().unwrap_or_default())
            .map_err(|message| Error::local("invalid_argument", exit::USAGE, message))?;
        return unseen(ctx, scopes, reader.as_deref(), args.limit);
    }
    let mut batch = Batch::new();
    let events = store::select_events(
        &mut batch,
        &EventFilter {
            scopes,
            since_seconds: args.since,
            after: After::Start,
        },
        args.limit,
    );
    let results = ctx.store().read(batch)?;
    let events = store::all(&results[events], |row| row.event())?;
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
        ctx.client.server(),
        &ctx.db,
        scopes.unwrap_or_default(),
        cursor::reader(reader, &ctx.session),
    );
    let saved = cursors.saved(&key)?;
    let filter = EventFilter {
        scopes,
        since_seconds: None,
        after: saved.as_ref().map_or(After::FirstLook, After::Cursor),
    };
    // The store id, the events and where they start in one read, so they
    // agree.
    let mut batch = Batch::new();
    let store_id = store::select_store_id(&mut batch);
    let events = store::select_events(&mut batch, &filter, limit);
    let counted = store::count_events(&mut batch, &filter);
    let results = ctx.store().read(batch)?;
    let store_id = store::store_id_of(&results[store_id])?;
    let events = store::all(&results[events], |row| row.event())?;
    let row = store::rows(&results[counted])
        .next()
        .ok_or_else(|| store::invalid_response("the count of events"))?;
    let (matching, after, top) = (row.int("events")?, row.int("after")?, row.int("top")?);
    // A cursor of another store (the database was created again) is a
    // first look, as the query took it.
    let first = saved.is_none_or(|c| c.store_id != store_id);
    let through = top.max(after);
    // The two statements read the clock apart: an event that crossed the
    // first look's start between them is in one and not the other.
    let skipped = (matching - events.len() as i64).max(0);
    if ctx.out.json {
        ctx.out.json(&json!({
            "events": events,
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
        print_events(ctx, &events, "No new events.")?;
        if skipped > 0 {
            ctx.out.note(&format!(
                "{skipped} older unseen events skipped; raise --limit to see more next time"
            ));
        }
    }
    cursors.advance(
        &key,
        Cursor {
            store_id,
            seq: through,
        },
    )
}

/// `export`: JSON Lines, one memory object per line, in `seq` order,
/// every status but `forgotten`. Written to a private temporary file
/// beside the target and renamed over it once complete.
pub fn export(ctx: &Context<'_>, scopes: &[String], path: &Path) -> Result<()> {
    let scopes = (!scopes.is_empty()).then_some(scopes);
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|e| io_error(dir, &e))?;
    let mut count = 0;
    let mut after = 0;
    loop {
        let mut batch = Batch::new();
        let page = store::select_export_page(&mut batch, scopes, after, EXPORT_PAGE);
        let results = ctx.store().read(batch)?;
        let memories = store::all(&results[page], |row| row.memory())?;
        let mut lines = String::new();
        for memory in &memories {
            lines.push_str(&serde_json::to_string(memory).expect("memories serialize"));
            lines.push('\n');
        }
        file.write_all(lines.as_bytes())
            .map_err(|e| io_error(path, &e))?;
        count += memories.len();
        match memories.last() {
            Some(last) if memories.len() as i64 == EXPORT_PAGE => after = last.seq,
            _ => break,
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
