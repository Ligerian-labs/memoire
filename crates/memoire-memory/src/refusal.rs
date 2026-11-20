//! What the memory refuses, as values: each refusal has a stable `code`, a
//! class callers map to their own failure kinds, the members an agent acts
//! on ([`Refusal::data`]) and a sentence for people.
use crate::model::{
    BODY_MAX_BYTES, EVIDENCE_MAX_CHARS, EVIDENCE_MAX_ITEMS, REASON_MAX_CHARS, Status, TAGS_MAX,
    TITLE_MAX_CHARS,
};
use serde_json::{Map, Value, json};

/// A refusal of the memory.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// `memory_not_initialized`: the database holds no store.
    NotInitialized,
    /// `memory_schema_older`: the store needs `init` to reach this version.
    SchemaOlder {
        store_version: i64,
        supported_version: i64,
    },
    /// `memory_schema_newer`: the store is ahead of this engine.
    SchemaNewer {
        store_version: i64,
        supported_version: i64,
    },
    /// `memory_conflict`: the memory moved after it was read. With
    /// `since_rev`, its text changed after the rev a verdict was about.
    Conflict {
        id: String,
        current_rev: i64,
        since_rev: Option<i64>,
    },
    /// `memory_duplicate`: current memories of `scope` with a similar
    /// title, closest first.
    Duplicate {
        scope: String,
        candidates: Vec<Candidate>,
    },
    /// `memory_not_current`: the memory was superseded or forgotten.
    NotCurrent {
        id: String,
        status: Status,
        superseded_by: Option<String>,
    },
    /// `memory_no_open_dispute`.
    NoOpenDispute { id: String },
    /// `memory_cycle`: superseding `id` by `by` would close a cycle; `id`
    /// and `by` are the same memory when it would supersede itself.
    Cycle { id: String, by: String },
    /// `memory_not_found`, with the time it was purged when it was.
    NotFound {
        id: String,
        purged_at: Option<String>,
    },
    /// `memory_self_confirm`: the confirmer wrote the text, in the same
    /// session; `without_session` when that session is the empty one.
    SelfConfirm { id: String, without_session: bool },
    /// `memory_secret_detected`: `field` matches `pattern`.
    SecretDetected { pattern: String, field: String },
    /// `memory_too_large`: `field` passes `limit` with `size`.
    TooLarge {
        field: String,
        limit: usize,
        size: usize,
    },
    /// `memory_unchanged`: a revision that would change nothing.
    Unchanged { id: String },
    /// `invalid_query`: a recall query without a word.
    InvalidQuery,
    /// `invalid_argument`: an input of the wrong shape.
    InvalidArgument(Invalid),
}

/// A near-duplicate of a new memory's title.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub title: String,
    /// The status as the store has it.
    pub status: String,
    /// The Dice coefficient, to three decimals.
    pub similarity: f64,
}

/// The `invalid_argument` refusals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invalid {
    EmptyTitle,
    MultilineTitle,
    EmptyEvidence,
    EmptyReason,
    /// A verdict about rev `rev`, past memory `id`'s current rev.
    RevPastCurrent {
        rev: i64,
        id: String,
        current_rev: i64,
    },
}

/// What kind of refusal it is, for an exit code or a status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalClass {
    /// No such memory.
    NotFound,
    /// The store's state forbids it; reading again may help.
    Conflict,
    /// The input is refused as it is.
    Rejected,
    /// The input is malformed.
    Usage,
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotInitialized => "memory_not_initialized",
            Self::SchemaOlder { .. } => "memory_schema_older",
            Self::SchemaNewer { .. } => "memory_schema_newer",
            Self::Conflict { .. } => "memory_conflict",
            Self::Duplicate { .. } => "memory_duplicate",
            Self::NotCurrent { .. } => "memory_not_current",
            Self::NoOpenDispute { .. } => "memory_no_open_dispute",
            Self::Cycle { .. } => "memory_cycle",
            Self::NotFound { .. } => "memory_not_found",
            Self::SelfConfirm { .. } => "memory_self_confirm",
            Self::SecretDetected { .. } => "memory_secret_detected",
            Self::TooLarge { .. } => "memory_too_large",
            Self::Unchanged { .. } => "memory_unchanged",
            Self::InvalidQuery => "invalid_query",
            Self::InvalidArgument(_) => "invalid_argument",
        }
    }

    pub fn class(&self) -> RefusalClass {
        match self {
            Self::NotFound { .. } => RefusalClass::NotFound,
            Self::NotInitialized
            | Self::SchemaOlder { .. }
            | Self::SchemaNewer { .. }
            | Self::Conflict { .. }
            | Self::Duplicate { .. }
            | Self::NotCurrent { .. }
            | Self::NoOpenDispute { .. }
            | Self::Cycle { .. } => RefusalClass::Conflict,
            Self::SelfConfirm { .. }
            | Self::SecretDetected { .. }
            | Self::TooLarge { .. }
            | Self::Unchanged { .. } => RefusalClass::Rejected,
            Self::InvalidQuery | Self::InvalidArgument(_) => RefusalClass::Usage,
        }
    }

    /// The members an agent acts on, beside `code`, in the order they are
    /// reported.
    pub fn data(&self) -> Map<String, Value> {
        let members: Vec<(&str, Value)> = match self {
            Self::SchemaOlder {
                store_version,
                supported_version,
            }
            | Self::SchemaNewer {
                store_version,
                supported_version,
            } => vec![
                ("store_version", json!(store_version)),
                ("supported_version", json!(supported_version)),
            ],
            Self::Conflict {
                id, current_rev, ..
            } => vec![("id", json!(id)), ("current_rev", json!(current_rev))],
            Self::Duplicate { candidates, .. } => {
                let candidates: Vec<_> = candidates
                    .iter()
                    .map(|c| {
                        json!({"id": c.id, "title": c.title, "status": c.status,
                               "similarity": c.similarity})
                    })
                    .collect();
                vec![("candidates", json!(candidates))]
            }
            Self::NotCurrent {
                id,
                status,
                superseded_by,
            } => vec![
                ("id", json!(id)),
                ("status", json!(status.as_str())),
                ("superseded_by", json!(superseded_by)),
            ],
            Self::NotFound { id, .. } => vec![("id", json!(id))],
            Self::SecretDetected { pattern, field } => {
                vec![("pattern", json!(pattern)), ("field", json!(field))]
            }
            Self::TooLarge { field, limit, .. } => {
                vec![("field", json!(field)), ("limit", json!(limit))]
            }
            Self::NotInitialized
            | Self::NoOpenDispute { .. }
            | Self::Cycle { .. }
            | Self::SelfConfirm { .. }
            | Self::Unchanged { .. }
            | Self::InvalidQuery
            | Self::InvalidArgument(_) => vec![],
        };
        members
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect()
    }

    /// One line for a person. `memory_not_initialized` does not name the
    /// database, which only the caller knows.
    pub fn message(&self) -> String {
        match self {
            Self::NotInitialized => {
                "the database holds no memory store; run `memoire memory init`".into()
            }
            Self::SchemaNewer {
                store_version: v,
                supported_version: supported,
            } => format!(
                "the memory store is at version {v}, newer than this CLI's {supported}; upgrade memoire"
            ),
            Self::SchemaOlder {
                store_version: v,
                supported_version: supported,
            } => format!(
                "the memory store is at version {v}, older than this CLI's {supported}; run `memoire memory init`"
            ),
            Self::Conflict {
                id,
                current_rev,
                since_rev: None,
            } => format!(
                "memory {id} changed while this ran: it is at rev {current_rev}; read it again and retry"
            ),
            Self::Conflict {
                id,
                current_rev,
                since_rev: Some(read),
            } => format!(
                "the text of memory {id} changed since rev {read}: it is at rev {current_rev}; read it again before judging it"
            ),
            Self::Duplicate { scope, candidates } => {
                let id = candidates.first().map_or("", |c| c.id.as_str());
                format!(
                    "a memory in {scope} has a similar title ({id}); revise it, or pass --force to record this one too"
                )
            }
            Self::NotCurrent {
                id,
                status,
                superseded_by,
            } => {
                let mut message = format!("memory {id} is {}", status.as_str());
                if let Some(by) = superseded_by {
                    message.push_str(&format!(" by {by}"));
                }
                message
            }
            Self::NoOpenDispute { id } => format!("memory {id} has no open dispute"),
            Self::Cycle { id, by } if id == by => format!("memory {id} cannot supersede itself"),
            Self::Cycle { id, by } => format!(
                "superseding {id} by {by} would close a cycle: {by} is already replaced by {id}"
            ),
            Self::NotFound {
                id,
                purged_at: Some(at),
            } => format!("memory {id} was purged at {at}"),
            Self::NotFound {
                id,
                purged_at: None,
            } => format!("no memory {id} in this store"),
            Self::SelfConfirm {
                id,
                without_session: true,
            } => format!(
                "you wrote the text of memory {id}, and MEMOIRE_SESSION is not set, so every run with this key counts as that same writer; set MEMOIRE_SESSION (or --session) to an id unique to each run"
            ),
            Self::SelfConfirm {
                id,
                without_session: false,
            } => format!(
                "you wrote the text of memory {id} in this session; a confirmation needs another principal or session"
            ),
            Self::SecretDetected { pattern, field } => format!(
                "the {field} looks like it holds a secret ({pattern}); memories are shared, so leave secrets out"
            ),
            Self::TooLarge { field, limit, size } => too_large_message(field, *limit, *size),
            Self::Unchanged { id } => format!("the revision would leave memory {id} as it is"),
            Self::InvalidQuery => "the query has no word to search for".into(),
            Self::InvalidArgument(invalid) => match invalid {
                Invalid::EmptyTitle => "--title is empty".into(),
                Invalid::MultilineTitle => "--title is one line".into(),
                Invalid::EmptyEvidence => "an --evidence item is empty".into(),
                Invalid::EmptyReason => "--reason is empty".into(),
                Invalid::RevPastCurrent {
                    rev,
                    id,
                    current_rev,
                } => format!("--rev {rev} is past memory {id}'s current rev {current_rev}"),
            },
        }
    }
}

/// The sentence of a `memory_too_large`, after the limit it passed.
fn too_large_message(field: &str, limit: usize, size: usize) -> String {
    match (field, limit) {
        ("title", TITLE_MAX_CHARS) => {
            format!("the title has {size} characters; at most {TITLE_MAX_CHARS}")
        }
        ("body", BODY_MAX_BYTES) => format!("the body has {size} bytes; at most {BODY_MAX_BYTES}"),
        ("tags", TAGS_MAX) => format!("{size} tags; at most {TAGS_MAX}"),
        ("evidence", EVIDENCE_MAX_ITEMS) => {
            format!("{size} evidence items; at most {EVIDENCE_MAX_ITEMS}")
        }
        ("evidence", EVIDENCE_MAX_CHARS) => {
            format!("an evidence item has {size} characters; at most {EVIDENCE_MAX_CHARS}")
        }
        ("reason", REASON_MAX_CHARS) => {
            format!("the reason has {size} characters; at most {REASON_MAX_CHARS}")
        }
        _ => format!("the {field} has {size}; at most {limit}"),
    }
}
