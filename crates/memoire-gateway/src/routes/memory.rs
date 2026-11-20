//! The memory routes under `…/{name}/memory` (docs/api.md, "Memory
//! routes"). Each handler admits the caller, reads its input, finds the
//! database, runs one engine verb and encodes what it returns; the memory's
//! own checks are the engine's.
use super::{Routes, data::active, data::now_unix_ms, permission};
use crate::{
    http::{Fields, TeamCaller, admit_any, invalid, json_object, refuse},
    memory::{Plane, failure, refused},
};
use axum::{
    Json,
    extract::{Path, Query, Request, State},
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
};
use memoire_api::memory::{
    ConfirmRequest, DEFAULT_EVENTS_LIMIT, DEFAULT_RECALL_LIMIT, DisputeRequest, EventsResponse,
    ExportResponse, ForgetRequest, Initialized, Labels as WireLabels, MAX_EXPORT_PAGE, Purged,
    RecallResponse, Recalled, RememberRequest, ResolveRequest, ReviseRequest, ShowResponse, Status,
    StoreInfo, SupersedeRequest, SupersedeResponse,
};
use memoire_memory::{
    Labels, Refusal, history, model, retire, schema,
    store::{After, Cursor},
    trust, verbs,
};
use merezha_domain::Failure;
use serde_json::{Map, Value};

/// The store of the database `name`, for a caller admitted to the route.
/// Every memory request is a use of the database, recorded before it runs.
async fn plane<'a>(
    routes: &'a Routes,
    caller: &TeamCaller,
    name: &str,
) -> Result<Plane<'a>, Response> {
    let entry = active(routes, caller, name).await?;
    let team = caller.team.as_str();
    routes.last_used.touch(team, &entry.id, now_unix_ms());
    Ok(Plane {
        data: &routes.data,
        target: entry.target(team),
        principal: caller.principal.subject.as_str().to_owned(),
    })
}

/// The memory id of a path: anything else is no memory of the store.
fn memory_id(text: &str) -> Result<String, Response> {
    model::parse_memory_id(text).map_err(|_| refuse(Failure::permanent("memory_not_found")))
}

macro_rules! tri {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(response) => return response,
        }
    };
}

macro_rules! engine {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(error) => return failure(error),
        }
    };
}

/// `GET …/memory`: the store, at the engine's version.
pub async fn store(
    State(routes): State<Routes>,
    Path(name): Path<String>,
    request: Request,
) -> Response {
    let caller = tri!(admit_any(
        &request,
        &routes.policy,
        permission::MEMORY_READERS
    ));
    tri!(QueryString::of(request.uri(), &[]));
    let plane = tri!(plane(&routes, &caller, &name).await);
    let store_id = engine!(schema::store_id(&plane).await);
    Json(StoreInfo {
        schema_version: schema::SCHEMA_VERSION,
        supported_version: schema::SCHEMA_VERSION,
        store_id,
    })
    .into_response()
}

/// `POST …/memory/init`: the store's tables, created or migrated.
pub async fn init(
    State(routes): State<Routes>,
    Path(name): Path<String>,
    request: Request,
) -> Response {
    let caller = tri!(admit_any(
        &request,
        &routes.policy,
        permission::MEMORY_WRITERS
    ));
    let body = tri!(json_object(request, true).await);
    tri!(Fields::new(body, &[]).finish());
    let plane = tri!(plane(&routes, &caller, &name).await);
    let migrated_from = engine!(schema::migrate(&plane).await);
    let store_id = engine!(schema::store_id(&plane).await);
    Json(Initialized {
        schema_version: schema::SCHEMA_VERSION,
        migrated_from,
        store_id,
    })
    .into_response()
}

/// Admits a write route and reads its body.
async fn write_body(
    routes: &Routes,
    request: Request,
    known: &[&str],
) -> Result<(TeamCaller, Body), Response> {
    let caller = admit_any(&request, &routes.policy, permission::MEMORY_WRITERS)?;
    let map = json_object(request, false).await?;
    let mut all = known.to_vec();
    all.extend(["agent", "session"]);
    Ok((caller, Body::new(map, &all)))
}

/// `POST …/memory/memories`: remember.
pub async fn remember(
    State(routes): State<Routes>,
    Path(name): Path<String>,
    request: Request,
) -> Response {
    let (caller, mut body) = tri!(
        write_body(
            &routes,
            request,
            &[
                "title",
                "body",
                "kind",
                "scope",
                "tags",
                "evidence",
                "force",
                "supersedes",
                "reason"
            ],
        )
        .await
    );
    let input = RememberRequest {
        title: body.required_text("title"),
        body: body.text("body"),
        kind: body.text("kind"),
        scope: body.text("scope"),
        tags: body.list("tags"),
        evidence: body.list("evidence"),
        force: body.flag("force"),
        supersedes: body.text("supersedes"),
        reason: body.text("reason"),
        labels: body.labels(),
    };
    tri!(body.finish());
    let plane = tri!(plane(&routes, &caller, &name).await);
    let remembered = verbs::remember(
        &plane,
        &labels(input.labels),
        verbs::Remember {
            title: input.title,
            body: input.body.unwrap_or_default(),
            kind: input.kind,
            scope: input.scope,
            tags: input.tags,
            evidence: input.evidence,
            force: input.force,
            supersedes: input.supersedes,
            reason: input.reason,
        },
        || None,
    )
    .await;
    let memory = match remembered {
        Ok(memory) => memory,
        // A caller that may write but not read learns that a similar
        // title exists, not what the store holds.
        Err(memoire_memory::Error::Refused(Refusal::Duplicate { scope, .. }))
            if !caller.holds_any(permission::MEMORY_READERS) =>
        {
            return refused(&Refusal::Duplicate {
                scope,
                candidates: Vec::new(),
            });
        }
        Err(error) => return failure(error),
    };
    (StatusCode::CREATED, Json(memory)).into_response()
}

/// `GET …/memory/memories`: recall.
pub async fn recall(
    State(routes): State<Routes>,
    Path(name): Path<String>,
    request: Request,
) -> Response {
    let caller = tri!(admit_any(
        &request,
        &routes.policy,
        permission::MEMORY_READERS
    ));
    let params = tri!(QueryString::of(
        request.uri(),
        &["query", "scope", "kind", "tag", "status", "limit"]
    ));
    let mut details = Vec::new();
    let query = params.last("query").map(str::to_owned);
    let scopes = params.all("scope");
    let kinds = params.all("kind");
    let tags = params.all("tag");
    let mut statuses = Vec::new();
    for (i, text) in params.all("status").iter().enumerate() {
        match Status::parse(text) {
            Some(status) => statuses.push(status),
            None => details.push((format!("/status/{i}"), "invalid")),
        }
    }
    let limit = params.int("limit", DEFAULT_RECALL_LIMIT, &mut details);
    if !details.is_empty() {
        return invalid(details);
    }
    let plane = tri!(plane(&routes, &caller, &name).await);
    let found = engine!(
        verbs::recall(
            &plane,
            verbs::Recall {
                query: query.as_deref(),
                scopes: (!scopes.is_empty()).then_some(scopes.as_slice()),
                statuses: &statuses,
                kinds: &kinds,
                tags: &tags,
                limit,
            },
        )
        .await
    );
    Json(RecallResponse {
        memories: found
            .into_iter()
            .map(|(memory, score)| Recalled { memory, score })
            .collect(),
    })
    .into_response()
}

/// `GET …/memory/memories/{id}`: show.
pub async fn show(
    State(routes): State<Routes>,
    Path((name, id)): Path<(String, String)>,
    request: Request,
) -> Response {
    let caller = tri!(admit_any(
        &request,
        &routes.policy,
        permission::MEMORY_READERS
    ));
    let params = tri!(QueryString::of(request.uri(), &["history"]));
    let mut details = Vec::new();
    let history = params.flag("history", &mut details);
    if !details.is_empty() {
        return invalid(details);
    }
    let plane = tri!(plane(&routes, &caller, &name).await);
    let id = tri!(memory_id(&id));
    let shown = engine!(verbs::show(&plane, &id, history).await);
    Json(ShowResponse {
        memory: shown.memory,
        confirmations: shown.confirmations,
        disputes: shown.disputes,
        revisions: shown.revisions,
    })
    .into_response()
}

/// `POST …/memories/{id}/revise`.
pub async fn revise(
    State(routes): State<Routes>,
    Path((name, id)): Path<(String, String)>,
    request: Request,
) -> Response {
    let (caller, mut body) = tri!(
        write_body(
            &routes,
            request,
            &[
                "reason",
                "title",
                "body",
                "kind",
                "scope",
                "tags",
                "add_tags",
                "evidence",
                "add_evidence"
            ],
        )
        .await
    );
    let input = ReviseRequest {
        reason: body.required_text("reason"),
        title: body.text("title"),
        body: body.text("body"),
        kind: body.text("kind"),
        scope: body.text("scope"),
        tags: body.list("tags"),
        add_tags: body.list("add_tags"),
        evidence: body.list("evidence"),
        add_evidence: body.list("add_evidence"),
        labels: body.labels(),
    };
    tri!(body.finish());
    let plane = tri!(plane(&routes, &caller, &name).await);
    let id = tri!(memory_id(&id));
    let memory = engine!(
        verbs::revise(
            &plane,
            &labels(input.labels),
            verbs::Revise {
                id,
                title: input.title,
                body: input.body,
                kind: input.kind,
                scope: input.scope,
                tags: input.tags,
                add_tags: input.add_tags,
                evidence: input.evidence,
                add_evidence: input.add_evidence,
                reason: input.reason,
            },
        )
        .await
    );
    Json(memory).into_response()
}

/// `POST …/memories/{id}/confirm`.
pub async fn confirm(
    State(routes): State<Routes>,
    Path((name, id)): Path<(String, String)>,
    request: Request,
) -> Response {
    let (caller, mut body) = tri!(write_body(&routes, request, &["evidence", "rev"]).await);
    if body.fields.optional("evidence").is_none() {
        body.fields.fail("evidence", "required");
    }
    let input = ConfirmRequest {
        evidence: body.list("evidence"),
        rev: body.int("rev"),
        labels: body.labels(),
    };
    tri!(body.finish());
    let plane = tri!(plane(&routes, &caller, &name).await);
    let id = tri!(memory_id(&id));
    let memory = engine!(
        trust::confirm(
            &plane,
            &labels(input.labels),
            &id,
            input.evidence,
            input.rev
        )
        .await
    );
    Json(memory).into_response()
}

/// `POST …/memories/{id}/dispute`.
pub async fn dispute(
    State(routes): State<Routes>,
    Path((name, id)): Path<(String, String)>,
    request: Request,
) -> Response {
    let (caller, mut body) =
        tri!(write_body(&routes, request, &["reason", "evidence", "rev"]).await);
    let input = DisputeRequest {
        reason: body.required_text("reason"),
        evidence: body.list("evidence"),
        rev: body.int("rev"),
        labels: body.labels(),
    };
    tri!(body.finish());
    let plane = tri!(plane(&routes, &caller, &name).await);
    let id = tri!(memory_id(&id));
    let memory = engine!(
        trust::dispute(
            &plane,
            &labels(input.labels),
            &id,
            &input.reason,
            input.evidence,
            input.rev,
        )
        .await
    );
    Json(memory).into_response()
}

/// `POST …/memories/{id}/resolve`.
pub async fn resolve(
    State(routes): State<Routes>,
    Path((name, id)): Path<(String, String)>,
    request: Request,
) -> Response {
    let (caller, mut body) = tri!(write_body(&routes, request, &["reason"]).await);
    let input = ResolveRequest {
        reason: body.required_text("reason"),
        labels: body.labels(),
    };
    tri!(body.finish());
    let plane = tri!(plane(&routes, &caller, &name).await);
    let id = tri!(memory_id(&id));
    let memory = engine!(trust::resolve(&plane, &labels(input.labels), &id, &input.reason).await);
    Json(memory).into_response()
}

/// `POST …/memories/{id}/supersede`.
pub async fn supersede(
    State(routes): State<Routes>,
    Path((name, id)): Path<(String, String)>,
    request: Request,
) -> Response {
    let (caller, mut body) = tri!(write_body(&routes, request, &["by", "reason"]).await);
    let input = SupersedeRequest {
        by: body.required_text("by"),
        reason: body.required_text("reason"),
        labels: body.labels(),
    };
    tri!(body.finish());
    let plane = tri!(plane(&routes, &caller, &name).await);
    let id = tri!(memory_id(&id));
    let (superseded, by) = engine!(
        retire::supersede(&plane, &labels(input.labels), &id, &input.by, &input.reason).await
    );
    Json(SupersedeResponse { superseded, by }).into_response()
}

/// `POST …/memories/{id}/forget`: a tombstone, or with `purge` the
/// memory erased.
pub async fn forget(
    State(routes): State<Routes>,
    Path((name, id)): Path<(String, String)>,
    request: Request,
) -> Response {
    let (caller, mut body) = tri!(write_body(&routes, request, &["reason", "purge"]).await);
    let input = ForgetRequest {
        reason: body.required_text("reason"),
        purge: body.flag("purge"),
        labels: body.labels(),
    };
    tri!(body.finish());
    let plane = tri!(plane(&routes, &caller, &name).await);
    let id = tri!(memory_id(&id));
    let labels = labels(input.labels);
    if input.purge {
        engine!(retire::purge(&plane, &labels, &id, &input.reason).await);
        return Json(Purged { id, purged: true }).into_response();
    }
    let memory = engine!(retire::forget(&plane, &labels, &id, &input.reason).await);
    Json(memory).into_response()
}

/// `GET …/memory/events`: the log, from where the reader says.
pub async fn events(
    State(routes): State<Routes>,
    Path(name): Path<String>,
    request: Request,
) -> Response {
    let caller = tri!(admit_any(
        &request,
        &routes.policy,
        permission::MEMORY_READERS
    ));
    let params = tri!(QueryString::of(
        request.uri(),
        &[
            "scope",
            "limit",
            "since_seconds",
            "after",
            "store_id",
            "first_look",
        ]
    ));
    let mut details = Vec::new();
    let scopes = params.all("scope");
    let limit = params.int("limit", DEFAULT_EVENTS_LIMIT, &mut details);
    let since_seconds = params.last("since_seconds").and_then(|text| {
        text.parse::<u64>()
            .map_err(|_| details.push(("/since_seconds".to_owned(), "invalid")))
            .ok()
    });
    let after = params.last("after").and_then(|text| {
        text.parse::<i64>()
            .map_err(|_| details.push(("/after".to_owned(), "invalid")))
            .ok()
    });
    let store_id = params.last("store_id").map(str::to_owned);
    let first_look = params.flag("first_look", &mut details);
    let cursor = match (after, store_id) {
        (Some(seq), Some(store_id)) => Some(Cursor { store_id, seq }),
        (Some(_), None) => {
            details.push(("/store_id".to_owned(), "required"));
            None
        }
        (None, Some(_)) => {
            details.push(("/after".to_owned(), "required"));
            None
        }
        (None, None) => None,
    };
    if first_look && (params.last("after").is_some() || params.last("store_id").is_some()) {
        details.push(("/first_look".to_owned(), "not_allowed"));
    }
    if !details.is_empty() {
        return invalid(details);
    }
    let after = match (&cursor, first_look) {
        (Some(cursor), _) => After::Cursor(cursor),
        (None, true) => After::FirstLook,
        (None, false) => After::Start,
    };
    let plane = tri!(plane(&routes, &caller, &name).await);
    let page = engine!(
        history::events(
            &plane,
            history::Events {
                scopes: (!scopes.is_empty()).then_some(scopes.as_slice()),
                since_seconds,
                after,
                limit,
            },
        )
        .await
    );
    Json(EventsResponse {
        events: page.events,
        store_id: page.store_id,
        after: page.after,
        top: page.top,
        matching: page.matching,
    })
    .into_response()
}

/// `GET …/memory/export`: one page of the memories.
pub async fn export(
    State(routes): State<Routes>,
    Path(name): Path<String>,
    request: Request,
) -> Response {
    let caller = tri!(admit_any(
        &request,
        &routes.policy,
        permission::MEMORY_READERS
    ));
    let params = tri!(QueryString::of(request.uri(), &["scope", "after", "limit"]));
    let mut details = Vec::new();
    let scopes = params.all("scope");
    let after = params.int("after", 0, &mut details);
    let limit = params.int("limit", MAX_EXPORT_PAGE, &mut details);
    if !details.is_empty() {
        return invalid(details);
    }
    let plane = tri!(plane(&routes, &caller, &name).await);
    let page = engine!(
        history::export(
            &plane,
            (!scopes.is_empty()).then_some(scopes.as_slice()),
            after,
            limit,
        )
        .await
    );
    Json(ExportResponse {
        memories: page.memories,
        next_after: page.next,
    })
    .into_response()
}

/// The engine's labels from a body's: absent is empty.
fn labels(wire: WireLabels) -> Labels {
    Labels {
        agent: wire.agent.unwrap_or_default(),
        session: wire.session.unwrap_or_default(),
    }
}

/// A write body read field by field: a detail for each field of the wrong
/// JSON type, an unknown field or a missing required one. The syntax and
/// limits of each value are the engine's to check.
struct Body {
    fields: Fields,
}

impl Body {
    fn new(map: Map<String, Value>, known: &[&str]) -> Self {
        Self {
            fields: Fields::new(map, known),
        }
    }

    fn text(&mut self, key: &str) -> Option<String> {
        match self.fields.optional(key) {
            None => None,
            Some(Value::String(text)) => Some(text.clone()),
            Some(_) => {
                self.fields.fail(key, "invalid");
                None
            }
        }
    }

    /// A required string; `""` when missing, which the details report.
    fn required_text(&mut self, key: &str) -> String {
        if self.fields.optional(key).is_none() {
            self.fields.fail(key, "required");
            return String::new();
        }
        self.text(key).unwrap_or_default()
    }

    /// An array of strings, empty when absent.
    fn list(&mut self, key: &str) -> Vec<String> {
        let mut bad = Vec::new();
        let list = match self.fields.optional(key) {
            None => return Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| match item {
                    Value::String(text) => Some(text.clone()),
                    _ => {
                        bad.push(format!("/{key}/{i}"));
                        None
                    }
                })
                .collect(),
            Some(_) => {
                self.fields.fail(key, "invalid");
                return Vec::new();
            }
        };
        for path in bad {
            self.fields.details.push((path, "invalid"));
        }
        list
    }

    fn flag(&mut self, key: &str) -> bool {
        match self.fields.optional(key) {
            None => false,
            Some(Value::Bool(flag)) => *flag,
            Some(_) => {
                self.fields.fail(key, "invalid");
                false
            }
        }
    }

    fn int(&mut self, key: &str) -> Option<i64> {
        match self.fields.optional(key).map(Value::as_i64) {
            None => None,
            Some(Some(n)) => Some(n),
            Some(None) => {
                self.fields.fail(key, "invalid");
                None
            }
        }
    }

    fn labels(&mut self) -> WireLabels {
        WireLabels {
            agent: self.text("agent"),
            session: self.text("session"),
        }
    }

    fn finish(self) -> Result<(), Response> {
        self.fields.finish()
    }
}

/// A route's query string, percent-decoded, with repeatable parameters.
struct QueryString(Vec<(String, String)>);

impl QueryString {
    /// The parameters of `uri`; a name not in `known` fails
    /// `unknown_field` at its path, as an unknown body field does.
    fn of(uri: &Uri, known: &[&str]) -> Result<Self, Response> {
        let Ok(Query(pairs)) = Query::<Vec<(String, String)>>::try_from_uri(uri) else {
            return Err(invalid(vec![(String::new(), "invalid")]));
        };
        let mut unknown: Vec<(String, &'static str)> = Vec::new();
        for (name, _) in &pairs {
            let path = format!("/{}", name.replace('~', "~0").replace('/', "~1"));
            if !known.contains(&name.as_str()) && !unknown.iter().any(|(p, _)| *p == path) {
                unknown.push((path, "unknown_field"));
            }
        }
        if unknown.is_empty() {
            Ok(Self(pairs))
        } else {
            Err(invalid(unknown))
        }
    }

    /// Every value of `key`, in order.
    fn all(&self, key: &str) -> Vec<String> {
        self.0
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .collect()
    }

    /// The last value of `key`.
    fn last(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// An integer, `default` when absent. Its range is the engine's.
    fn int(&self, key: &str, default: i64, details: &mut Vec<(String, &'static str)>) -> i64 {
        match self.last(key).map(str::parse::<i64>) {
            None => default,
            Some(Ok(n)) => n,
            Some(Err(_)) => {
                details.push((format!("/{key}"), "invalid"));
                default
            }
        }
    }

    /// `true` or `false`, `false` when absent.
    fn flag(&self, key: &str, details: &mut Vec<(String, &'static str)>) -> bool {
        match self.last(key) {
            None | Some("false") => false,
            Some("true") => true,
            Some(_) => {
                details.push((format!("/{key}"), "invalid"));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn query(text: &str) -> QueryString {
        QueryString::of(
            &format!("/x?{text}").parse().unwrap(),
            &["scope", "limit", "query", "after", "history"],
        )
        .ok()
        .unwrap()
    }

    #[test]
    fn query_strings_keep_repeats_and_decode() {
        let params = query("scope=global&scope=client%3Aacme&limit=5&query=deploy+vpn");
        assert_eq!(params.all("scope"), ["global", "client:acme"]);
        assert_eq!(params.last("query"), Some("deploy vpn"));
        let mut details = Vec::new();
        assert_eq!(params.int("limit", 10, &mut details), 5);
        assert_eq!(params.int("after", 0, &mut details), 0);
        assert!(details.is_empty());
        assert_eq!(query("limit=x").int("limit", 10, &mut details), 10);
        assert!(!query("history=false").flag("history", &mut details));
        assert!(query("history=true").flag("history", &mut details));
        query("history=yes").flag("history", &mut details);
        assert_eq!(
            details,
            vec![
                ("/limit".to_owned(), "invalid"),
                ("/history".to_owned(), "invalid")
            ]
        );
    }

    #[tokio::test]
    async fn unknown_parameters_fail() {
        let uri = "/x?scpoe=global&limit=2&scpoe=x&a%2Fb=1".parse().unwrap();
        let response = QueryString::of(&uri, &["scope", "limit"]).err().unwrap();
        assert_eq!(response.status(), 422);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body["error"]["details"],
            json!([{"path": "/scpoe", "code": "unknown_field"},
                   {"path": "/a~1b", "code": "unknown_field"}])
        );
        assert!(QueryString::of(&"/x".parse().unwrap(), &[]).is_ok());
        assert!(QueryString::of(&"/x?limit=1".parse().unwrap(), &[]).is_err());
    }

    #[test]
    fn bodies_report_each_field() {
        let map = |value: Value| value.as_object().unwrap().clone();
        let mut body = Body::new(
            map(
                json!({"tags": ["a", 1, "b", null], "force": "yes", "extra": 1,
                       "agent": null, "rev": 1.5}),
            ),
            &["title", "tags", "force", "agent", "rev"],
        );
        assert_eq!(body.required_text("title"), "");
        assert_eq!(body.list("tags"), ["a", "b"]);
        assert!(!body.flag("force"));
        assert_eq!(body.int("rev"), None);
        assert_eq!(body.labels(), WireLabels::default());
        let mut details = body.fields.details.clone();
        details.sort();
        assert_eq!(
            details,
            vec![
                ("/extra".to_owned(), "unknown_field"),
                ("/force".to_owned(), "invalid"),
                ("/rev".to_owned(), "invalid"),
                ("/tags/1".to_owned(), "invalid"),
                ("/tags/3".to_owned(), "invalid"),
                ("/title".to_owned(), "required"),
            ]
        );
    }
}
