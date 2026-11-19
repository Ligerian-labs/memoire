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
use super::Context;
use super::model::{self, Memory, Status};
use super::store::{self, Batch};
use super::verbs::{not_current, print_memory, read_memory, written};
use crate::error::{Error, Result, exit};
use memoire_api::StatementResult;

/// Sends `batch`, whose statement `guarded` fails when memory `id` is
/// gone (reported as not found) or no longer as it was read (reported by
/// `still_there`, from the memory as it is now, in the order the guard's
/// conditions are documented).
fn write_guarded(
    ctx: &Context<'_>,
    id: &str,
    batch: Batch,
    guarded: usize,
    still_there: impl FnOnce(Memory) -> Option<Error>,
) -> Result<Vec<StatementResult>> {
    match ctx.store().write(batch) {
        Ok(results) => Ok(results),
        Err(e) if store::guard_failed(&e, guarded) => match read_memory(ctx, id) {
            Err(gone) => Err(gone),
            Ok(memory) => Err(still_there(memory).unwrap_or(e)),
        },
        Err(e) => Err(e),
    }
}

/// The rev whose text a confirm or dispute is about: `--rev`, else the rev
/// just read. Refused when the text changed after it.
fn text_read_at(memory: &Memory, rev: Option<i64>) -> Result<i64> {
    let read = rev.unwrap_or(memory.rev);
    if read > memory.rev {
        return Err(Error::local(
            "invalid_argument",
            exit::USAGE,
            format!(
                "--rev {read} is past memory {}'s current rev {}",
                memory.id, memory.rev
            ),
        ));
    }
    if memory.content_rev > read {
        return Err(text_moved(memory, read));
    }
    Ok(read)
}

/// The text of `memory` changed after rev `read`.
fn text_moved(memory: &Memory, read: i64) -> Error {
    Error::local(
        "memory_conflict",
        exit::CONFLICT,
        format!(
            "the text of memory {} changed since rev {read}: it is at rev {}; read it again before judging it",
            memory.id, memory.rev
        ),
    )
    .with("id", memory.id.clone())
    .with("current_rev", memory.rev)
}

/// `confirm`.
pub fn confirm(ctx: &Context<'_>, id: &str, evidence: Vec<String>, rev: Option<i64>) -> Result<()> {
    let evidence = model::evidence(evidence)?;
    model::guard_secrets(
        evidence
            .iter()
            .map(|e| ("evidence", e.as_str()))
            .chain(ctx.labels()),
    )?;
    let memory = read_memory(ctx, id)?;
    if !memory.status.is_current() {
        return Err(not_current(&memory));
    }
    let read = text_read_at(&memory, rev)?;
    let actor = ctx.actor()?;
    // Against the content author, not the latest revision's: retagging
    // someone's text does not let them confirm it themselves.
    if actor.principal == memory.content_principal && actor.session == memory.content_session {
        let message = if actor.session.is_empty() {
            format!(
                "you wrote the text of memory {id}, and MEMOIRE_SESSION is not set, so every run with this key counts as that same writer; set MEMOIRE_SESSION (or --session) to an id unique to each run"
            )
        } else {
            format!(
                "you wrote the text of memory {id} in this session; a confirmation needs another principal or session"
            )
        };
        return Err(Error::local("memory_self_confirm", exit::REJECTED, message));
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
    let results = write_guarded(ctx, id, batch, guarded, |now| {
        Some(if now.status.is_current() {
            text_moved(&now, read)
        } else {
            not_current(&now)
        })
    })?;
    print_memory(ctx, &written(&results)?)
}

/// `dispute`.
pub fn dispute(
    ctx: &Context<'_>,
    id: &str,
    reason: &str,
    evidence: Vec<String>,
    rev: Option<i64>,
) -> Result<()> {
    let reason = model::reason(reason)?;
    let evidence = model::evidence(evidence)?;
    model::guard_secrets(
        [("reason", reason.as_str())]
            .into_iter()
            .chain(evidence.iter().map(|e| ("evidence", e.as_str())))
            .chain(ctx.labels()),
    )?;
    let memory = read_memory(ctx, id)?;
    if memory.status == Status::Forgotten {
        return Err(not_current(&memory));
    }
    let read = text_read_at(&memory, rev)?;
    let actor = ctx.actor()?;
    let mut batch = Batch::new();
    let guarded = store::insert_dispute(&mut batch, id, read, &actor, &reason, &evidence);
    store::touch(&mut batch, id);
    store::update_status(&mut batch, id);
    store::append_event(&mut batch, id, "dispute", &actor, &reason);
    store::select_memory(&mut batch, id);
    let results = write_guarded(ctx, id, batch, guarded, |now| {
        Some(if now.status == Status::Forgotten {
            not_current(&now)
        } else {
            text_moved(&now, read)
        })
    })?;
    print_memory(ctx, &written(&results)?)
}

fn no_open_dispute(id: &str) -> Error {
    Error::local(
        "memory_no_open_dispute",
        exit::CONFLICT,
        format!("memory {id} has no open dispute"),
    )
}

/// `resolve`. Runs on a superseded memory too: its disputes can still be
/// closed.
pub fn resolve(ctx: &Context<'_>, id: &str, reason: &str) -> Result<()> {
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
    if memory.open_disputes == 0 {
        return Err(no_open_dispute(id));
    }
    let actor = ctx.actor()?;
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
    let results = write_guarded(ctx, id, batch, guarded, |now| {
        if now.status == Status::Forgotten {
            Some(not_current(&now))
        } else {
            (now.open_disputes == 0).then(|| no_open_dispute(id))
        }
    })?;
    print_memory(ctx, &written(&results)?)
}
