//! The store's SQL: statement builders shared by the verbs, the batches
//! they travel in, and the decoding of the rows that come back.
//!
//! Every batch runs with `transaction: true`, so a write applies whole or
//! not at all and a read sees one state. Writes are guarded rather than
//! locked: the first statement of a revision batch fails when the memory
//! moved since it was read (see [`insert_revision`]).
use super::model::{Author, Confirmation, Dispute, Event, Memory, Revision, Status};
use super::schema;
use crate::client::Client;
use crate::error::{Error, Result, exit};
use memoire_api::{Params, QueryMode, QueryRequest, SqlValue, Statement, StatementResult};

/// The store's clock, the gateway host's, to the millisecond.
pub const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

/// The memory object's columns, from `memoire_memory_memories m`.
/// "Distinct confirmers" counts the (principal, session) pairs whose
/// confirmation covers the current text and is not its author's.
pub const MEMORY_COLUMNS: &str = "m.seq, m.id, m.rev, m.content_rev, m.content_principal, \
     m.content_session, m.kind, m.scope, m.title, \
     m.body, m.tags, m.evidence, m.status, m.supersedes, m.superseded_by, m.author_principal, \
     m.author_agent, m.author_session, m.created_at, m.updated_at, \
     (SELECT count(DISTINCT json_array(c.principal, c.session)) \
      FROM memoire_memory_confirmations c \
      WHERE c.memory_id = m.id AND c.rev >= m.content_rev \
        AND NOT (c.principal = m.content_principal AND c.session = m.content_session)) AS confirmers, \
     (SELECT count(*) FROM memoire_memory_disputes d \
      WHERE d.memory_id = m.id AND d.resolved_at IS NULL) AS open_disputes";

/// The live status of the `memoire_memory_memories` row being updated,
/// derived from its rows.
macro_rules! status_case {
    () => {
        "UPDATE memoire_memory_memories SET status = CASE \
  WHEN forgotten = 1 THEN 'forgotten' \
  WHEN superseded_by IS NOT NULL THEN 'superseded' \
  WHEN EXISTS (SELECT 1 FROM memoire_memory_disputes d \
               WHERE d.memory_id = memoire_memory_memories.id AND d.resolved_at IS NULL) THEN 'disputed' \
  WHEN EXISTS (SELECT 1 FROM memoire_memory_confirmations c \
               WHERE c.memory_id = memoire_memory_memories.id \
                 AND c.rev >= memoire_memory_memories.content_rev \
                 AND NOT (c.principal = memoire_memory_memories.content_principal \
                          AND c.session = memoire_memory_memories.content_session)) THEN 'confirmed' \
  ELSE 'inferred' END "
    };
}

/// Derives the live status of memory `?1` from its rows. Every write verb
/// runs it inside its batch; the client never computes a status.
pub const STATUS_SQL: &str = concat!(status_case!(), "WHERE id = ?1");

/// [`STATUS_SQL`] for every memory left `superseded` with no
/// `superseded_by`: what clearing the links to a purged memory leaves. It
/// finds them in the write itself, so a link set after a purge read its
/// state is rederived too.
pub const STATUS_UNLINKED_SQL: &str = concat!(
    status_case!(),
    "WHERE status = 'superseded' AND superseded_by IS NULL"
);

const REVISION_COLUMNS: &str = "memory_id, rev, kind, scope, title, body, tags, evidence, status, \
     supersedes, superseded_by, author_principal, author_agent, author_session, reason, created_at";

pub fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.to_owned())
}

pub fn int(value: i64) -> SqlValue {
    SqlValue::Integer(value)
}

pub fn opt_text(value: Option<&str>) -> SqlValue {
    value.map_or(SqlValue::Null, text)
}

/// A JSON array of strings, as tags and evidence are stored.
pub fn json_list(items: &[String]) -> SqlValue {
    SqlValue::Text(serde_json::to_string(items).expect("strings serialize"))
}

/// Statements sent together.
#[derive(Debug, Default)]
pub struct Batch {
    statements: Vec<Statement>,
    timeout_ms: Option<u64>,
}

impl Batch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a statement; its index in the batch.
    pub fn push(&mut self, sql: impl Into<String>, params: Vec<SqlValue>) -> usize {
        self.statements.push(Statement {
            sql: sql.into(),
            params: Params::Positional(params),
        });
        self.statements.len() - 1
    }

    /// Gives each statement up to `ms` milliseconds instead of the
    /// gateway's default.
    pub fn timeout_ms(&mut self, ms: u64) {
        self.timeout_ms = Some(ms);
    }

    pub fn len(&self) -> usize {
        self.statements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
    }
}

/// The store in one database.
pub struct Store<'a> {
    pub client: &'a Client,
    pub db: &'a str,
}

impl Store<'_> {
    /// Runs `batch` as one transaction; one result per statement.
    pub fn run(&self, mode: QueryMode, batch: Batch) -> Result<Vec<StatementResult>> {
        let expected = batch.len();
        let response = self
            .client
            .query(
                self.db,
                &QueryRequest {
                    mode,
                    transaction: true,
                    timeout_ms: batch.timeout_ms,
                    statements: batch.statements,
                },
            )
            .map_err(|e| self.not_found_hint(e))?;
        if response.results.len() != expected {
            return Err(invalid_response("a result per statement"));
        }
        Ok(response.results)
    }

    /// Runs a read batch behind the schema version check, statement 0 of
    /// the batch; the results of `batch`'s own statements.
    pub fn read(&self, batch: Batch) -> Result<Vec<StatementResult>> {
        let mut checked = Batch::new();
        checked.push(schema::VERSION_SQL, vec![]);
        checked.statements.extend(batch.statements);
        match self.run(QueryMode::Read, checked) {
            Ok(mut results) => {
                schema::require_current(schema::version_of(&results[0])?)?;
                results.remove(0);
                Ok(results)
            }
            Err(e) if schema::missing_store(&e) => Err(schema::not_initialized(self.db)),
            // A store of another version can fail on any later statement:
            // say which version it is rather than what SQLite tripped on.
            Err(e) if sql_statement(&e).is_some() => {
                schema::require_current(self.version()?)?;
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    /// Runs a write batch.
    pub fn write(&self, batch: Batch) -> Result<Vec<StatementResult>> {
        self.run(QueryMode::Write, batch)
    }

    /// The store's schema version; 0 before `init`.
    pub fn version(&self) -> Result<i64> {
        let mut batch = Batch::new();
        batch.push(schema::VERSION_SQL, vec![]);
        match self.run(QueryMode::Read, batch) {
            Ok(results) => schema::version_of(&results[0]),
            Err(e) if schema::missing_store(&e) => Ok(0),
            Err(e) => Err(e),
        }
    }

    fn not_found_hint(&self, error: Error) -> Error {
        if error.code() != "database_not_found" {
            return error;
        }
        Error::local(
            "database_not_found",
            exit::NOT_FOUND,
            format!(
                "no database `{}` in the team; run `memoire memory init`",
                self.db
            ),
        )
        .with("status", 404)
    }
}

/// The statement an `sql_error` names, and its SQLite code.
pub fn sql_statement(error: &Error) -> Option<(usize, &str, &str)> {
    match error {
        Error::Api { sql: Some(sql), .. } => Some((
            sql.statement,
            sql.sqlite_code.as_str(),
            sql.message.as_str(),
        )),
        _ => None,
    }
}

/// Whether a guarded statement at `index` failed its guard: a constraint
/// (primary key or `NOT NULL`) refused it.
pub fn guard_failed(error: &Error, index: usize) -> bool {
    sql_statement(error)
        .is_some_and(|(at, code, _)| at == index && code.starts_with("SQLITE_CONSTRAINT"))
}

pub fn invalid_response(what: &str) -> Error {
    Error::local(
        "invalid_response",
        exit::FAILURE,
        format!("the gateway's answer for the memory store lacks {what}"),
    )
}

/// How a new revision's `status` is set.
#[derive(Clone, Copy, Debug)]
pub enum RevisionStatus {
    /// This status.
    Set(Status),
    /// The memory's status as the batch finds it.
    Keep,
    /// After a change of title, body or kind: older confirmations no
    /// longer count, so `inferred`, or `disputed` while a dispute is open.
    TextChanged,
}

impl RevisionStatus {
    fn sql(self) -> String {
        match self {
            Self::Set(status) => format!("'{}'", status.as_str()),
            Self::Keep => "(SELECT status FROM memoire_memory_memories WHERE id = ?1)".into(),
            Self::TextChanged => "CASE WHEN EXISTS (SELECT 1 FROM memoire_memory_disputes d \
                 WHERE d.memory_id = ?1 AND d.resolved_at IS NULL) THEN 'disputed' ELSE 'inferred' END"
                .into(),
        }
    }
}

/// A revision's fields.
#[derive(Clone, Debug)]
pub struct NewRevision<'a> {
    pub id: &'a str,
    pub rev: i64,
    pub kind: &'a str,
    pub scope: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub tags: &'a [String],
    pub evidence: &'a [String],
    /// `Some`: set to this memory. `None`: as the memory row has it when
    /// the write runs (none for a new memory), never as it was read, so a
    /// link to a memory purged in between is not written back.
    pub supersedes: Option<&'a str>,
    /// As `supersedes`.
    pub superseded_by: Option<&'a str>,
    pub author: &'a Author,
    pub reason: &'a str,
}

/// For a guarded [`insert_revision`] of a `supersede`: holds unless
/// following `superseded_by` from the new superseder (`?10`) reaches the
/// memory (`?1`). `UNION` drops ids already visited, so it ends on any
/// graph, however long the chains.
pub const SUPERSEDER_DOES_NOT_REACH: &str = "NOT EXISTS (WITH RECURSIVE reach (id) AS ( \
       SELECT ?10 \
       UNION \
       SELECT m.superseded_by FROM reach \
       JOIN memoire_memory_memories m ON m.id = reach.id \
       WHERE m.superseded_by IS NOT NULL) \
     SELECT 1 FROM reach WHERE reach.id = ?1)";

/// Inserts a revision. With `read_rev`, the revision is `read_rev + 1`
/// and its `created_at` comes from the memory row at `read_rev`, so the
/// insert fails on a constraint when another writer took the rev first
/// (primary key) or the memory is gone or moved (`NOT NULL`): the caller
/// reports a conflict instead of writing over a state it never saw.
/// Without, it is revision 1 of a new memory.
pub fn insert_revision(
    batch: &mut Batch,
    revision: &NewRevision<'_>,
    status: RevisionStatus,
    read_rev: Option<i64>,
) -> usize {
    insert_revision_when(batch, revision, status, read_rev, "1")
}

/// [`insert_revision`] whose guard also needs `condition` (SQL over the
/// insert's parameters) to hold.
pub fn insert_revision_when(
    batch: &mut Batch,
    revision: &NewRevision<'_>,
    status: RevisionStatus,
    read_rev: Option<i64>,
    condition: &str,
) -> usize {
    let created_at = if read_rev.is_some() {
        format!(
            "(SELECT {NOW} FROM memoire_memory_memories WHERE id = ?1 AND rev = ?15 AND {condition})"
        )
    } else {
        NOW.to_owned()
    };
    let link = |column: &str, param: &str| {
        format!("coalesce({param}, (SELECT {column} FROM memoire_memory_memories WHERE id = ?1))")
    };
    let sql = format!(
        "INSERT INTO memoire_memory_revisions ({REVISION_COLUMNS}) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, {}, {}, {}, ?11, ?12, ?13, ?14, {created_at})",
        status.sql(),
        link("supersedes", "?9"),
        link("superseded_by", "?10"),
    );
    let mut params = vec![
        text(revision.id),
        int(revision.rev),
        text(revision.kind),
        text(revision.scope),
        text(revision.title),
        text(revision.body),
        json_list(revision.tags),
        json_list(revision.evidence),
        opt_text(revision.supersedes),
        opt_text(revision.superseded_by),
        text(&revision.author.principal),
        text(&revision.author.agent),
        text(&revision.author.session),
        text(revision.reason),
    ];
    if let Some(rev) = read_rev {
        params.push(int(rev));
    }
    batch.push(sql, params)
}

/// The memories row of a new memory, copied from its revision 1.
pub fn insert_memory(batch: &mut Batch, id: &str) -> usize {
    batch.push(
        "INSERT INTO memoire_memory_memories (id, rev, content_rev, content_principal, \
         content_session, kind, scope, title, body, tags, evidence, status, supersedes, \
         superseded_by, author_principal, author_agent, author_session, created_at, updated_at) \
         SELECT memory_id, rev, rev, author_principal, author_session, kind, scope, title, body, \
         tags, evidence, status, supersedes, superseded_by, author_principal, author_agent, \
         author_session, created_at, created_at \
         FROM memoire_memory_revisions WHERE memory_id = ?1 AND rev = 1",
        vec![text(id)],
    )
}

/// Moves the memories row from `read_rev` to the revision `read_rev + 1`
/// just inserted. A change of text also makes that revision the content
/// revision, whose author confirmations are compared with.
pub fn update_memory(batch: &mut Batch, id: &str, read_rev: i64, text_changed: bool) -> usize {
    let (content_columns, content_values) = if text_changed {
        (
            ", content_rev, content_principal, content_session",
            ", r.rev, r.author_principal, r.author_session",
        )
    } else {
        ("", "")
    };
    batch.push(
        format!(
            "UPDATE memoire_memory_memories SET (rev, kind, scope, title, body, tags, evidence, \
             supersedes, superseded_by, author_principal, author_agent, author_session, \
             updated_at{content_columns}) = \
             (SELECT r.rev, r.kind, r.scope, r.title, r.body, r.tags, r.evidence, r.supersedes, \
             r.superseded_by, r.author_principal, r.author_agent, r.author_session, \
             r.created_at{content_values} \
             FROM memoire_memory_revisions r WHERE r.memory_id = ?1 AND r.rev = ?2 + 1) \
             WHERE id = ?1 AND rev = ?2"
        ),
        vec![text(id), int(read_rev)],
    )
}

/// Moves memory `id`'s `updated_at` to the store's now, for a write that
/// is about the memory without being a revision of it (a confirmation, a
/// dispute, a resolution). Kept apart from [`STATUS_SQL`], which a purge
/// also runs on linked memories whose time must not move.
pub fn touch(batch: &mut Batch, id: &str) -> usize {
    batch.push(
        format!("UPDATE memoire_memory_memories SET updated_at = {NOW} WHERE id = ?1"),
        vec![text(id)],
    )
}

/// The status statement for memory `id`.
pub fn update_status(batch: &mut Batch, id: &str) -> usize {
    batch.push(STATUS_SQL, vec![text(id)])
}

/// Replaces memory `id`'s search row with its current title, body, tags
/// and scope. FTS5 has no update that keeps the index exact without a
/// trigger, so the row is deleted and inserted again.
pub fn sync_search(batch: &mut Batch, id: &str) {
    batch.push(
        "DELETE FROM memoire_memory_fts \
         WHERE rowid = (SELECT seq FROM memoire_memory_memories WHERE id = ?1)",
        vec![text(id)],
    );
    insert_search(batch, id);
}

/// Inserts memory `id`'s search row.
pub fn insert_search(batch: &mut Batch, id: &str) -> usize {
    batch.push(
        "INSERT INTO memoire_memory_fts (rowid, title, body, tags, scope) \
         SELECT m.seq, m.title, m.body, \
         coalesce((SELECT group_concat(t.value, ' ') FROM json_each(m.tags) t), ''), m.scope \
         FROM memoire_memory_memories m WHERE m.id = ?1",
        vec![text(id)],
    )
}

/// Appends an event on memory `id` at its current rev and scope.
pub fn append_event(
    batch: &mut Batch,
    id: &str,
    verb: &str,
    actor: &Author,
    summary: &str,
) -> usize {
    batch.push(
        format!(
            "INSERT INTO memoire_memory_events \
             (memory_id, verb, rev, scope, principal, agent, session, summary, created_at) \
             SELECT m.id, ?2, m.rev, m.scope, ?3, ?4, ?5, ?6, {NOW} \
             FROM memoire_memory_memories m WHERE m.id = ?1"
        ),
        vec![
            text(id),
            text(verb),
            text(&actor.principal),
            text(&actor.agent),
            text(&actor.session),
            text(summary),
        ],
    )
}

/// Selects memory `id` as a memory object.
pub fn select_memory(batch: &mut Batch, id: &str) -> usize {
    batch.push(
        format!("SELECT {MEMORY_COLUMNS} FROM memoire_memory_memories m WHERE m.id = ?1"),
        vec![text(id)],
    )
}

/// Selects when memory `id` was purged, if it was.
pub fn select_purge(batch: &mut Batch, id: &str) -> usize {
    batch.push(
        "SELECT created_at FROM memoire_memory_events \
         WHERE memory_id = ?1 AND verb = 'purge' ORDER BY seq DESC LIMIT 1",
        vec![text(id)],
    )
}

pub fn select_confirmations(batch: &mut Batch, id: &str) -> usize {
    batch.push(
        "SELECT c.rev, c.principal, c.agent, c.session, c.evidence, c.created_at, \
         (c.rev >= m.content_rev \
          AND NOT (c.principal = m.content_principal AND c.session = m.content_session)) AS counts \
         FROM memoire_memory_confirmations c JOIN memoire_memory_memories m ON m.id = c.memory_id \
         WHERE c.memory_id = ?1 ORDER BY c.id",
        vec![text(id)],
    )
}

pub fn select_disputes(batch: &mut Batch, id: &str) -> usize {
    batch.push(
        "SELECT rev, principal, agent, session, reason, evidence, created_at, resolved_at, \
         resolution FROM memoire_memory_disputes WHERE memory_id = ?1 ORDER BY id",
        vec![text(id)],
    )
}

pub fn select_revisions(batch: &mut Batch, id: &str) -> usize {
    batch.push(
        format!(
            "SELECT {REVISION_COLUMNS} FROM memoire_memory_revisions \
             WHERE memory_id = ?1 ORDER BY rev"
        ),
        vec![text(id)],
    )
}

/// A memory that may still be confirmed, as SQL over its row.
pub const CONFIRMABLE: &str = "status NOT IN ('superseded', 'forgotten')";
/// A memory that may still be disputed or resolved, as SQL over its row.
pub const NOT_FORGOTTEN: &str = "status <> 'forgotten'";

/// `created_at` for a verdict on memory `?1` about its text at the rev in
/// `?2`: NULL, so the insert fails its `NOT NULL`, when the memory is gone,
/// no longer `eligible`, or its text changed after that rev. A verdict
/// never lands on text, or a state, its writer did not read.
fn clock_if_still_as_read(eligible: &str) -> String {
    format!(
        "(SELECT {NOW} FROM memoire_memory_memories \
          WHERE id = ?1 AND content_rev <= ?2 AND {eligible})"
    )
}

/// Records a confirmation of memory `id` on `rev`, the revision whose text
/// the confirmer checked.
pub fn insert_confirmation(
    batch: &mut Batch,
    id: &str,
    rev: i64,
    actor: &Author,
    evidence: &[String],
) -> usize {
    batch.push(
        format!(
            "INSERT INTO memoire_memory_confirmations \
             (memory_id, rev, principal, agent, session, evidence, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, {})",
            clock_if_still_as_read(CONFIRMABLE)
        ),
        vec![
            text(id),
            int(rev),
            text(&actor.principal),
            text(&actor.agent),
            text(&actor.session),
            json_list(evidence),
        ],
    )
}

/// Opens a dispute of memory `id` on `rev`, the revision whose text the
/// disputer read.
pub fn insert_dispute(
    batch: &mut Batch,
    id: &str,
    rev: i64,
    actor: &Author,
    reason: &str,
    evidence: &[String],
) -> usize {
    batch.push(
        format!(
            "INSERT INTO memoire_memory_disputes \
             (memory_id, rev, principal, agent, session, reason, evidence, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, {})",
            clock_if_still_as_read(NOT_FORGOTTEN)
        ),
        vec![
            text(id),
            int(rev),
            text(&actor.principal),
            text(&actor.agent),
            text(&actor.session),
            text(reason),
            json_list(evidence),
        ],
    )
}

/// Whether memory `?1` has an open dispute, as SQL.
pub const OPEN_DISPUTE: &str = "EXISTS (SELECT 1 FROM memoire_memory_disputes d \
     WHERE d.memory_id = ?1 AND d.resolved_at IS NULL)";

/// Appends an event that fails on its `NOT NULL created_at` unless memory
/// `id` exists and `condition` (SQL over `?1`, the id) holds, so a batch
/// whose other statements change nothing on their own still fails when
/// the state it read is gone.
pub fn append_guarded_event(
    batch: &mut Batch,
    id: &str,
    verb: &str,
    actor: &Author,
    summary: &str,
    condition: &str,
) -> usize {
    batch.push(
        format!(
            "INSERT INTO memoire_memory_events \
             (memory_id, verb, rev, scope, principal, agent, session, summary, created_at) \
             VALUES (?1, ?2, \
               (SELECT rev FROM memoire_memory_memories WHERE id = ?1), \
               (SELECT scope FROM memoire_memory_memories WHERE id = ?1), \
               ?3, ?4, ?5, ?6, \
               (SELECT {NOW} FROM memoire_memory_memories WHERE id = ?1 AND {condition}))"
        ),
        vec![
            text(id),
            text(verb),
            text(&actor.principal),
            text(&actor.agent),
            text(&actor.session),
            text(summary),
        ],
    )
}

/// Closes every open dispute of memory `id`.
pub fn resolve_disputes(batch: &mut Batch, id: &str, actor: &Author, resolution: &str) -> usize {
    batch.push(
        format!(
            "UPDATE memoire_memory_disputes SET resolved_at = {NOW}, resolved_principal = ?2, \
             resolved_agent = ?3, resolved_session = ?4, resolution = ?5 \
             WHERE memory_id = ?1 AND resolved_at IS NULL"
        ),
        vec![
            text(id),
            text(&actor.principal),
            text(&actor.agent),
            text(&actor.session),
            text(resolution),
        ],
    )
}

/// Marks memory `id` forgotten; the status statement then says so.
pub fn mark_forgotten(batch: &mut Batch, id: &str) -> usize {
    batch.push(
        "UPDATE memoire_memory_memories SET forgotten = 1 WHERE id = ?1",
        vec![text(id)],
    )
}

/// The ids reached from memory `id` by following `superseded_by`, `id`
/// included. `UNION` drops ids already visited, so it ends on any graph.
pub fn select_chain(batch: &mut Batch, id: &str) -> usize {
    batch.push(
        "WITH RECURSIVE chain (id) AS ( \
           SELECT ?1 \
           UNION \
           SELECT m.superseded_by FROM chain \
           JOIN memoire_memory_memories m ON m.id = chain.id \
           WHERE m.superseded_by IS NOT NULL) \
         SELECT id FROM chain",
        vec![text(id)],
    )
}

/// What a purge leaves of the reason of another memory's revision that
/// linked it to the purged one.
pub const PURGED_REASON: &str = "purged";

/// Erases memory `id` and everything written about it: its rows, the
/// reasons other memories' revisions gave for linking to it, and the
/// reasons in their `supersede` events naming it. Links to it are cleared
/// and the memories they leave hanging get their status back. One `purge`
/// event stays, with no caller text. Returns the index of its first
/// statement, the purge event, which fails when the memory is already
/// gone, so two purges racing leave the first one's event.
pub fn purge(batch: &mut Batch, id: &str, actor: &Author) -> usize {
    let guarded = batch.push(
        format!(
            "INSERT INTO memoire_memory_events \
             (memory_id, verb, rev, scope, principal, agent, session, summary, created_at) \
             VALUES (?1, 'purge', NULL, NULL, ?2, ?3, ?4, NULL, \
               (SELECT {NOW} FROM memoire_memory_memories WHERE id = ?1))"
        ),
        vec![
            text(id),
            text(&actor.principal),
            text(&actor.agent),
            text(&actor.session),
        ],
    );
    // The search row before the memories row it is found through.
    batch.push(
        "DELETE FROM memoire_memory_fts \
         WHERE rowid = (SELECT seq FROM memoire_memory_memories WHERE id = ?1)",
        vec![text(id)],
    );
    for table in [
        "memoire_memory_confirmations",
        "memoire_memory_disputes",
        "memoire_memory_revisions",
    ] {
        batch.push(
            format!("DELETE FROM {table} WHERE memory_id = ?1"),
            vec![text(id)],
        );
    }
    batch.push(
        "DELETE FROM memoire_memory_events WHERE memory_id = ?1 AND verb <> 'purge'",
        vec![text(id)],
    );
    // A revision that set a link to it explains why, often by quoting it.
    batch.push(
        "UPDATE memoire_memory_revisions AS r SET reason = ?2 \
         WHERE r.memory_id <> ?1 AND ( \
           (r.supersedes = ?1 AND NOT EXISTS (SELECT 1 FROM memoire_memory_revisions p \
              WHERE p.memory_id = r.memory_id AND p.rev = r.rev - 1 AND p.supersedes IS ?1)) \
           OR (r.superseded_by = ?1 AND NOT EXISTS (SELECT 1 FROM memoire_memory_revisions p \
              WHERE p.memory_id = r.memory_id AND p.rev = r.rev - 1 AND p.superseded_by IS ?1)))",
        vec![text(id), text(PURGED_REASON)],
    );
    // `by <id>: <reason>` on the memory it replaced.
    batch.push(
        "UPDATE memoire_memory_events SET summary = 'by ' || ?1 \
         WHERE verb = 'supersede' AND memory_id <> ?1 \
           AND substr(summary, 1, length(?1) + 4) = 'by ' || ?1 || ':'",
        vec![text(id)],
    );
    batch.push(
        "UPDATE memoire_memory_memories SET supersedes = NULL WHERE supersedes = ?1",
        vec![text(id)],
    );
    batch.push(
        "UPDATE memoire_memory_memories SET superseded_by = NULL WHERE superseded_by = ?1",
        vec![text(id)],
    );
    batch.push(STATUS_UNLINKED_SQL, vec![]);
    batch.push(
        "DELETE FROM memoire_memory_memories WHERE id = ?1",
        vec![text(id)],
    );
    // Merges the index segments, so the deleted terms leave the index too.
    batch.push(
        "INSERT INTO memoire_memory_fts (memoire_memory_fts) VALUES ('optimize')",
        vec![],
    );
    guarded
}

/// Selects the store's id.
pub fn select_store_id(batch: &mut Batch) -> usize {
    batch.push(
        "SELECT value FROM memoire_memory_meta WHERE key = 'store_id'",
        vec![],
    )
}

/// The store id a [`select_store_id`] result holds.
pub fn store_id_of(result: &StatementResult) -> Result<String> {
    match result.rows.first().and_then(|row| row.first()) {
        Some(SqlValue::Text(id)) => Ok(id.clone()),
        _ => Err(invalid_response("the store_id")),
    }
}

/// A JSON array of strings, or NULL for "no filter".
fn json_or_null(items: Option<&[String]>) -> SqlValue {
    items.map_or(SqlValue::Null, json_list)
}

/// Which events `log` reads.
#[derive(Debug)]
pub struct EventFilter<'a> {
    /// `None`: every scope.
    pub scopes: Option<&'a [String]>,
    /// `--since`, as seconds before the store's now.
    pub since_seconds: Option<u64>,
    /// Only events after a cursor's `seq`, when the cursor belongs to
    /// this store; every event when it belongs to another.
    pub after: Option<&'a super::cursor::Cursor>,
}

/// The store's id, as SQL.
const STORE_ID: &str = "(SELECT value FROM memoire_memory_meta WHERE key = 'store_id')";

fn event_conditions() -> String {
    format!(
        "(?1 IS NULL OR scope IN (SELECT value FROM json_each(?1))) \
         AND (?2 IS NULL OR created_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?2)) \
         AND seq > (CASE WHEN ?3 IS NULL OR ?3 = {STORE_ID} THEN ?4 ELSE 0 END)"
    )
}

fn event_params(filter: &EventFilter<'_>) -> Vec<SqlValue> {
    vec![
        json_or_null(filter.scopes),
        filter
            .since_seconds
            .map_or(SqlValue::Null, |s| text(&format!("-{s} seconds"))),
        opt_text(filter.after.map(|c| c.store_id.as_str())),
        int(filter.after.map_or(0, |c| c.seq)),
    ]
}

/// The newest `limit` events passing `filter`, newest first.
pub fn select_events(batch: &mut Batch, filter: &EventFilter<'_>, limit: i64) -> usize {
    let mut params = event_params(filter);
    params.push(int(limit));
    batch.push(
        format!(
            "SELECT seq, memory_id, verb, rev, scope, principal, agent, session, summary, \
             created_at FROM memoire_memory_events WHERE {} \
             ORDER BY seq DESC LIMIT ?5",
            event_conditions()
        ),
        params,
    )
}

/// How many events pass `filter`, and the newest one's `seq`.
pub fn count_events(batch: &mut Batch, filter: &EventFilter<'_>) -> usize {
    batch.push(
        format!(
            "SELECT count(*) AS events, max(seq) AS newest FROM memoire_memory_events \
             WHERE {}",
            event_conditions()
        ),
        event_params(filter),
    )
}

/// One page of `export`: memories of `scopes` (all when `None`) that are
/// not forgotten, after `seq`, in `seq` order.
pub fn select_export_page(
    batch: &mut Batch,
    scopes: Option<&[String]>,
    after: i64,
    limit: i64,
) -> usize {
    batch.push(
        format!(
            "SELECT {MEMORY_COLUMNS} FROM memoire_memory_memories m \
             WHERE m.status <> 'forgotten' \
               AND (?1 IS NULL OR m.scope IN (SELECT value FROM json_each(?1))) \
               AND m.seq > ?2 \
             ORDER BY m.seq LIMIT ?3"
        ),
        vec![json_or_null(scopes), int(after), int(limit)],
    )
}

/// One row of a result, read by column name.
pub struct Row<'a> {
    result: &'a StatementResult,
    values: &'a [SqlValue],
}

/// The rows of a result.
pub fn rows(result: &StatementResult) -> impl Iterator<Item = Row<'_>> {
    result.rows.iter().map(move |values| Row { result, values })
}

impl Row<'_> {
    fn get(&self, name: &str) -> Result<&SqlValue> {
        self.result
            .columns
            .iter()
            .position(|c| c.name == name)
            .and_then(|i| self.values.get(i))
            .ok_or_else(|| invalid_response(&format!("the column `{name}`")))
    }

    pub fn text(&self, name: &str) -> Result<String> {
        match self.get(name)? {
            SqlValue::Text(s) => Ok(s.clone()),
            _ => Err(invalid_response(&format!("text in `{name}`"))),
        }
    }

    pub fn opt_text(&self, name: &str) -> Result<Option<String>> {
        match self.get(name)? {
            SqlValue::Null => Ok(None),
            SqlValue::Text(s) => Ok(Some(s.clone())),
            _ => Err(invalid_response(&format!("text or NULL in `{name}`"))),
        }
    }

    pub fn int(&self, name: &str) -> Result<i64> {
        match self.get(name)? {
            SqlValue::Integer(n) => Ok(*n),
            _ => Err(invalid_response(&format!("an integer in `{name}`"))),
        }
    }

    pub fn opt_int(&self, name: &str) -> Result<Option<i64>> {
        match self.get(name)? {
            SqlValue::Null => Ok(None),
            SqlValue::Integer(n) => Ok(Some(*n)),
            _ => Err(invalid_response(&format!("an integer or NULL in `{name}`"))),
        }
    }

    pub fn event(&self) -> Result<Event> {
        Ok(Event {
            seq: self.int("seq")?,
            memory_id: self.text("memory_id")?,
            verb: self.text("verb")?,
            rev: self.opt_int("rev")?,
            scope: self.opt_text("scope")?,
            principal: self.text("principal")?,
            agent: self.text("agent")?,
            session: self.text("session")?,
            summary: self.opt_text("summary")?,
            created_at: self.text("created_at")?,
        })
    }

    pub fn real(&self, name: &str) -> Result<f64> {
        match self.get(name)? {
            SqlValue::Real(x) => Ok(*x),
            SqlValue::Integer(n) => Ok(*n as f64),
            _ => Err(invalid_response(&format!("a number in `{name}`"))),
        }
    }

    /// A JSON array of strings.
    pub fn list(&self, name: &str) -> Result<Vec<String>> {
        serde_json::from_str(&self.text(name)?)
            .map_err(|_| invalid_response(&format!("a JSON list in `{name}`")))
    }

    pub fn status(&self, name: &str) -> Result<Status> {
        Status::parse(&self.text(name)?).ok_or_else(|| invalid_response("a known status"))
    }

    pub fn memory(&self) -> Result<Memory> {
        Ok(Memory {
            seq: self.int("seq")?,
            id: self.text("id")?,
            rev: self.int("rev")?,
            content_rev: self.int("content_rev")?,
            content_principal: self.text("content_principal")?,
            content_session: self.text("content_session")?,
            kind: self.text("kind")?,
            scope: self.text("scope")?,
            title: self.text("title")?,
            body: self.text("body")?,
            tags: self.list("tags")?,
            evidence: self.list("evidence")?,
            status: self.status("status")?,
            supersedes: self.opt_text("supersedes")?,
            superseded_by: self.opt_text("superseded_by")?,
            author: Author {
                principal: self.text("author_principal")?,
                agent: self.text("author_agent")?,
                session: self.text("author_session")?,
            },
            confirmers: self.int("confirmers")?,
            open_disputes: self.int("open_disputes")?,
            created_at: self.text("created_at")?,
            updated_at: self.text("updated_at")?,
        })
    }

    pub fn revision(&self) -> Result<Revision> {
        Ok(Revision {
            memory_id: self.text("memory_id")?,
            rev: self.int("rev")?,
            kind: self.text("kind")?,
            scope: self.text("scope")?,
            title: self.text("title")?,
            body: self.text("body")?,
            tags: self.list("tags")?,
            evidence: self.list("evidence")?,
            status: self.status("status")?,
            supersedes: self.opt_text("supersedes")?,
            superseded_by: self.opt_text("superseded_by")?,
            author_principal: self.text("author_principal")?,
            author_agent: self.text("author_agent")?,
            author_session: self.text("author_session")?,
            reason: self.text("reason")?,
            created_at: self.text("created_at")?,
        })
    }

    pub fn confirmation(&self) -> Result<Confirmation> {
        Ok(Confirmation {
            rev: self.int("rev")?,
            principal: self.text("principal")?,
            agent: self.text("agent")?,
            session: self.text("session")?,
            evidence: self.list("evidence")?,
            created_at: self.text("created_at")?,
            counts: self.int("counts")? != 0,
        })
    }

    pub fn dispute(&self) -> Result<Dispute> {
        Ok(Dispute {
            rev: self.int("rev")?,
            principal: self.text("principal")?,
            agent: self.text("agent")?,
            session: self.text("session")?,
            reason: self.text("reason")?,
            evidence: self.list("evidence")?,
            created_at: self.text("created_at")?,
            resolved_at: self.opt_text("resolved_at")?,
            resolution: self.opt_text("resolution")?,
        })
    }
}

/// The memory a single-row select returned, if any.
pub fn one_memory(result: &StatementResult) -> Result<Option<Memory>> {
    rows(result).next().map(|row| row.memory()).transpose()
}

/// Every row of a result, decoded.
pub fn all<T>(result: &StatementResult, decode: impl Fn(&Row<'_>) -> Result<T>) -> Result<Vec<T>> {
    rows(result).map(|row| decode(&row)).collect()
}
