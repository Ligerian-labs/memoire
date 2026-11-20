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
    /// Item `index` of the list `field` (`evidence`, `add_evidence`) is
    /// empty.
    EmptyEvidence {
        field: &'static str,
        index: usize,
    },
    EmptyReason,
    /// A verdict about rev `rev`, past memory `id`'s current rev.
    RevPastCurrent {
        rev: i64,
        id: String,
        current_rev: i64,
    },
    /// An input of the wrong syntax that the CLI's parser refuses before a
    /// verb runs, refused by the verb for other callers: the JSON Pointer
    /// of the field as the HTTP API names it (`/tags/2`, `/supersedes`,
    /// `""` for the body as a whole) and a code (`invalid`, `required`,
    /// `not_allowed`, `out_of_range`).
    Field {
        pointer: String,
        code: &'static str,
    },
}

/// The codes of [`Invalid::Field`].
const FIELD_CODES: [&str; 4] = ["invalid", "required", "not_allowed", "out_of_range"];

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

    /// For a refusal of an input's form (class [`RefusalClass::Usage`]):
    /// the JSON Pointer of the field it is about, as the HTTP API names
    /// it, and a code, as a `validation_failed` detail reports them.
    pub fn detail(&self) -> Option<(String, &'static str)> {
        let at = |pointer: &str| Some((pointer.to_owned(), "invalid"));
        match self {
            Self::InvalidQuery => at("/query"),
            Self::InvalidArgument(invalid) => match invalid {
                Invalid::EmptyTitle | Invalid::MultilineTitle => at("/title"),
                Invalid::EmptyEvidence { field, index } => at(&format!("/{field}/{index}")),
                Invalid::EmptyReason => at("/reason"),
                Invalid::RevPastCurrent { .. } => Some(("/rev".into(), "out_of_range")),
                Invalid::Field { pointer, code } => Some((pointer.clone(), code)),
            },
            _ => None,
        }
    }

    /// The refusal as it travels: its code and every member needed to
    /// build it again with [`Refusal::from_wire`], [`Refusal::data`] and
    /// what that leaves out (a duplicate's scope, the purge time, the rev
    /// a verdict was about, the ids, a too-large field's size, a
    /// self-confirm without a session). Members without a value
    /// (`since_rev`, `purged_at`) are left out.
    pub fn to_wire(&self) -> (&'static str, Map<String, Value>) {
        let mut map = self.data();
        let mut put = |name: &str, value: Value| {
            map.insert(name.to_owned(), value);
        };
        match self {
            Self::Conflict {
                since_rev: Some(rev),
                ..
            } => put("since_rev", json!(rev)),
            Self::Duplicate { scope, .. } => put("scope", json!(scope)),
            Self::NoOpenDispute { id } | Self::Unchanged { id } => put("id", json!(id)),
            Self::Cycle { id, by } => {
                put("id", json!(id));
                put("by", json!(by));
            }
            Self::NotFound {
                purged_at: Some(at),
                ..
            } => put("purged_at", json!(at)),
            Self::SelfConfirm {
                id,
                without_session,
            } => {
                put("id", json!(id));
                put("without_session", json!(without_session));
            }
            Self::TooLarge { size, .. } => put("size", json!(size)),
            Self::InvalidArgument(invalid) => match invalid {
                Invalid::EmptyTitle => put("argument", json!("empty_title")),
                Invalid::MultilineTitle => put("argument", json!("multiline_title")),
                Invalid::EmptyReason => put("argument", json!("empty_reason")),
                Invalid::EmptyEvidence { field, index } => {
                    put("argument", json!("empty_evidence"));
                    put("field", json!(field));
                    put("index", json!(index));
                }
                Invalid::RevPastCurrent {
                    rev,
                    id,
                    current_rev,
                } => {
                    put("argument", json!("rev_past_current"));
                    put("rev", json!(rev));
                    put("id", json!(id));
                    put("current_rev", json!(current_rev));
                }
                Invalid::Field { pointer, code } => {
                    put("argument", json!("field"));
                    put("pointer", json!(pointer));
                    put("code", json!(code));
                }
            },
            _ => {}
        }
        (self.code(), map)
    }

    /// The refusal [`Refusal::to_wire`] sent as `code` and `data`; `None`
    /// for another code or members that do not make one.
    pub fn from_wire(code: &str, data: &Map<String, Value>) -> Option<Self> {
        let text = |name: &str| data.get(name)?.as_str().map(str::to_owned);
        let int = |name: &str| data.get(name)?.as_i64();
        let size = |name: &str| usize::try_from(data.get(name)?.as_u64()?).ok();
        let opt_text = |name: &str| match data.get(name) {
            None | Some(Value::Null) => Some(None),
            Some(Value::String(s)) => Some(Some(s.clone())),
            Some(_) => None,
        };
        Some(match code {
            "memory_not_initialized" => Self::NotInitialized,
            "memory_schema_older" => Self::SchemaOlder {
                store_version: int("store_version")?,
                supported_version: int("supported_version")?,
            },
            "memory_schema_newer" => Self::SchemaNewer {
                store_version: int("store_version")?,
                supported_version: int("supported_version")?,
            },
            "memory_conflict" => Self::Conflict {
                id: text("id")?,
                current_rev: int("current_rev")?,
                since_rev: match data.get("since_rev") {
                    None | Some(Value::Null) => None,
                    Some(rev) => Some(rev.as_i64()?),
                },
            },
            "memory_duplicate" => Self::Duplicate {
                scope: text("scope")?,
                candidates: data
                    .get("candidates")?
                    .as_array()?
                    .iter()
                    .map(|c| {
                        Some(Candidate {
                            id: c.get("id")?.as_str()?.to_owned(),
                            title: c.get("title")?.as_str()?.to_owned(),
                            status: c.get("status")?.as_str()?.to_owned(),
                            similarity: c.get("similarity")?.as_f64()?,
                        })
                    })
                    .collect::<Option<_>>()?,
            },
            "memory_not_current" => Self::NotCurrent {
                id: text("id")?,
                status: Status::parse(&text("status")?)?,
                superseded_by: opt_text("superseded_by")?,
            },
            "memory_no_open_dispute" => Self::NoOpenDispute { id: text("id")? },
            "memory_cycle" => Self::Cycle {
                id: text("id")?,
                by: text("by")?,
            },
            "memory_not_found" => Self::NotFound {
                id: text("id")?,
                purged_at: opt_text("purged_at")?,
            },
            "memory_self_confirm" => Self::SelfConfirm {
                id: text("id")?,
                without_session: data.get("without_session")?.as_bool()?,
            },
            "memory_secret_detected" => Self::SecretDetected {
                pattern: text("pattern")?,
                field: text("field")?,
            },
            "memory_too_large" => Self::TooLarge {
                field: text("field")?,
                limit: size("limit")?,
                size: size("size")?,
            },
            "memory_unchanged" => Self::Unchanged { id: text("id")? },
            "invalid_query" => Self::InvalidQuery,
            "invalid_argument" => Self::InvalidArgument(match text("argument")?.as_str() {
                "empty_title" => Invalid::EmptyTitle,
                "multiline_title" => Invalid::MultilineTitle,
                "empty_reason" => Invalid::EmptyReason,
                "empty_evidence" => Invalid::EmptyEvidence {
                    field: ["evidence", "add_evidence"]
                        .into_iter()
                        .find(|f| Some(*f) == data.get("field").and_then(Value::as_str))?,
                    index: size("index")?,
                },
                "rev_past_current" => Invalid::RevPastCurrent {
                    rev: int("rev")?,
                    id: text("id")?,
                    current_rev: int("current_rev")?,
                },
                "field" => Invalid::Field {
                    pointer: text("pointer")?,
                    code: FIELD_CODES
                        .into_iter()
                        .find(|c| Some(*c) == data.get("code").and_then(Value::as_str))?,
                },
                _ => return None,
            }),
            _ => return None,
        })
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
                Invalid::EmptyEvidence { .. } => "an --evidence item is empty".into(),
                Invalid::EmptyReason => "--reason is empty".into(),
                Invalid::RevPastCurrent {
                    rev,
                    id,
                    current_rev,
                } => format!("--rev {rev} is past memory {id}'s current rev {current_rev}"),
                Invalid::Field { pointer, code } => {
                    format!("the input at `{pointer}` is refused: {code}")
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn every_variant() -> Vec<Refusal> {
        let id = || "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8".to_owned();
        vec![
            Refusal::NotInitialized,
            Refusal::SchemaOlder {
                store_version: 0,
                supported_version: 1,
            },
            Refusal::SchemaNewer {
                store_version: 2,
                supported_version: 1,
            },
            Refusal::Conflict {
                id: id(),
                current_rev: 3,
                since_rev: None,
            },
            Refusal::Conflict {
                id: id(),
                current_rev: 3,
                since_rev: Some(1),
            },
            Refusal::Duplicate {
                scope: "global".into(),
                candidates: vec![Candidate {
                    id: id(),
                    title: "Deploys need the VPN".into(),
                    status: "inferred".into(),
                    similarity: 0.75,
                }],
            },
            Refusal::NotCurrent {
                id: id(),
                status: Status::Superseded,
                superseded_by: Some(id()),
            },
            Refusal::NotCurrent {
                id: id(),
                status: Status::Forgotten,
                superseded_by: None,
            },
            Refusal::NoOpenDispute { id: id() },
            Refusal::Cycle { id: id(), by: id() },
            Refusal::NotFound {
                id: id(),
                purged_at: None,
            },
            Refusal::NotFound {
                id: id(),
                purged_at: Some("2026-11-19T10:00:00.000Z".into()),
            },
            Refusal::SelfConfirm {
                id: id(),
                without_session: true,
            },
            Refusal::SecretDetected {
                pattern: "github_token".into(),
                field: "body".into(),
            },
            Refusal::TooLarge {
                field: "title".into(),
                limit: 120,
                size: 121,
            },
            Refusal::Unchanged { id: id() },
            Refusal::InvalidQuery,
            Refusal::InvalidArgument(Invalid::EmptyTitle),
            Refusal::InvalidArgument(Invalid::MultilineTitle),
            Refusal::InvalidArgument(Invalid::EmptyEvidence {
                field: "add_evidence",
                index: 2,
            }),
            Refusal::InvalidArgument(Invalid::EmptyReason),
            Refusal::InvalidArgument(Invalid::RevPastCurrent {
                rev: 4,
                id: id(),
                current_rev: 3,
            }),
            Refusal::InvalidArgument(Invalid::Field {
                pointer: "/tags/2".into(),
                code: "invalid",
            }),
        ]
    }

    #[test]
    fn every_refusal_round_trips_through_its_wire_form() {
        for refusal in every_variant() {
            let (code, map) = refusal.to_wire();
            assert_eq!(code, refusal.code());
            // What the CLI prints is a part of the wire form.
            for (name, value) in refusal.data() {
                assert_eq!(map.get(&name), Some(&value), "{code} {name}");
            }
            assert_eq!(
                Refusal::from_wire(code, &map).as_ref(),
                Some(&refusal),
                "{code} {map:?}"
            );
        }
        assert_eq!(Refusal::from_wire("sql_error", &Map::new()), None);
        assert_eq!(Refusal::from_wire("memory_conflict", &Map::new()), None);
    }

    #[test]
    fn values_without_a_value_stay_off_the_wire() {
        let (_, map) = Refusal::Conflict {
            id: "mem_x".into(),
            current_rev: 3,
            since_rev: None,
        }
        .to_wire();
        assert_eq!(Value::Object(map), json!({"id": "mem_x", "current_rev": 3}));
        let (_, map) = Refusal::NotFound {
            id: "mem_x".into(),
            purged_at: None,
        }
        .to_wire();
        assert_eq!(Value::Object(map), json!({"id": "mem_x"}));
    }

    #[test]
    fn input_refusals_name_their_field() {
        for refusal in every_variant() {
            assert_eq!(
                refusal.detail().is_some(),
                refusal.class() == RefusalClass::Usage,
                "{refusal:?}"
            );
        }
        assert_eq!(
            Refusal::InvalidArgument(Invalid::EmptyEvidence {
                field: "evidence",
                index: 1
            })
            .detail(),
            Some(("/evidence/1".into(), "invalid"))
        );
        assert_eq!(
            Refusal::InvalidQuery.detail(),
            Some(("/query".into(), "invalid"))
        );
    }
}
