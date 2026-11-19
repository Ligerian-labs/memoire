//! `supersede` and `forget`: taking a memory out of recall, as history
//! (superseded, forgotten) or, with `--purge`, for good.
use super::model::{self, Memory, Status};
use super::store::{self, Batch, NewRevision, RevisionStatus};
use super::verbs::{
    carried, conflict, not_current, not_found, print_memory, purged_at, read_memory, written,
};
use super::{Context, render};
use crate::error::{Error, Result, exit};
use serde_json::json;

/// A purge deletes every row of a memory and merges the search index.
const PURGE_TIMEOUT_MS: u64 = 30_000;

fn cycle(message: String) -> Error {
    Error::local("memory_cycle", exit::CONFLICT, message)
}

/// Reads both sides of a `supersede` and checks it may happen: both
/// exist, `id` is current, `by` is not forgotten, and `by` is not already
/// replaced, through `superseded_by`, by `id`.
fn supersede_pair(ctx: &Context<'_>, id: &str, by: &str) -> Result<(Memory, Memory)> {
    let mut batch = Batch::new();
    let old = store::select_memory(&mut batch, id);
    let old_purge = store::select_purge(&mut batch, id);
    let new = store::select_memory(&mut batch, by);
    let new_purge = store::select_purge(&mut batch, by);
    let chain = store::select_chain(&mut batch, by);
    let results = ctx.store().read(batch)?;
    let Some(old) = store::one_memory(&results[old])? else {
        return Err(not_found(id, purged_at(&results[old_purge])?));
    };
    let Some(new) = store::one_memory(&results[new])? else {
        return Err(not_found(by, purged_at(&results[new_purge])?));
    };
    if old.id == new.id {
        return Err(cycle(format!("memory {id} cannot supersede itself")));
    }
    if !old.status.is_current() {
        return Err(not_current(&old));
    }
    if new.status == Status::Forgotten {
        return Err(not_current(&new));
    }
    for row in store::rows(&results[chain]) {
        if row.text("id")? == old.id {
            return Err(cycle(format!(
                "superseding {id} by {by} would close a cycle: {by} is already replaced by {id}"
            )));
        }
    }
    Ok((old, new))
}

/// `supersede`.
pub fn supersede(ctx: &Context<'_>, id: &str, by: &str, reason: &str) -> Result<()> {
    let reason = model::reason(reason)?;
    model::guard_secrets(
        [("reason", reason.as_str())]
            .into_iter()
            .chain(ctx.labels()),
    )?;
    // Checked here for a clear error, and again by the write's guard,
    // which a concurrent supersede elsewhere in the chain cannot outrun.
    let (old, new) = supersede_pair(ctx, id, by)?;

    let author = ctx.actor()?;
    let mut batch = Batch::new();
    let old_guard = store::insert_revision_when(
        &mut batch,
        &NewRevision {
            superseded_by: Some(&new.id),
            ..carried(&old, &author, &reason)
        },
        RevisionStatus::Set(Status::Superseded),
        Some(old.rev),
        store::SUPERSEDER_DOES_NOT_REACH,
    );
    let new_guard = store::insert_revision(
        &mut batch,
        &NewRevision {
            supersedes: Some(&old.id),
            ..carried(&new, &author, &reason)
        },
        RevisionStatus::Keep,
        Some(new.rev),
    );
    for memory in [&old, &new] {
        store::update_memory(&mut batch, &memory.id, memory.rev, false);
    }
    for memory in [&old, &new] {
        store::update_status(&mut batch, &memory.id);
    }
    store::append_event(
        &mut batch,
        &old.id,
        "supersede",
        &author,
        &format!("by {}: {reason}", new.id),
    );
    let old_row = store::select_memory(&mut batch, &old.id);
    let new_row = store::select_memory(&mut batch, &new.id);
    let results = match ctx.store().write(batch) {
        Ok(results) => results,
        // Whatever changed in between: say what is true now.
        Err(e) if store::guard_failed(&e, old_guard) || store::guard_failed(&e, new_guard) => {
            return Err(match supersede_pair(ctx, id, by) {
                Err(now) => now,
                Ok(_) if store::guard_failed(&e, old_guard) => conflict(ctx, &old.id),
                Ok(_) => conflict(ctx, &new.id),
            });
        }
        Err(e) => return Err(e),
    };
    let decode = |index: usize| {
        store::one_memory(&results[index])?
            .ok_or_else(|| store::invalid_response("the written memories"))
    };
    let (old, new) = (decode(old_row)?, decode(new_row)?);
    if ctx.out.json {
        return ctx.out.json(&json!({"superseded": old, "by": new}));
    }
    ctx.out.line(&render::superseded(&old, &new))
}

/// `forget` without `--purge`: a tombstone. The search row stays, so
/// `recall --status forgotten` still finds it.
pub fn forget(ctx: &Context<'_>, id: &str, reason: &str) -> Result<()> {
    let reason = model::reason(reason)?;
    model::guard_secrets(
        [("reason", reason.as_str())]
            .into_iter()
            .chain(ctx.labels()),
    )?;
    let memory = read_memory(ctx, id)?;
    if memory.status == Status::Forgotten {
        return Err(not_current(&memory));
    }
    let author = ctx.actor()?;
    let mut batch = Batch::new();
    let guarded = store::insert_revision(
        &mut batch,
        &carried(&memory, &author, &reason),
        RevisionStatus::Set(Status::Forgotten),
        Some(memory.rev),
    );
    store::update_memory(&mut batch, id, memory.rev, false);
    store::mark_forgotten(&mut batch, id);
    store::update_status(&mut batch, id);
    store::append_event(&mut batch, id, "forget", &author, &reason);
    store::select_memory(&mut batch, id);
    let results = match ctx.store().write(batch) {
        Ok(results) => results,
        Err(e) if store::guard_failed(&e, guarded) => return Err(conflict(ctx, id)),
        Err(e) => return Err(e),
    };
    print_memory(ctx, &written(&results)?)
}

/// `forget --purge`: every row about the memory goes, whatever its
/// status, with the text other memories' rows hold about it, and one
/// `purge` event without scope, rev or summary stays. `--reason` is
/// checked like any reason but not stored: it would be text about the
/// memory. The batch's first statement fails when the memory is already
/// gone, so a second purge racing the first leaves the first's event.
pub fn purge(ctx: &Context<'_>, id: &str, reason: &str) -> Result<()> {
    let reason = model::reason(reason)?;
    model::guard_secrets(
        [("reason", reason.as_str())]
            .into_iter()
            .chain(ctx.labels()),
    )?;
    let memory: Memory = read_memory(ctx, id)?;
    let author = ctx.actor()?;
    let mut batch = Batch::new();
    batch.timeout_ms(PURGE_TIMEOUT_MS);
    let guarded = store::purge(&mut batch, &memory.id, &author);
    match ctx.store().write(batch) {
        Ok(_) => {}
        Err(e) if store::guard_failed(&e, guarded) => {
            return Err(read_memory(ctx, id).err().unwrap_or(e));
        }
        Err(e) => return Err(e),
    }
    if ctx.out.json {
        return ctx.out.json(&json!({"id": memory.id, "purged": true}));
    }
    ctx.out
        .line(&format!("Purged {} and its history", memory.id))
}
