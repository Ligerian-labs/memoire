//! `supersede` and `forget`: taking a memory out of recall, as history
//! (superseded, forgotten) or, with `--purge`, for good.
use crate::model::{self, Memory, Status};
use crate::store::{self, Batch, NewRevision, RevisionStatus, Store};
use crate::verbs::{carried, conflict, not_current, not_found, purged_at, read_memory, written};
use crate::{Backend, Error, Labels, Refusal};

/// A purge deletes every row of a memory and merges the search index.
const PURGE_TIMEOUT_MS: u64 = 30_000;

fn cycle(id: &str, by: &str) -> Refusal {
    Refusal::Cycle {
        id: id.to_owned(),
        by: by.to_owned(),
    }
}

/// Reads both sides of a `supersede` and checks it may happen: both
/// exist, `id` is current, `by` is not forgotten, and `by` is not already
/// replaced, through `superseded_by`, by `id`.
async fn supersede_pair<B: Backend>(
    backend: &B,
    id: &str,
    by: &str,
) -> Result<(Memory, Memory), Error<B::Error>> {
    let mut batch = Batch::new();
    let old = store::select_memory(&mut batch, id);
    let old_purge = store::select_purge(&mut batch, id);
    let new = store::select_memory(&mut batch, by);
    let new_purge = store::select_purge(&mut batch, by);
    let chain = store::select_chain(&mut batch, by);
    let results = Store { backend }.read(batch).await?;
    let Some(old) = store::one_memory(&results[old])? else {
        return Err(not_found(id, purged_at(&results[old_purge])?).into());
    };
    let Some(new) = store::one_memory(&results[new])? else {
        return Err(not_found(by, purged_at(&results[new_purge])?).into());
    };
    if old.id == new.id {
        // `id` names the same memory as `by`: it would supersede itself.
        return Err(cycle(id, id).into());
    }
    if !old.status.is_current() {
        return Err(not_current(&old).into());
    }
    if new.status == Status::Forgotten {
        return Err(not_current(&new).into());
    }
    for row in store::rows(&results[chain]) {
        if row.text("id")? == old.id {
            return Err(cycle(id, by).into());
        }
    }
    Ok((old, new))
}

/// `supersede`: the old memory and the one that replaces it, as the
/// write leaves them.
pub async fn supersede<B: Backend>(
    backend: &B,
    labels: &Labels,
    id: &str,
    by: &str,
    reason: &str,
) -> Result<(Memory, Memory), Error<B::Error>> {
    labels.check()?;
    let id = &model::memory_id("id", id)?;
    let by = &model::memory_id("by", by)?;
    let reason = model::reason(reason)?;
    model::guard_secrets(
        [("reason", reason.as_str())]
            .into_iter()
            .chain(labels.fields()),
    )?;
    // Checked here for a clear error, and again by the write's guard,
    // which a concurrent supersede elsewhere in the chain cannot outrun.
    let (old, new) = supersede_pair(backend, id, by).await?;

    let author = labels.actor(backend).await?;
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
    let results = match (Store { backend }).write(batch).await {
        Ok(results) => results,
        // Whatever changed in between: say what is true now.
        Err(e) if store::guard_failed(&e, old_guard) || store::guard_failed(&e, new_guard) => {
            return Err(match supersede_pair(backend, id, by).await {
                Err(now) => now,
                Ok(_) if store::guard_failed(&e, old_guard) => conflict(backend, &old.id).await,
                Ok(_) => conflict(backend, &new.id).await,
            });
        }
        Err(e) => return Err(e),
    };
    let decode = |index: usize| {
        store::one_memory(&results[index])?
            .ok_or_else(|| store::invalid_response("the written memories"))
    };
    Ok((decode(old_row)?, decode(new_row)?))
}

/// `forget` without `--purge`: a tombstone. The search row stays, so
/// `recall --status forgotten` still finds it.
pub async fn forget<B: Backend>(
    backend: &B,
    labels: &Labels,
    id: &str,
    reason: &str,
) -> Result<Memory, Error<B::Error>> {
    labels.check()?;
    let id = &model::memory_id("id", id)?;
    let reason = model::reason(reason)?;
    model::guard_secrets(
        [("reason", reason.as_str())]
            .into_iter()
            .chain(labels.fields()),
    )?;
    let memory = read_memory(backend, id).await?;
    if memory.status == Status::Forgotten {
        return Err(not_current(&memory).into());
    }
    let author = labels.actor(backend).await?;
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
    let results = match (Store { backend }).write(batch).await {
        Ok(results) => results,
        Err(e) if store::guard_failed(&e, guarded) => return Err(conflict(backend, id).await),
        Err(e) => return Err(e),
    };
    Ok(written(&results)?)
}

/// `forget --purge`: every row about the memory goes, whatever its
/// status, with the text other memories' rows hold about it, and one
/// `purge` event without scope, rev or summary stays. `--reason` is
/// checked like any reason but not stored: it would be text about the
/// memory. The batch's first statement fails when the memory is already
/// gone, so a second purge racing the first leaves the first's event.
pub async fn purge<B: Backend>(
    backend: &B,
    labels: &Labels,
    id: &str,
    reason: &str,
) -> Result<(), Error<B::Error>> {
    labels.check()?;
    let id = &model::memory_id("id", id)?;
    let reason = model::reason(reason)?;
    model::guard_secrets(
        [("reason", reason.as_str())]
            .into_iter()
            .chain(labels.fields()),
    )?;
    let memory: Memory = read_memory(backend, id).await?;
    let author = labels.actor(backend).await?;
    let mut batch = Batch::new();
    batch.timeout_ms(PURGE_TIMEOUT_MS);
    let guarded = store::purge(&mut batch, &memory.id, &author);
    match (Store { backend }).write(batch).await {
        Ok(_) => Ok(()),
        Err(e) if store::guard_failed(&e, guarded) => {
            Err(read_memory(backend, id).await.err().unwrap_or(e))
        }
        Err(e) => Err(e),
    }
}
