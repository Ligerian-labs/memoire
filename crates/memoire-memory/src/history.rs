//! `log` and `export`: what happened in the store, and its memories page
//! by page. Both only read, so a read-only key can use them.
use crate::model::{self, Event, Memory};
use crate::store::{self, After, Batch, Cursor, EventFilter, Store};
use crate::{Backend, Error};
use memoire_api::memory::{MAX_EVENTS_LIMIT, MAX_EXPORT_PAGE};

/// Memories per export page.
pub const EXPORT_PAGE: i64 = MAX_EXPORT_PAGE;

/// `log`: the newest `limit` events of `scopes` (all when `None`), of the
/// last `since_seconds` when given, newest first.
pub async fn log<B: Backend>(
    backend: &B,
    scopes: Option<&[String]>,
    since_seconds: Option<u64>,
    limit: i64,
) -> Result<Vec<Event>, Error<B::Error>> {
    check_scopes(scopes)?;
    model::check_limit(limit, MAX_EVENTS_LIMIT)?;
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

fn check_scopes(scopes: Option<&[String]>) -> Result<(), crate::Refusal> {
    model::check_items("scope", scopes.unwrap_or_default(), model::parse_scope)
}

/// Which events [`events`] reads.
#[derive(Clone, Copy, Debug)]
pub struct Events<'a> {
    /// `None`: every scope.
    pub scopes: Option<&'a [String]>,
    /// Only events of the last this many seconds; only with
    /// [`After::Start`].
    pub since_seconds: Option<u64>,
    pub after: After<'a>,
    pub limit: i64,
}

/// What [`events`] read.
#[derive(Clone, Debug, PartialEq)]
pub struct EventsPage {
    /// The newest `limit` events after `after`, newest first.
    pub events: Vec<Event>,
    pub store_id: String,
    /// The `seq` the read started after.
    pub after: i64,
    /// The store's highest `seq`, whatever the filter.
    pub top: i64,
    /// How many events pass the filter after `after`.
    pub matching: i64,
}

/// The log as a reader that keeps its own cursor reads it: the events
/// from where `after` says, and where the read started, the store's top
/// and how many passed the filter, all from one read.
pub async fn events<B: Backend>(
    backend: &B,
    args: Events<'_>,
) -> Result<EventsPage, Error<B::Error>> {
    check_scopes(args.scopes)?;
    model::check_limit(args.limit, MAX_EVENTS_LIMIT)?;
    if let After::Cursor(cursor) = args.after {
        model::check_store_id(&cursor.store_id)?;
        if cursor.seq < 0 {
            return Err(model::invalid_field("/after", "out_of_range").into());
        }
    }
    if args.since_seconds.is_some() && !matches!(args.after, After::Start) {
        return Err(model::invalid_field("/since_seconds", "not_allowed").into());
    }
    let filter = EventFilter {
        scopes: args.scopes,
        since_seconds: args.since_seconds,
        after: args.after,
    };
    let mut batch = Batch::new();
    let store_id = store::select_store_id(&mut batch);
    let events = store::select_events(&mut batch, &filter, args.limit);
    let counted = store::count_events(&mut batch, &filter);
    let results = Store { backend }.read(batch).await?;
    let row = store::rows(&results[counted])
        .next()
        .ok_or_else(|| store::invalid_response("the count of events"))?;
    Ok(EventsPage {
        events: store::all(&results[events], |row| row.event())?,
        store_id: store::store_id_of(&results[store_id])?,
        after: row.int("after")?,
        top: row.int("top")?,
        matching: row.int("events")?,
    })
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
    check_scopes(scopes)?;
    model::check_limit(limit, MAX_EVENTS_LIMIT)?;
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
    let page = EventsPage {
        events,
        store_id,
        after: row.int("after")?,
        top: row.int("top")?,
        matching: row.int("events")?,
    };
    Ok(Unseen::of(page, saved))
}

impl Unseen {
    /// What a reader whose cursor was `saved` (`None`: a first look) read
    /// in `page`, the events after that cursor; for a reader that reads
    /// the log through [`events`] and keeps the cursor itself.
    pub fn of(page: EventsPage, saved: Option<&Cursor>) -> Self {
        // A cursor of another store (the database was created again) is a
        // first look, as the query took it.
        let first = saved.is_none_or(|c| c.store_id != page.store_id);
        let through = page.top.max(page.after);
        // The two statements read the clock apart: an event that crossed the
        // first look's start between them is in one and not the other.
        let skipped = (page.matching - page.events.len() as i64).max(0);
        Self {
            events: page.events,
            store_id: page.store_id,
            first,
            after: page.after,
            through,
            skipped,
        }
    }
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
    export(backend, scopes, after, EXPORT_PAGE).await
}

/// [`export_page`] with pages of `limit` memories, 1 to
/// [`EXPORT_PAGE`].
pub async fn export<B: Backend>(
    backend: &B,
    scopes: Option<&[String]>,
    after: i64,
    limit: i64,
) -> Result<ExportPage, Error<B::Error>> {
    check_scopes(scopes)?;
    model::check_limit(limit, EXPORT_PAGE)?;
    if after < 0 {
        return Err(model::invalid_field("/after", "out_of_range").into());
    }
    let mut batch = Batch::new();
    let page = store::select_export_page(&mut batch, scopes, after, limit);
    let results = Store { backend }.read(batch).await?;
    let memories = store::all(&results[page], |row| row.memory())?;
    let next = match memories.last() {
        Some(last) if memories.len() as i64 == limit => Some(last.seq),
        _ => None,
    };
    Ok(ExportPage { memories, next })
}
