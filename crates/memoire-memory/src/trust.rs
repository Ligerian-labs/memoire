//! `confirm`, `dispute` and `resolve`: what agents say about a memory's
//! text. They are rows of their own beside the revisions, attached to the
//! rev they were made on; the status statement decides what they mean.
//! Each also moves the memory's `updated_at`: a memory someone just
//! checked is as fresh, for recall, as one just written. Its rev and
//! author stay.
//!
//! None of them writes a revision, so none needs a rev guard. The first
//! statement of each batch fails unless the memory is still as the verb
//! checked it: there, still eligible (confirm: not superseded or
//! forgotten; dispute and resolve: not forgotten), for confirm and dispute
//! with its text unchanged since the rev the agent read, and for resolve
//! with an open dispute. A verdict never lands on text, or a state, its
//! writer did not see.
use crate::model::{self, Memory, Status};
use crate::store::{self, Batch, Store};
use crate::verbs::{not_current, read_memory, written};
use crate::{Backend, Error, Invalid, Labels, Refusal};
use memoire_api::StatementResult;

/// Sends `batch`, whose statement `guarded` fails when memory `id` is
/// gone (reported as not found) or no longer as it was read (reported by
/// `still_there`, from the memory as it is now, in the order the guard's
/// conditions are documented).
async fn write_guarded<B: Backend>(
    backend: &B,
    id: &str,
    batch: Batch,
    guarded: usize,
    still_there: impl FnOnce(Memory) -> Option<Refusal>,
) -> Result<Vec<StatementResult>, Error<B::Error>> {
    match (Store { backend }).write(batch).await {
        Ok(results) => Ok(results),
        Err(e) if store::guard_failed(&e, guarded) => match read_memory(backend, id).await {
            Err(gone) => Err(gone),
            Ok(memory) => Err(still_there(memory).map_or(e, Error::Refused)),
        },
        Err(e) => Err(e),
    }
}

/// The rev whose text a confirm or dispute is about: `--rev`, else the rev
/// just read. Refused when the text changed after it.
fn text_read_at(memory: &Memory, rev: Option<i64>) -> Result<i64, Refusal> {
    let read = rev.unwrap_or(memory.rev);
    if read > memory.rev {
        return Err(Refusal::InvalidArgument(Invalid::RevPastCurrent {
            rev: read,
            id: memory.id.clone(),
            current_rev: memory.rev,
        }));
    }
    if memory.content_rev > read {
        return Err(text_moved(memory, read));
    }
    Ok(read)
}

/// The text of `memory` changed after rev `read`.
fn text_moved(memory: &Memory, read: i64) -> Refusal {
    Refusal::Conflict {
        id: memory.id.clone(),
        current_rev: memory.rev,
        since_rev: Some(read),
    }
}

/// `confirm`: the memory as the confirmation leaves it.
pub async fn confirm<B: Backend>(
    backend: &B,
    labels: &Labels,
    id: &str,
    evidence: Vec<String>,
    rev: Option<i64>,
) -> Result<Memory, Error<B::Error>> {
    let evidence = model::evidence(evidence)?;
    model::guard_secrets(
        evidence
            .iter()
            .map(|e| ("evidence", e.as_str()))
            .chain(labels.fields()),
    )?;
    let memory = read_memory(backend, id).await?;
    if !memory.status.is_current() {
        return Err(not_current(&memory).into());
    }
    let read = text_read_at(&memory, rev)?;
    let actor = labels.actor(backend).await?;
    // Against the content author, not the latest revision's: retagging
    // someone's text does not let them confirm it themselves.
    if actor.principal == memory.content_principal && actor.session == memory.content_session {
        return Err(Refusal::SelfConfirm {
            id: id.to_owned(),
            without_session: actor.session.is_empty(),
        }
        .into());
    }
    let mut batch = Batch::new();
    let guarded = store::insert_confirmation(&mut batch, id, read, &actor, &evidence);
    store::touch(&mut batch, id);
    store::update_status(&mut batch, id);
    store::append_event(
        &mut batch,
        id,
        "confirm",
        &actor,
        &format!("confirmed rev {read}"),
    );
    store::select_memory(&mut batch, id);
    let results = write_guarded(backend, id, batch, guarded, |now| {
        Some(if now.status.is_current() {
            text_moved(&now, read)
        } else {
            not_current(&now)
        })
    })
    .await?;
    Ok(written(&results)?)
}

/// `dispute`: the memory as the dispute leaves it.
pub async fn dispute<B: Backend>(
    backend: &B,
    labels: &Labels,
    id: &str,
    reason: &str,
    evidence: Vec<String>,
    rev: Option<i64>,
) -> Result<Memory, Error<B::Error>> {
    let reason = model::reason(reason)?;
    let evidence = model::evidence(evidence)?;
    model::guard_secrets(
        [("reason", reason.as_str())]
            .into_iter()
            .chain(evidence.iter().map(|e| ("evidence", e.as_str())))
            .chain(labels.fields()),
    )?;
    let memory = read_memory(backend, id).await?;
    if memory.status == Status::Forgotten {
        return Err(not_current(&memory).into());
    }
    let read = text_read_at(&memory, rev)?;
    let actor = labels.actor(backend).await?;
    let mut batch = Batch::new();
    let guarded = store::insert_dispute(&mut batch, id, read, &actor, &reason, &evidence);
    store::touch(&mut batch, id);
    store::update_status(&mut batch, id);
    store::append_event(&mut batch, id, "dispute", &actor, &reason);
    store::select_memory(&mut batch, id);
    let results = write_guarded(backend, id, batch, guarded, |now| {
        Some(if now.status == Status::Forgotten {
            not_current(&now)
        } else {
            text_moved(&now, read)
        })
    })
    .await?;
    Ok(written(&results)?)
}

fn no_open_dispute(id: &str) -> Refusal {
    Refusal::NoOpenDispute { id: id.to_owned() }
}

/// `resolve`: the memory as the resolution leaves it. Runs on a
/// superseded memory too: its disputes can still be closed.
pub async fn resolve<B: Backend>(
    backend: &B,
    labels: &Labels,
    id: &str,
    reason: &str,
) -> Result<Memory, Error<B::Error>> {
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
    if memory.open_disputes == 0 {
        return Err(no_open_dispute(id).into());
    }
    let actor = labels.actor(backend).await?;
    let mut batch = Batch::new();
    // An UPDATE cannot fail on its own, so the event goes first and fails
    // unless the memory is not forgotten and still has an open dispute.
    let guarded = store::append_guarded_event(
        &mut batch,
        id,
        "resolve",
        &actor,
        &reason,
        &format!("{} AND {}", store::NOT_FORGOTTEN, store::OPEN_DISPUTE),
    );
    store::resolve_disputes(&mut batch, id, &actor, &reason);
    store::touch(&mut batch, id);
    store::update_status(&mut batch, id);
    store::select_memory(&mut batch, id);
    let results = write_guarded(backend, id, batch, guarded, |now| {
        if now.status == Status::Forgotten {
            Some(not_current(&now))
        } else {
            (now.open_disputes == 0).then(|| no_open_dispute(id))
        }
    })
    .await?;
    Ok(written(&results)?)
}
