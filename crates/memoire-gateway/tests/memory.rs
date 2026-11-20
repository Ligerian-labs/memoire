//! The memory routes through the gateway: the engine on the data plane, its
//! refusals with their `data` through the real edge, and who may call
//! them, on a real host and PostgreSQL.
mod common;

use common::*;
use serde_json::{Value, json};

const SETTINGS: &str = r#"data_dir = "data""#;

/// A caller: a key's `Authorization` header.
struct Key<'a> {
    gateway: &'a Gateway,
    auth: String,
}

impl Key<'_> {
    fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.gateway.request(
            method,
            &format!("/v1/memoire{path}"),
            &[("authorization", self.auth.as_str())],
            body.as_ref(),
        )
    }

    /// A route of database `db`'s memory.
    fn memory(&self, db: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.call(method, &format!("/databases/{db}/memory{path}"), body)
    }

    /// The raw response text of a memory route, to check what it holds.
    fn raw(&self, db: &str, path: &str, body: &Value) -> String {
        let body = body.to_string();
        let (_, head, bytes) = self.gateway.raw(
            "POST",
            &format!("/v1/memoire/databases/{db}/memory{path}"),
            &[("authorization", self.auth.as_str())],
            Some(("application/json", body.as_bytes())),
        );
        format!("{head}\r\n\r\n{}", String::from_utf8_lossy(&bytes))
    }

    fn subject(&self) -> String {
        let (status, body) = self.call("GET", "/whoami", None);
        assert_eq!(status, 200, "{body}");
        body["subject"].as_str().unwrap().to_owned()
    }
}

/// A key of `team` with `permissions`.
fn key<'a>(gateway: &'a Gateway, owner: &Member, team: &str, permissions: &[&str]) -> Key<'a> {
    let (status, body) = api_key(gateway, owner, team, permissions, None);
    assert_eq!(status, 201, "{body}");
    Key {
        gateway,
        auth: format!("Bearer {}", body["key"].as_str().unwrap()),
    }
}

const MEMORY: &[&str] = &["memoire.memory.read", "memoire.memory.write"];

fn code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("")
}

fn details(body: &Value) -> Value {
    body["error"]["details"].clone()
}

/// A gateway with one signed-in owner, its team and a persistent database
/// `name`, created by the owner.
fn setup(gateway: &Gateway, email: &str, slug: &str, name: &str) -> (Member, String) {
    let owner = register(gateway, email);
    let team = create_team(gateway, &owner, slug);
    let creator = key(gateway, &owner, &team, &["memoire.databases.create"]);
    let (status, body) = creator.call(
        "POST",
        "/databases",
        Some(json!({"name": name, "kind": "persistent"})),
    );
    assert_eq!(status, 201, "{body}");
    (owner, team)
}

fn id_of(memory: &Value) -> String {
    memory["id"].as_str().unwrap().to_owned()
}

fn ids(memories: &Value) -> Vec<String> {
    memories
        .as_array()
        .unwrap()
        .iter()
        .map(id_of)
        .collect::<Vec<_>>()
}

#[test]
fn a_memory_lives_its_whole_life_over_http() {
    let Some(db) = database() else { return };
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::start(&config(dir.path(), SETTINGS), &db);
    let (owner, team) = setup(&gateway, "life@memoire.test", "life-team", "mem");
    let a = key(&gateway, &owner, &team, MEMORY);
    let b = key(&gateway, &owner, &team, MEMORY);
    let reader = key(&gateway, &owner, &team, &["memoire.memory.read"]);
    let (subject_a, subject_b) = (a.subject(), b.subject());
    assert!(subject_a.starts_with("key_"), "{subject_a}");

    // Before init: no database, then no store.
    let (status, body) = a.memory("missing", "POST", "/init", None);
    assert_eq!((status, code(&body)), (404, "database_not_found"), "{body}");
    let not_initialized =
        json!({"error": {"code": "memory_not_initialized", "classification": "conflict"}});
    assert_eq!(
        reader.memory("mem", "GET", "", None),
        (409, not_initialized.clone())
    );
    assert_eq!(
        a.memory("mem", "GET", "/memories", None),
        (409, not_initialized.clone())
    );

    // Init, twice.
    let (status, first) = a.memory("mem", "POST", "/init", None);
    assert_eq!(status, 200, "{first}");
    assert_eq!(
        (
            first["schema_version"].clone(),
            first["migrated_from"].clone()
        ),
        (json!(1), json!(0))
    );
    let store_id = first["store_id"].as_str().unwrap().to_owned();
    assert!(store_id.starts_with("mst_"), "{first}");
    let (status, again) = a.memory("mem", "POST", "/init", Some(json!({})));
    assert_eq!(status, 200, "{again}");
    assert_eq!(
        again,
        json!({"schema_version": 1, "migrated_from": 1, "store_id": store_id})
    );
    assert_eq!(
        reader.memory("mem", "GET", "", None),
        (
            200,
            json!({"schema_version": 1, "supported_version": 1, "store_id": store_id})
        )
    );

    // Remember: the principal is the key's, agent and session the body's.
    let (status, m1) = a.memory(
        "mem",
        "POST",
        "/memories",
        Some(json!({
            "title": "Widgets deploys fail with a timeout unless the VPN is connected",
            "kind": "pitfall", "scope": "project:example.com/acme/widgets",
            "tags": ["deploy", "vpn"], "evidence": ["make deploy, run at 14:02 UTC"],
            "agent": "agent-a", "session": "s-1",
        })),
    );
    assert_eq!(status, 201, "{m1}");
    assert_eq!(
        m1["author"],
        json!({"principal": subject_a, "agent": "agent-a", "session": "s-1"})
    );
    assert_eq!(
        (m1["rev"].clone(), m1["status"].clone()),
        (json!(1), json!("inferred"))
    );
    let id1 = id_of(&m1);
    assert!(id1.starts_with("mem_") && id1.len() == 30, "{id1}");
    let (status, m2) = a.memory(
        "mem",
        "POST",
        "/memories",
        Some(json!({"title": "Release notes live in the changelog", "body": null})),
    );
    assert_eq!(status, 201, "{m2}");
    assert_eq!(
        (m2["scope"].clone(), m2["kind"].clone()),
        (json!("global"), json!("fact"))
    );
    assert_eq!(m2["author"]["session"], "");
    let id2 = id_of(&m2);

    // Recall: newest first without a query, scored with one.
    let (status, plain) = reader.memory("mem", "GET", "/memories", None);
    assert_eq!(status, 200, "{plain}");
    assert_eq!(ids(&plain["memories"]), [id2.clone(), id1.clone()]);
    assert!(plain["memories"][0].get("score").is_none(), "{plain}");
    let (_, found) = reader.memory("mem", "GET", "/memories?query=vpn+deploying", None);
    assert_eq!(ids(&found["memories"]), std::slice::from_ref(&id1));
    assert!(
        found["memories"][0]["score"].as_f64().unwrap() > 0.0,
        "{found}"
    );
    for (query, expected) in [
        ("scope=global", vec![id2.clone()]),
        (
            "scope=global&scope=project%3Aexample.com%2Facme%2Fwidgets",
            vec![id2.clone(), id1.clone()],
        ),
        ("kind=pitfall", vec![id1.clone()]),
        ("kind=fact&kind=pitfall", vec![id2.clone(), id1.clone()]),
        ("tag=deploy&tag=vpn", vec![id1.clone()]),
        ("tag=deploy&tag=other", vec![]),
        ("status=confirmed", vec![]),
        ("status=inferred&limit=1", vec![id2.clone()]),
    ] {
        let (status, body) = reader.memory("mem", "GET", &format!("/memories?{query}"), None);
        assert_eq!(status, 200, "{query}: {body}");
        assert_eq!(ids(&body["memories"]), expected, "{query}");
    }

    // Show, revise, confirm by another key, dispute, resolve.
    let (status, shown) =
        reader.memory("mem", "GET", &format!("/memories/{id1}?history=true"), None);
    assert_eq!(status, 200, "{shown}");
    assert_eq!(
        (shown["confirmations"].clone(), shown["disputes"].clone()),
        (json!([]), json!([]))
    );
    assert_eq!(shown["revisions"][0]["reason"], "remembered");
    let lower = format!("mem_{}", id1[4..].to_ascii_lowercase());
    let (status, shown) = reader.memory("mem", "GET", &format!("/memories/{lower}"), None);
    assert_eq!((status, shown["memory"]["id"].clone()), (200, json!(id1)));
    assert!(shown.get("revisions").is_none(), "{shown}");
    let (status, revised) = a.memory(
        "mem",
        "POST",
        &format!("/memories/{id1}/revise"),
        Some(
            json!({"reason": "say what to do", "body": "Connect the VPN first.",
                    "add_tags": ["network"], "session": "s-1"}),
        ),
    );
    assert_eq!(status, 200, "{revised}");
    assert_eq!(revised["rev"], 2);
    assert_eq!(revised["tags"], json!(["deploy", "vpn", "network"]));
    let (status, refused) = a.memory(
        "mem",
        "POST",
        &format!("/memories/{id1}/confirm"),
        Some(json!({"evidence": ["ran it"], "session": "s-1"})),
    );
    assert_eq!(
        (status, refused),
        (
            422,
            json!({"error": {"code": "memory_self_confirm", "classification": "permanent"},
                   "data": {"id": id1, "without_session": false}})
        )
    );
    let (status, confirmed) = b.memory(
        "mem",
        "POST",
        &format!("/memories/{id1}/confirm"),
        Some(json!({"evidence": ["make deploy with the VPN up"], "rev": 2, "agent": "agent-b", "session": "s-2"})),
    );
    assert_eq!(status, 200, "{confirmed}");
    assert_eq!(
        (confirmed["status"].clone(), confirmed["confirmers"].clone()),
        (json!("confirmed"), json!(1))
    );
    let (_, disputed) = b.memory(
        "mem",
        "POST",
        &format!("/memories/{id1}/dispute"),
        Some(json!({"reason": "it passed once without the VPN", "evidence": ["make deploy"]})),
    );
    assert_eq!(
        (
            disputed["status"].clone(),
            disputed["open_disputes"].clone()
        ),
        (json!("disputed"), json!(1))
    );
    let (status, resolved) = a.memory(
        "mem",
        "POST",
        &format!("/memories/{id1}/resolve"),
        Some(json!({"reason": "the VPN was up after all"})),
    );
    assert_eq!(status, 200, "{resolved}");
    assert_eq!(resolved["status"], "confirmed");
    let (_, shown) = reader.memory("mem", "GET", &format!("/memories/{id1}"), None);
    assert_eq!(shown["confirmations"][0]["principal"], json!(subject_b));
    assert_eq!(shown["confirmations"][0]["counts"], true);
    assert_eq!(
        shown["disputes"][0]["resolution"],
        "the VPN was up after all"
    );

    // Remember with supersedes: the replaced memory's scope, kind and tags.
    let (status, m3) = a.memory(
        "mem",
        "POST",
        "/memories",
        Some(
            json!({"title": "Widgets deploys need the VPN connected before make deploy",
                    "supersedes": lower, "reason": "rewritten as a procedure"}),
        ),
    );
    assert_eq!(status, 201, "{m3}");
    let id3 = id_of(&m3);
    assert_eq!(m3["supersedes"], json!(id1));
    assert_eq!(
        (m3["scope"].clone(), m3["kind"].clone(), m3["tags"].clone()),
        (
            json!("project:example.com/acme/widgets"),
            json!("pitfall"),
            json!(["deploy", "vpn", "network"])
        )
    );
    let (_, shown) = reader.memory("mem", "GET", &format!("/memories/{id1}"), None);
    assert_eq!(
        (
            shown["memory"]["status"].clone(),
            shown["memory"]["superseded_by"].clone()
        ),
        (json!("superseded"), json!(id3))
    );

    // Supersede, and the cycle it refuses.
    let (status, pair) = a.memory(
        "mem",
        "POST",
        &format!("/memories/{id2}/supersede"),
        Some(json!({"by": id3, "reason": "folded in"})),
    );
    assert_eq!(status, 200, "{pair}");
    assert_eq!(pair["superseded"]["status"], "superseded");
    assert_eq!(pair["by"]["supersedes"], json!(id2));
    let (status, body) = a.memory(
        "mem",
        "POST",
        &format!("/memories/{id3}/supersede"),
        Some(json!({"by": id2, "reason": "back again"})),
    );
    assert_eq!(
        (status, body),
        (
            409,
            json!({"error": {"code": "memory_cycle", "classification": "conflict"},
                   "data": {"id": id3, "by": id2}})
        )
    );

    // Forget, then purge.
    let (status, forgotten) = a.memory(
        "mem",
        "POST",
        &format!("/memories/{id3}/forget"),
        Some(json!({"reason": "obsolete", "purge": false})),
    );
    assert_eq!(
        (status, forgotten["status"].clone()),
        (200, json!("forgotten"))
    );
    let (status, body) = a.memory(
        "mem",
        "POST",
        &format!("/memories/{id3}/forget"),
        Some(json!({"reason": "again"})),
    );
    assert_eq!((status, code(&body)), (409, "memory_not_current"), "{body}");
    let (status, purged) = a.memory(
        "mem",
        "POST",
        &format!("/memories/{id2}/forget"),
        Some(json!({"reason": "a leak", "purge": true})),
    );
    assert_eq!((status, purged), (200, json!({"id": id2, "purged": true})));
    let (status, gone) = reader.memory("mem", "GET", &format!("/memories/{id2}"), None);
    assert_eq!((status, code(&gone)), (404, "memory_not_found"), "{gone}");
    assert_eq!(gone["data"]["id"], json!(id2));
    assert!(
        gone["data"]["purged_at"].as_str().unwrap().ends_with('Z'),
        "{gone}"
    );

    // The log, plain, from a cursor, and as a first look.
    let (status, log) = reader.memory("mem", "GET", "/events", None);
    assert_eq!(status, 200, "{log}");
    let events = log["events"].as_array().unwrap();
    let top = events[0]["seq"].as_i64().unwrap();
    assert_eq!(
        (
            log["store_id"].clone(),
            log["after"].clone(),
            log["top"].clone(),
            log["matching"].clone()
        ),
        (json!(store_id), json!(0), json!(top), json!(events.len()))
    );
    assert_eq!(events.last().unwrap()["verb"], "remember");
    assert_eq!(events[0]["verb"], "purge");
    let (_, short) = reader.memory("mem", "GET", "/events?limit=2", None);
    assert_eq!(short["events"].as_array().unwrap().len(), 2);
    assert_eq!(short["matching"], json!(events.len()));
    let (_, cursor) = reader.memory(
        "mem",
        "GET",
        &format!("/events?after={}&store_id={store_id}", top - 2),
        None,
    );
    assert_eq!(
        (cursor["after"].clone(), cursor["matching"].clone()),
        (json!(top - 2), json!(2))
    );
    let other = format!("mst_{}", "0".repeat(26));
    let (_, elsewhere) = reader.memory(
        "mem",
        "GET",
        &format!("/events?after=3&store_id={other}"),
        None,
    );
    assert_eq!(
        (elsewhere["after"].clone(), elsewhere["matching"].clone()),
        (json!(0), json!(events.len()))
    );
    let (_, first) = reader.memory("mem", "GET", "/events?first_look=true", None);
    assert_eq!(
        (first["after"].clone(), first["top"].clone()),
        (json!(0), json!(top))
    );
    let (_, scoped) = reader.memory(
        "mem",
        "GET",
        "/events?scope=global&since_seconds=3600",
        None,
    );
    assert!(
        scoped["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["scope"] == "global"),
        "{scoped}"
    );
    for (query, path, why) in [
        (
            "first_look=true&after=1&store_id=x",
            "/first_look",
            "not_allowed",
        ),
        ("after=1", "/store_id", "required"),
        (
            "since_seconds=60&first_look=true",
            "/since_seconds",
            "not_allowed",
        ),
        ("limit=1001", "/limit", "out_of_range"),
        ("after=1&store_id=mst_nope", "/store_id", "invalid"),
    ] {
        let (status, body) = reader.memory("mem", "GET", &format!("/events?{query}"), None);
        assert_eq!(
            (status, details(&body)),
            (422, json!([{"path": path, "code": why}])),
            "{query}: {body}"
        );
    }

    // Export, page by page: every status but forgotten.
    let mut exported = Vec::new();
    let mut after = 0;
    loop {
        let (status, page) = reader.memory(
            "mem",
            "GET",
            &format!("/export?limit=1&after={after}"),
            None,
        );
        assert_eq!(status, 200, "{page}");
        exported.extend(ids(&page["memories"]));
        match page["next_after"].as_i64() {
            Some(next) => after = next,
            None => break,
        }
    }
    assert_eq!(exported, std::slice::from_ref(&id1));
    let (_, all) = reader.memory("mem", "GET", "/export", None);
    assert_eq!(
        (ids(&all["memories"]), all["next_after"].clone()),
        (vec![id1.clone()], Value::Null)
    );

    // One store, two ways: the rows are plain SQL over the query route.
    let sql = key(&gateway, &owner, &team, &["memoire.databases.read"]);
    let (status, rows) = sql.call(
        "POST",
        "/databases/mem/query",
        Some(json!({"statements": [
            {"sql": "SELECT id, author_principal, status FROM memoire_memory_memories ORDER BY seq"},
            {"sql": "SELECT count(*) FROM memoire_memory_events WHERE memory_id = ?", "params": [id1]},
            {"sql": "SELECT value FROM memoire_memory_meta WHERE key = 'schema_version'"},
        ]})),
    );
    assert_eq!(status, 200, "{rows}");
    assert_eq!(
        rows["results"][0]["rows"],
        json!([
            [id1, subject_a, "superseded"],
            [id3, subject_a, "forgotten"]
        ])
    );
    assert!(
        rows["results"][1]["rows"][0][0].as_i64().unwrap() >= 5,
        "{rows}"
    );
    assert_eq!(rows["results"][2]["rows"], json!([["1"]]));

    // A memory request is a use of the database.
    let (_, detail) = sql.call("GET", "/databases/mem", None);
    assert!(detail["last_used_at"].is_string(), "{detail}");
}

#[test]
fn refusals_carry_their_data_through_the_edge() {
    let Some(db) = database() else { return };
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::start(&config(dir.path(), SETTINGS), &db);
    let (owner, team) = setup(&gateway, "refusals@memoire.test", "refusals-team", "mem");
    let a = key(&gateway, &owner, &team, MEMORY);
    let b = key(&gateway, &owner, &team, MEMORY);
    assert_eq!(a.memory("mem", "POST", "/init", None).0, 200);
    let remember = |key: &Key<'_>, body: Value| key.memory("mem", "POST", "/memories", Some(body));

    // memory_duplicate: the candidates and the scope.
    let (status, first) = remember(
        &a,
        json!({"title": "Deploys require the VPN to be connected"}),
    );
    assert_eq!(status, 201, "{first}");
    let id = id_of(&first);
    let (status, body) = remember(&a, json!({"title": "Deploys require the VPN connected"}));
    assert_eq!(
        (status, body),
        (
            409,
            json!({"error": {"code": "memory_duplicate", "classification": "conflict"},
                   "data": {"candidates": [{"id": id, "title": "Deploys require the VPN to be connected",
                                            "status": "inferred", "similarity": 1.0}],
                            "scope": "global"}})
        )
    );
    let (status, _) = remember(
        &a,
        json!({"title": "Deploys require the VPN connected", "force": true}),
    );
    assert_eq!(status, 201);

    // memory_conflict, deterministic: a verdict on a rev whose text changed.
    let (status, revised) = a.memory(
        "mem",
        "POST",
        &format!("/memories/{id}/revise"),
        Some(json!({"reason": "sharper", "title": "Deploys need the VPN up"})),
    );
    assert_eq!(
        (status, revised["rev"].clone()),
        (200, json!(2)),
        "{revised}"
    );
    let (status, body) = b.memory(
        "mem",
        "POST",
        &format!("/memories/{id}/confirm"),
        Some(json!({"evidence": ["read rev 1"], "rev": 1})),
    );
    assert_eq!(
        (status, body),
        (
            409,
            json!({"error": {"code": "memory_conflict", "classification": "conflict"},
                   "data": {"id": id, "current_rev": 2, "since_rev": 1}})
        )
    );

    // memory_conflict under a burst: revises of one memory at once. Each
    // answers its revision or the conflict, and the revisions written are
    // the ones answered.
    let barrier = std::sync::Barrier::new(6);
    let answers: Vec<(u16, Value)> = std::thread::scope(|scope| {
        let tasks: Vec<_> = (0..6)
            .map(|i| {
                let (a, barrier, id) = (&a, &barrier, &id);
                scope.spawn(move || {
                    barrier.wait();
                    a.memory(
                        "mem",
                        "POST",
                        &format!("/memories/{id}/revise"),
                        Some(json!({"reason": "burst", "body": format!("writer {i}")})),
                    )
                })
            })
            .collect();
        tasks.into_iter().map(|t| t.join().unwrap()).collect()
    });
    let won = answers.iter().filter(|(status, _)| *status == 200).count();
    assert!(won >= 1, "{answers:?}");
    for (status, body) in &answers {
        match status {
            200 => assert!(body["rev"].as_i64().unwrap() > 2, "{body}"),
            409 => {
                let current = body["data"]["current_rev"].as_i64().unwrap();
                assert!(current > 2, "{body}");
                assert_eq!(
                    body,
                    &json!({"error": {"code": "memory_conflict", "classification": "conflict"},
                            "data": {"id": id, "current_rev": current}})
                );
            }
            _ => panic!("{status} {body}"),
        }
    }
    let (_, shown) = a.memory("mem", "GET", &format!("/memories/{id}"), None);
    assert_eq!(shown["memory"]["rev"], json!(2 + won), "{answers:?}");
    eprintln!("burst of 6 revises: {won} won");

    // memory_unchanged, memory_not_current.
    let (status, body) = a.memory(
        "mem",
        "POST",
        &format!("/memories/{id}/revise"),
        Some(json!({"reason": "same", "title": "Deploys need the VPN up"})),
    );
    assert_eq!(
        (status, body),
        (
            422,
            json!({"error": {"code": "memory_unchanged", "classification": "permanent"},
                   "data": {"id": id}})
        )
    );
    let (status, _) = a.memory(
        "mem",
        "POST",
        &format!("/memories/{id}/forget"),
        Some(json!({"reason": "gone"})),
    );
    assert_eq!(status, 200);
    let (status, body) = a.memory(
        "mem",
        "POST",
        &format!("/memories/{id}/revise"),
        Some(json!({"reason": "late", "body": "x"})),
    );
    assert_eq!(
        (status, body),
        (
            409,
            json!({"error": {"code": "memory_not_current", "classification": "conflict"},
                   "data": {"id": id, "status": "forgotten", "superseded_by": null}})
        )
    );

    // memory_secret_detected: the pattern and field, never the text.
    let secret = format!("{}{}", "gh", "p_") + &"Zq7".repeat(12);
    let request = json!({"title": "Where the CI token is", "body": format!("token: {secret}")});
    let raw = a.raw("mem", "/memories", &request);
    assert!(!raw.contains(&secret), "{raw}");
    let (status, body) = remember(&a, request);
    assert_eq!(
        (status, body),
        (
            422,
            json!({"error": {"code": "memory_secret_detected", "classification": "permanent"},
                   "data": {"pattern": "github_token", "field": "body"}})
        )
    );

    // memory_too_large: the field, its limit and its size.
    let (status, body) = remember(&a, json!({"title": "t".repeat(121)}));
    assert_eq!(
        (status, body),
        (
            422,
            json!({"error": {"code": "memory_too_large", "classification": "permanent"},
                   "data": {"field": "title", "limit": 120, "size": 121}})
        )
    );

    // validation_failed: the body's shape, then the engine's checks.
    let invalid = |path: &str, why: &str| {
        json!({"error": {"code": "validation_failed", "classification": "permanent",
                         "details": [{"path": path, "code": why}]}})
    };
    for (body, path, why) in [
        (json!({"title": "x", "extra": 1}), "/extra", "unknown_field"),
        (json!({"body": "no title"}), "/title", "required"),
        (
            json!({"title": "x", "tags": ["ok", "Bad"]}),
            "/tags/1",
            "invalid",
        ),
        (json!({"title": "x", "tags": "ok"}), "/tags", "invalid"),
        (json!({"title": "two\nlines"}), "/title", "invalid"),
        (json!({"title": "x", "kind": "K"}), "/kind", "invalid"),
        (
            json!({"title": "x", "evidence": ["a", " "]}),
            "/evidence/1",
            "invalid",
        ),
        (
            json!({"title": "x", "supersedes": "mem_x", "reason": "r"}),
            "/supersedes",
            "invalid",
        ),
        (
            json!({"title": "x", "reason": "r"}),
            "/supersedes",
            "required",
        ),
        (
            json!({"title": "x", "session": "s\u{1}"}),
            "/session",
            "invalid",
        ),
    ] {
        assert_eq!(
            remember(&a, body.clone()),
            (422, invalid(path, why)),
            "{body}"
        );
    }
    let (status, other) = remember(&a, json!({"title": "Something else entirely"}));
    assert_eq!(status, 201);
    let other = id_of(&other);
    for (verb, body, path, why) in [
        ("revise", json!({"reason": "r"}), "", "required"),
        (
            "revise",
            json!({"reason": "r", "tags": ["a"], "add_tags": ["b"]}),
            "/add_tags",
            "not_allowed",
        ),
        ("revise", json!({"title": "x"}), "/reason", "required"),
        ("confirm", json!({}), "/evidence", "required"),
        ("confirm", json!({"evidence": []}), "/evidence", "required"),
        (
            "confirm",
            json!({"evidence": ["e"], "rev": 0}),
            "/rev",
            "out_of_range",
        ),
        (
            "confirm",
            json!({"evidence": ["e"], "rev": "1"}),
            "/rev",
            "invalid",
        ),
        ("dispute", json!({"reason": "  "}), "/reason", "required"),
        (
            "supersede",
            json!({"by": "nope", "reason": "r"}),
            "/by",
            "invalid",
        ),
        (
            "forget",
            json!({"reason": "r", "purge": 1}),
            "/purge",
            "invalid",
        ),
    ] {
        let (status, body) = b.memory(
            "mem",
            "POST",
            &format!("/memories/{other}/{verb}"),
            Some(body.clone()),
        );
        assert_eq!((status, body), (422, invalid(path, why)), "{verb}");
    }
    // A rev past the current one: the detail, and what to say about it.
    let (status, body) = b.memory(
        "mem",
        "POST",
        &format!("/memories/{other}/confirm"),
        Some(json!({"evidence": ["e"], "rev": 9})),
    );
    assert_eq!(
        (status, body),
        (
            422,
            json!({"error": {"code": "validation_failed", "classification": "permanent",
                             "details": [{"path": "/rev", "code": "out_of_range"}]},
                   "data": {"id": other, "rev": 9, "current_rev": 1}})
        )
    );
    for (query, path, why) in [
        ("limit=0", "/limit", "out_of_range"),
        ("limit=x", "/limit", "invalid"),
        ("status=nope", "/status/0", "invalid"),
        ("scope=global&scope=Bad", "/scope/1", "invalid"),
        ("tag=Bad", "/tag/0", "invalid"),
        ("query=%2B%2B", "/query", "invalid"),
    ] {
        let (status, body) = a.memory("mem", "GET", &format!("/memories?{query}"), None);
        assert_eq!((status, body), (422, invalid(path, why)), "{query}");
    }
    let (status, body) = a.memory("mem", "GET", "/export?limit=501", None);
    assert_eq!((status, body), (422, invalid("/limit", "out_of_range")));

    // A path that is no memory id is no memory.
    let not_found = json!({"error": {"code": "memory_not_found", "classification": "permanent"}});
    assert_eq!(
        a.memory("mem", "GET", "/memories/not-an-id", None),
        (404, not_found.clone())
    );
    assert_eq!(
        a.memory(
            "mem",
            "POST",
            "/memories/mem_x/confirm",
            Some(json!({"evidence": ["e"]}))
        ),
        (404, not_found)
    );
    let missing = format!("mem_{}", "0".repeat(26));
    assert_eq!(
        a.memory("mem", "GET", &format!("/memories/{missing}"), None),
        (
            404,
            json!({"error": {"code": "memory_not_found", "classification": "permanent"},
                   "data": {"id": missing}})
        )
    );
    // The body's media type, as everywhere.
    let (status, _, _) = gateway.raw(
        "POST",
        "/v1/memoire/databases/mem/memory/memories",
        &[("authorization", a.auth.as_str())],
        Some(("text/plain", b"{}")),
    );
    assert_eq!(status, 415);
}

#[test]
fn memory_permissions_or_database_permissions_open_the_routes() {
    let Some(db) = database() else { return };
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::start(&config(dir.path(), SETTINGS), &db);
    let (owner, team) = setup(&gateway, "grants@memoire.test", "grants-team", "shared");

    // Identity's catalog lists the memory permissions too.
    let bearer = owner.bearer();
    let (status, catalog) = gateway.request(
        "GET",
        "/v1/identity/permissions",
        &[("authorization", bearer.as_str())],
        None,
    );
    assert_eq!(status, 200, "{catalog}");
    for permission in MEMORY {
        assert!(catalog.to_string().contains(permission), "{permission}");
    }

    let memory_rw = key(&gateway, &owner, &team, MEMORY);
    let memory_read = key(&gateway, &owner, &team, &["memoire.memory.read"]);
    let db_read = key(&gateway, &owner, &team, &["memoire.databases.read"]);
    let db_rw = key(
        &gateway,
        &owner,
        &team,
        &["memoire.databases.read", "memoire.databases.write"],
    );
    assert_eq!(memory_rw.memory("shared", "POST", "/init", None).0, 200);
    let (status, memory) = memory_rw.memory(
        "shared",
        "POST",
        "/memories",
        Some(json!({"title": "A shared fact"})),
    );
    assert_eq!(status, 201, "{memory}");
    let id = id_of(&memory);
    let (status, by) = db_rw.memory(
        "shared",
        "POST",
        "/memories",
        Some(json!({"title": "Another fact here"})),
    );
    assert_eq!(status, 201, "{by}");
    let by = id_of(&by);

    let reads = [
        "".to_owned(),
        "/memories".to_owned(),
        format!("/memories/{id}"),
        "/events".to_owned(),
        "/export".to_owned(),
    ];
    let writes = [
        ("/init".to_owned(), json!({})),
        ("/memories".to_owned(), json!({"title": "Nope"})),
        (
            format!("/memories/{id}/revise"),
            json!({"reason": "r", "add_tags": ["shared"]}),
        ),
        (
            format!("/memories/{id}/confirm"),
            json!({"evidence": ["e"]}),
        ),
        (format!("/memories/{id}/dispute"), json!({"reason": "r"})),
        (format!("/memories/{id}/resolve"), json!({"reason": "r"})),
        (
            format!("/memories/{id}/supersede"),
            json!({"by": by, "reason": "r"}),
        ),
        (format!("/memories/{id}/forget"), json!({"reason": "r"})),
    ];
    for reader in [&memory_read, &db_read, &memory_rw, &db_rw] {
        for path in &reads {
            let (status, body) = reader.memory("shared", "GET", path, None);
            assert_eq!(status, 200, "{path}: {body}");
        }
    }
    for reader in [&memory_read, &db_read] {
        for (path, body) in &writes {
            let (status, answer) = reader.memory("shared", "POST", path, Some(body.clone()));
            assert_eq!(
                (status, code(&answer)),
                (403, "unauthorized"),
                "{path}: {answer}"
            );
        }
    }
    // The memory keys reach nothing else of the database.
    for key in [&memory_rw, &memory_read] {
        for (method, path, body) in [
            (
                "POST",
                "/databases/shared/query",
                Some(json!({"statements": [{"sql": "SELECT 1"}]})),
            ),
            ("GET", "/databases/shared/schema", None),
            (
                "GET",
                "/databases/shared/tables/memoire_memory_memories/rows",
                None,
            ),
            ("GET", "/databases/shared/export", None),
            ("GET", "/databases/shared", None),
            ("GET", "/databases", None),
        ] {
            let (status, answer) = key.call(method, path, body);
            assert_eq!(
                (status, code(&answer)),
                (403, "unauthorized"),
                "{path}: {answer}"
            );
        }
    }
    // A key that may write SQL may write the memory: every write route.
    for (path, body) in &writes[1..] {
        let (status, answer) = db_rw.memory("shared", "POST", path, Some(body.clone()));
        assert!([200, 201].contains(&status), "{path}: {status} {answer}");
    }

    // A key that may write but not read learns a similar title exists,
    // not what the store holds; one that may read sees the candidates.
    let writer = key(&gateway, &owner, &team, &["memoire.memory.write"]);
    let duplicate = json!({"title": "Another fact here"});
    assert_eq!(
        writer.memory("shared", "POST", "/memories", Some(duplicate.clone())),
        (
            409,
            json!({"error": {"code": "memory_duplicate", "classification": "conflict"},
                   "data": {"candidates": [], "scope": "global"}})
        )
    );
    let (status, body) = memory_rw.memory("shared", "POST", "/memories", Some(duplicate));
    assert_eq!((status, code(&body)), (409, "memory_duplicate"), "{body}");
    assert_eq!(body["data"]["candidates"][0]["id"], json!(by), "{body}");
    assert_eq!(body["data"]["candidates"].as_array().unwrap().len(), 1);

    // A query parameter a route does not know fails, on every route.
    for path in [
        "?scpoe=global".to_owned(),
        "/memories?scpoe=global".to_owned(),
        format!("/memories/{id}?scpoe=global"),
        "/events?scpoe=global".to_owned(),
        "/export?scpoe=global".to_owned(),
    ] {
        let (status, body) = memory_read.memory("shared", "GET", &path, None);
        assert_eq!(
            (status, details(&body)),
            (422, json!([{"path": "/scpoe", "code": "unknown_field"}])),
            "{path}: {body}"
        );
    }

    // Another team's key finds no such database.
    let stranger = register(&gateway, "stranger@memoire.test");
    let elsewhere = create_team(&gateway, &stranger, "stranger-team");
    let outsider = key(&gateway, &stranger, &elsewhere, MEMORY);
    for path in ["", "/memories"] {
        let (status, body) = outsider.memory("shared", "GET", path, None);
        assert_eq!((status, code(&body)), (404, "database_not_found"), "{body}");
    }
    let (status, body) = outsider.memory("shared", "POST", "/init", None);
    assert_eq!((status, code(&body)), (404, "database_not_found"), "{body}");

    // No credential at all.
    let (status, body) = gateway.request("GET", "/v1/memoire/databases/shared/memory", &[], None);
    assert_eq!((status, code(&body)), (401, "unauthenticated"), "{body}");
}
