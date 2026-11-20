//! `log` and `export`: what happened in the store, and its memories page
//! by page. Both only read, so a read-only key can use them.
use crate::model::{Event, Memory};
use crate::store::{self, After, Batch, Cursor, EventFilter, Store};
use crate::{Backend, Error};

/// Memories per export page.
pub const EXPORT_PAGE: i64 = 500;

/// `log`: the newest `limit` events of `scopes` (all when `None`), of the
/// last `since_seconds` when given, newest first.
pub async fn log<B: Backend>(
    backend: &B,
    scopes: Option<&[String]>,
    since_seconds: Option<u64>,
    limit: i64,
) -> Result<Vec<Event>, Error<B::Error>> {
    let mut batch = Batch::new();
    let events = store::select_events(
        &mut batch,
        &EventFilter {
            scopes,
            since_seconds,
            after: After::Start,
        },
        limit,
    );
    let results = Store { backend }.read(batch).await?;
    Ok(store::all(&results[events], |row| row.event())?)
}

/// What `log --unseen` read.
#[derive(Clone, Debug, PartialEq)]
pub struct Unseen {
    /// The events after the cursor, newest first.
    pub events: Vec<Event>,
    /// The store they come from.
    pub store_id: String,
    /// Whether this was a first look: no cursor, or one of another store.
    pub first: bool,
    /// The `seq` the read started after.
    pub after: i64,
    /// The `seq` the cursor moves to: the store's newest event.
    pub through: i64,
    /// Unseen events past `limit`, left out.
    pub skipped: i64,
}

impl Unseen {
    /// The cursor to save once the events are shown.
    pub fn cursor(&self) -> Cursor {
        Cursor {
            store_id: self.store_id.clone(),
            seq: self.through,
        }
    }
}

/// `log --unseen`: the events after `saved`, or of the last
/// [`store::FIRST_LOOK_SECONDS`] on a first look. The caller shows them,
/// then moves its cursor to [`Unseen::cursor`].
pub async fn unseen<B: Backend>(
    backend: &B,
    scopes: Option<&[String]>,
    saved: Option<&Cursor>,
    limit: i64,
) -> Result<Unseen, Error<B::Error>> {
    let filter = EventFilter {
        scopes,
        since_seconds: None,
        after: saved.map_or(After::FirstLook, After::Cursor),
    };
    // The store id, the events and where they start in one read, so they
    // agree.
    let mut batch = Batch::new();
    let store_id = store::select_store_id(&mut batch);
    let events = store::select_events(&mut batch, &filter, limit);
    let counted = store::count_events(&mut batch, &filter);
    let results = Store { backend }.read(batch).await?;
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
    Ok(Unseen {
        events,
        store_id,
        first,
        after,
        through,
        skipped,
    })
}

/// One page of `export`.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportPage {
    /// In `seq` order, every status but `forgotten`.
    pub memories: Vec<Memory>,
    /// Where the next page starts, when there may be one.
    pub next: Option<i64>,
}

/// The export page of the memories of `scopes` (all when `None`) after
/// `seq` `after`; 0 for the first page.
pub async fn export_page<B: Backend>(
    backend: &B,
    scopes: Option<&[String]>,
    after: i64,
) -> Result<ExportPage, Error<B::Error>> {
    let mut batch = Batch::new();
    let page = store::select_export_page(&mut batch, scopes, after, EXPORT_PAGE);
    let results = Store { backend }.read(batch).await?;
    let memories = store::all(&results[page], |row| row.memory())?;
    let next = match memories.last() {
        Some(last) if memories.len() as i64 == EXPORT_PAGE => Some(last.seq),
        _ => None,
    };
    Ok(ExportPage { memories, next })
}
