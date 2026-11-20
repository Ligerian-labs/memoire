//! The memory routes (docs/api.md, "Memory routes"): the objects they
//! return, the request bodies and the response envelopes.
//!
//! The object types are the memory engine's own (`memoire-memory` uses
//! them for every verb), so the CLI's `--json` output and the routes share
//! one definition of each shape.
use serde::{Deserialize, Serialize};

/// Default and largest `limit` of recall.
pub const DEFAULT_RECALL_LIMIT: i64 = 10;
pub const MAX_RECALL_LIMIT: i64 = 100;
/// Default and largest `limit` of the log.
pub const DEFAULT_EVENTS_LIMIT: i64 = 50;
pub const MAX_EVENTS_LIMIT: i64 = 1_000;
/// Default and largest page of the memory export.
pub const MAX_EXPORT_PAGE: i64 = 500;

/// The live status of a memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Inferred,
    Confirmed,
    Disputed,
    Superseded,
    Forgotten,
}

impl Status {
    /// What recall searches without `--status`.
    pub const RECALLED: [Status; 3] = [Status::Inferred, Status::Confirmed, Status::Disputed];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inferred => "inferred",
            Self::Confirmed => "confirmed",
            Self::Disputed => "disputed",
            Self::Superseded => "superseded",
            Self::Forgotten => "forgotten",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::Inferred,
            Self::Confirmed,
            Self::Disputed,
            Self::Superseded,
            Self::Forgotten,
        ]
        .into_iter()
        .find(|s| s.as_str() == text)
    }

    /// Whether the memory may still be revised or confirmed.
    pub fn is_current(self) -> bool {
        !matches!(self, Self::Superseded | Self::Forgotten)
    }
}

/// Who wrote a revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    pub principal: String,
    pub agent: String,
    pub session: String,
}

/// A memory's current state, as every verb prints it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    /// The store's position of the memory; the engine reads it, it does
    /// not travel.
    #[serde(skip)]
    pub seq: i64,
    pub id: String,
    pub rev: i64,
    /// The latest revision that changed title, body or kind, and its
    /// author's principal and session: confirmations are compared with
    /// them. The engine reads them; they do not travel.
    #[serde(skip)]
    pub content_rev: i64,
    #[serde(skip)]
    pub content_principal: String,
    #[serde(skip)]
    pub content_session: String,
    pub kind: String,
    pub scope: String,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub evidence: Vec<String>,
    pub status: Status,
    pub supersedes: Option<String>,
    pub superseded_by: Option<String>,
    /// The latest revision's author.
    pub author: Author,
    /// Distinct (principal, session) pairs whose confirmation counts.
    pub confirmers: i64,
    pub open_disputes: i64,
    pub created_at: String,
    /// The last time anything was written about the memory: a revision, a
    /// confirmation, a dispute or a resolution. Recall's recency and its
    /// newest-first order follow it.
    pub updated_at: String,
}

/// One entry of a memory's history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Revision {
    pub memory_id: String,
    pub rev: i64,
    pub kind: String,
    pub scope: String,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub evidence: Vec<String>,
    pub status: Status,
    pub supersedes: Option<String>,
    pub superseded_by: Option<String>,
    pub author_principal: String,
    pub author_agent: String,
    pub author_session: String,
    pub reason: String,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Confirmation {
    pub rev: i64,
    pub principal: String,
    pub agent: String,
    pub session: String,
    pub evidence: Vec<String>,
    pub created_at: String,
    /// Whether it covers the current text and comes from someone other
    /// than the content author.
    pub counts: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dispute {
    pub rev: i64,
    pub principal: String,
    pub agent: String,
    pub session: String,
    pub reason: String,
    pub evidence: Vec<String>,
    pub created_at: String,
    pub resolved_at: Option<String>,
    pub resolution: Option<String>,
}

/// One entry of the store's log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub seq: i64,
    pub memory_id: String,
    pub verb: String,
    pub rev: Option<i64>,
    pub scope: Option<String>,
    pub principal: String,
    pub agent: String,
    pub session: String,
    pub summary: Option<String>,
    pub created_at: String,
}

/// `GET …/memory`: the store at the gateway's version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreInfo {
    pub schema_version: i64,
    pub supported_version: i64,
    pub store_id: String,
}

/// `POST …/memory/init`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Initialized {
    pub schema_version: i64,
    pub migrated_from: i64,
    pub store_id: String,
}

/// `POST …/memory/memories`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RememberRequest {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(flatten)]
    pub labels: Labels,
}

/// What every write body may carry: client-reported, stored as given.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Labels {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// `POST …/memories/{id}/revise`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviseRequest {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add_evidence: Vec<String>,
    #[serde(flatten)]
    pub labels: Labels,
}

/// `POST …/memories/{id}/confirm`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmRequest {
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<i64>,
    #[serde(flatten)]
    pub labels: Labels,
}

/// `POST …/memories/{id}/dispute`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisputeRequest {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<i64>,
    #[serde(flatten)]
    pub labels: Labels,
}

/// `POST …/memories/{id}/resolve`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveRequest {
    pub reason: String,
    #[serde(flatten)]
    pub labels: Labels,
}

/// `POST …/memories/{id}/supersede`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupersedeRequest {
    pub by: String,
    pub reason: String,
    #[serde(flatten)]
    pub labels: Labels,
}

/// `POST …/memories/{id}/forget`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgetRequest {
    pub reason: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub purge: bool,
    #[serde(flatten)]
    pub labels: Labels,
}

/// A recalled memory: the memory object, with its score when the recall
/// had a query.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recalled {
    #[serde(flatten)]
    pub memory: Memory,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

/// `GET …/memories`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecallResponse {
    pub memories: Vec<Recalled>,
}

/// `GET …/memories/{id}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShowResponse {
    pub memory: Memory,
    pub confirmations: Vec<Confirmation>,
    pub disputes: Vec<Dispute>,
    /// Every revision, oldest first, with `history=true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revisions: Option<Vec<Revision>>,
}

/// `POST …/memories/{id}/supersede`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SupersedeResponse {
    pub superseded: Memory,
    pub by: Memory,
}

/// `POST …/memories/{id}/forget` with `purge`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Purged {
    pub id: String,
    pub purged: bool,
}

/// `GET …/memory/events`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventsResponse {
    /// Newest first.
    pub events: Vec<Event>,
    pub store_id: String,
    /// The `seq` the read started after.
    pub after: i64,
    /// The store's highest `seq`, whatever the filter.
    pub top: i64,
    /// Events passing the filter after `after`; `events` holds the newest
    /// `limit` of them.
    pub matching: i64,
}

/// `GET …/memory/export`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportResponse {
    /// In creation order.
    pub memories: Vec<Memory>,
    /// `after` for the next page; `None` when this page is not full.
    pub next_after: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn memory() -> Memory {
        Memory {
            seq: 7,
            id: "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8".into(),
            rev: 2,
            content_rev: 1,
            content_principal: "key_a".into(),
            content_session: "s-41".into(),
            kind: "pitfall".into(),
            scope: "global".into(),
            title: "t".into(),
            body: String::new(),
            tags: vec!["deploy".into()],
            evidence: vec![],
            status: Status::Confirmed,
            supersedes: None,
            superseded_by: None,
            author: Author {
                principal: "key_a".into(),
                agent: "a".into(),
                session: "s-41".into(),
            },
            confirmers: 1,
            open_disputes: 0,
            created_at: "2026-11-19T10:00:00.000Z".into(),
            updated_at: "2026-11-19T11:00:00.000Z".into(),
        }
    }

    #[test]
    fn the_memory_object_leaves_the_engine_fields_out() {
        let value = serde_json::to_value(memory()).unwrap();
        let keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        for internal in ["seq", "content_rev", "content_principal", "content_session"] {
            assert!(!keys.iter().any(|k| k == internal), "{internal}");
        }
        assert_eq!(value["status"], "confirmed");
        let back: Memory = serde_json::from_value(value).unwrap();
        assert_eq!(back.id, memory().id);
        assert_eq!((back.seq, back.content_rev), (0, 0));
    }

    #[test]
    fn a_score_travels_only_with_a_query() {
        let plain = serde_json::to_value(Recalled {
            memory: memory(),
            score: None,
        })
        .unwrap();
        assert!(plain.get("score").is_none());
        let scored = serde_json::to_value(Recalled {
            memory: memory(),
            score: Some(2.5),
        })
        .unwrap();
        assert_eq!(scored["score"], 2.5);
        assert_eq!(scored["id"], "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8");
    }

    #[test]
    fn requests_leave_out_what_is_not_given() {
        let body = RememberRequest {
            title: "t".into(),
            labels: Labels {
                agent: Some("a".into()),
                session: None,
            },
            ..RememberRequest::default()
        };
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            json!({"title": "t", "agent": "a"})
        );
        let back: RememberRequest =
            serde_json::from_value(json!({"title": "t", "agent": "a"})).unwrap();
        assert_eq!(back, body);
        assert!(
            serde_json::to_value(ShowResponse {
                memory: memory(),
                confirmations: vec![],
                disputes: vec![],
                revisions: None,
            })
            .unwrap()
            .get("revisions")
            .is_none()
        );
    }

    #[test]
    fn statuses_round_trip() {
        for status in [
            Status::Inferred,
            Status::Confirmed,
            Status::Disputed,
            Status::Superseded,
            Status::Forgotten,
        ] {
            assert_eq!(Status::parse(status.as_str()), Some(status));
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                json!(status.as_str())
            );
        }
    }
}
