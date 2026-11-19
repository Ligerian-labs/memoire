//! The catalog's rules through its `handle` export, as the host calls it:
//! protocol 2 requests in, responses out.
use memoire_catalog::handle;
use serde_json::{Value, json};

const TEAM: &str = "org_0192aaaa";
const DAY_MS: u64 = 24 * 60 * 60 * 1000;
const T0: u64 = 1_795_000_000_000;

fn stream() -> String {
    format!("{TEAM}:Catalog/{TEAM}")
}

fn call(request: Value) -> Value {
    serde_json::from_str(&handle(&request.to_string()).expect("the ABI call succeeds")).unwrap()
}

fn member() -> Value {
    json!({"subject": "usr_0192bbbb", "kind": "user", "tenant": TEAM, "roles": ["member"]})
}

fn system() -> Value {
    json!({"subject": "sys_memoire_sweeper", "kind": "system", "tenant": TEAM})
}

fn metadata(principal: &Value) -> Value {
    json!({"message_id": "m-1", "correlation_id": "c-1", "actor": principal["subject"], "principal": principal})
}

fn id(n: u64) -> String {
    format!("db_{n:032x}")
}

/// A history built from (tag, payload, accepted_unix_ms) triples.
#[derive(Default)]
struct Stream(Vec<Value>);

impl Stream {
    fn push(&mut self, tag: &str, payload: Value, at: u64) -> &mut Self {
        let n = self.0.len() as u64 + 1;
        self.0.push(json!({
            "event_id": format!("event/{n}"), "stream": stream(), "version": n, "position": n,
            "event": {"tag": tag, "schema_version": 1, "payload": payload},
            "metadata": {"message_id": format!("m-{n}"), "correlation_id": "c"},
            "accepted_unix_ms": at,
        }));
        self
    }

    fn created(&mut self, n: u64, name: &str, kind: &str, ttl: Option<u64>, at: u64) -> &mut Self {
        self.push(
            "DatabaseCreated",
            json!({"id": id(n), "name": name, "kind": kind, "idle_ttl_seconds": ttl}),
            at,
        )
    }

    fn expired(&mut self, n: u64, name: &str, at: u64) -> &mut Self {
        self.push(
            "DatabaseExpired",
            json!({"id": id(n), "name": name, "reason": "idle"}),
            at,
        )
    }

    /// Decide `tag` with `payload` for `principal`: `Ok(events)` or the
    /// error code.
    fn decide(&self, principal: &Value, tag: &str, payload: Value) -> Result<Vec<Value>, String> {
        decide_on(&stream(), &self.0, principal, tag, payload)
    }

    /// The projection over this history, then `query` on it.
    fn query(&self, tag: &str, payload: Value) -> Result<Value, String> {
        let projected = call(json!({"version": 2, "body": {
            "op": "project", "projection": "databases", "state": {"databases": {}},
            "events": self.0, "live": true, "partition": TEAM,
        }}));
        assert_eq!(projected["outcome"]["status"], "ok", "{projected}");
        let state = projected["outcome"]["reply"]["state"].clone();
        let answer = call(json!({"version": 2, "body": {
            "op": "query", "tag": tag, "payload": payload, "state": state,
            "metadata": metadata(&member()),
        }}));
        outcome(&answer).map(|reply| reply["value"].clone())
    }
}

fn decide_on(
    stream: &str,
    history: &[Value],
    principal: &Value,
    tag: &str,
    payload: Value,
) -> Result<Vec<Value>, String> {
    let response = call(json!({"version": 2, "body": {
        "op": "decide", "tag": tag, "stream": stream, "payload": payload,
        "history": history, "metadata": metadata(principal),
    }}));
    outcome(&response).map(|reply| reply["events"].as_array().unwrap().clone())
}

fn outcome(response: &Value) -> Result<Value, String> {
    assert_eq!(response["version"], 2, "{response}");
    match response["outcome"]["status"].as_str() {
        Some("ok") => Ok(response["outcome"]["reply"].clone()),
        _ => Err(response["outcome"]["error"]["code"]
            .as_str()
            .unwrap()
            .to_owned()),
    }
}

fn validate(tag: &str, payload: Value) -> Result<Value, String> {
    let response = call(json!({"version": 2, "body": {
        "op": "validate", "kind": "command", "tag": tag, "payload": payload,
    }}));
    outcome(&response).map(|reply| reply["payload"].clone())
}

fn code_of(result: Result<Vec<Value>, String>) -> String {
    result.expect_err("refused")
}

#[test]
fn describe_reports_protocol_2() {
    let response = call(json!({"version": 2, "body": {"op": "describe"}}));
    assert_eq!(response["outcome"]["reply"]["protocol"], 2);
}

#[test]
fn validate_canonicalizes_and_refuses_bad_payloads() {
    assert_eq!(
        validate(
            "CreateDatabase",
            json!({"id": id(1), "name": "scratch", "kind": "burstable"})
        )
        .unwrap(),
        json!({"id": id(1), "name": "scratch", "kind": "burstable", "idle_ttl_seconds": 86400})
    );
    assert_eq!(
        validate(
            "CreateDatabase",
            json!({"id": id(1), "name": "notes", "kind": "persistent"})
        )
        .unwrap(),
        json!({"id": id(1), "name": "notes", "kind": "persistent"})
    );
    for bad in [
        json!({"id": id(1), "name": "notes", "kind": "persistent", "idle_ttl_seconds": 60}),
        json!({"id": id(1), "name": "Bad", "kind": "burstable"}),
        json!({"id": id(1), "name": "a", "kind": "burstable"}),
        json!({"id": "db_xyz", "name": "ok", "kind": "burstable"}),
        json!({"id": id(1), "name": "ok", "kind": "burstable", "idle_ttl_seconds": 0}),
        json!({"id": id(1), "name": "ok", "kind": "burstable", "idle_ttl_seconds": 604_801}),
        json!({"id": id(1), "name": "ok", "kind": "forever"}),
        json!({"id": id(1), "name": "ok", "kind": "burstable", "extra": 1}),
    ] {
        assert_eq!(
            validate("CreateDatabase", bad.clone()).unwrap_err(),
            "validation_failed",
            "{bad}"
        );
    }
    assert!(validate("ExtendDatabase", json!({"name": "ok"})).is_ok());
    assert_eq!(
        validate(
            "ExtendDatabase",
            json!({"name": "ok", "idle_ttl_seconds": 0})
        )
        .unwrap_err(),
        "validation_failed"
    );
    assert!(validate("DeleteDatabase", json!({"name": "ok"})).is_ok());
    assert!(
        validate(
            "ExpireDatabase",
            json!({"name": "ok", "id": id(1), "now_unix_ms": T0})
        )
        .is_ok()
    );
    assert_eq!(
        validate("DropEverything", json!({})).unwrap_err(),
        "unknown_command"
    );
}

#[test]
fn create_records_a_new_entry_on_the_team_stream_only() {
    let empty = Stream::default();
    let events = empty
        .decide(
            &member(),
            "CreateDatabase",
            json!({"id": id(1), "name": "scratch", "kind": "burstable", "idle_ttl_seconds": 3600}),
        )
        .unwrap();
    assert_eq!(
        events,
        vec![
            json!({"tag": "DatabaseCreated", "schema_version": 1, "payload":
            {"id": id(1), "name": "scratch", "kind": "burstable", "idle_ttl_seconds": 3600}})
        ]
    );

    // Another stream of the same team is not a catalog.
    let other = format!("{TEAM}:Catalog/second");
    let payload = json!({"id": id(1), "name": "scratch", "kind": "persistent"});
    assert_eq!(
        decide_on(&other, &[], &member(), "CreateDatabase", payload.clone()).unwrap_err(),
        "unauthorized"
    );
    let no_team = json!({"subject": "usr_0192bbbb", "kind": "user"});
    assert_eq!(
        decide_on(&stream(), &[], &no_team, "CreateDatabase", payload).unwrap_err(),
        "tenant_required"
    );
}

#[test]
fn names_are_unique_among_active_databases_and_ids_never_return() {
    let mut s = Stream::default();
    s.created(1, "scratch", "burstable", Some(60), T0);
    let again = |s: &Stream, n| {
        s.decide(
            &member(),
            "CreateDatabase",
            json!({"id": id(n), "name": "scratch", "kind": "persistent"}),
        )
    };
    assert_eq!(code_of(again(&s, 2)), "database_exists");
    let response = call(json!({"version": 2, "body": {
        "op": "decide", "tag": "CreateDatabase", "stream": stream(),
        "payload": {"id": id(2), "name": "scratch", "kind": "persistent"},
        "history": s.0, "metadata": metadata(&member()),
    }}));
    assert_eq!(response["outcome"]["error"]["classification"], "conflict");

    // An expired entry gives its name to a new database with a new id.
    s.expired(1, "scratch", T0 + 61_000);
    assert_eq!(again(&s, 2).unwrap()[0]["tag"], "DatabaseCreated");
    // The old id is never reused, even after a delete.
    s.push(
        "DatabaseDeleted",
        json!({"id": id(1), "name": "scratch"}),
        T0 + 62_000,
    );
    assert_eq!(code_of(again(&s, 1)), "database_exists");
    assert!(again(&s, 3).is_ok());
}

#[test]
fn the_quota_counts_active_databases_only() {
    let mut s = Stream::default();
    for n in 0..50 {
        s.created(n, &format!("db-{n}"), "persistent", None, T0 + n);
    }
    let create = |s: &Stream, n: u64, name: &str| {
        s.decide(
            &member(),
            "CreateDatabase",
            json!({"id": id(n), "name": name, "kind": "burstable"}),
        )
    };
    assert_eq!(code_of(create(&s, 100, "one-more")), "database_limit");
    let response = call(json!({"version": 2, "body": {
        "op": "decide", "tag": "CreateDatabase", "stream": stream(),
        "payload": {"id": id(100), "name": "one-more", "kind": "burstable"},
        "history": s.0, "metadata": metadata(&member()),
    }}));
    assert_eq!(response["outcome"]["error"]["classification"], "permanent");
    // The name check comes first: a held name is database_exists.
    assert_eq!(code_of(create(&s, 100, "db-3")), "database_exists");

    s.push(
        "DatabaseDeleted",
        json!({"id": id(7), "name": "db-7"}),
        T0 + 100,
    );
    assert!(create(&s, 100, "one-more").is_ok());
}

#[test]
fn extend_restarts_the_clock_of_active_burstable_databases() {
    let mut s = Stream::default();
    s.created(1, "scratch", "burstable", Some(3600), T0)
        .created(2, "notes", "persistent", None, T0)
        .created(3, "old", "burstable", Some(60), T0)
        .expired(3, "old", T0 + 60_000);
    let extend = |name: &str, ttl: Option<u64>| {
        let mut payload = json!({"name": name});
        if let Some(ttl) = ttl {
            payload["idle_ttl_seconds"] = json!(ttl);
        }
        s.decide(&member(), "ExtendDatabase", payload)
    };
    assert_eq!(
        extend("scratch", None).unwrap()[0]["payload"],
        json!({"id": id(1), "name": "scratch", "idle_ttl_seconds": 3600})
    );
    assert_eq!(
        extend("scratch", Some(7200)).unwrap()[0]["payload"]["idle_ttl_seconds"],
        7200
    );
    assert_eq!(code_of(extend("notes", None)), "not_burstable");
    assert_eq!(code_of(extend("old", None)), "database_expired");
    assert_eq!(code_of(extend("missing", None)), "database_not_found");
}

#[test]
fn delete_removes_active_and_expired_entries() {
    let mut s = Stream::default();
    s.created(1, "scratch", "burstable", Some(60), T0)
        .created(2, "old", "burstable", Some(60), T0)
        .expired(2, "old", T0 + 60_000);
    for (name, n) in [("scratch", 1), ("old", 2)] {
        assert_eq!(
            s.decide(&member(), "DeleteDatabase", json!({"name": name}))
                .unwrap()[0],
            json!({"tag": "DatabaseDeleted", "schema_version": 1,
                   "payload": {"id": id(n), "name": name}})
        );
    }
    assert_eq!(
        code_of(s.decide(&member(), "DeleteDatabase", json!({"name": "gone"}))),
        "database_not_found"
    );
    // Pinned by id: another entry of the name is kept.
    let pinned = json!({"name": "scratch", "id": id(9)});
    assert_eq!(
        code_of(s.decide(&member(), "DeleteDatabase", pinned)),
        "database_not_found"
    );
    let pinned = json!({"name": "scratch", "id": id(1)});
    assert!(s.decide(&member(), "DeleteDatabase", pinned).is_ok());
}

#[test]
fn expire_follows_the_idle_ttl_the_last_use_and_the_lifetime() {
    let mut s = Stream::default();
    s.created(1, "scratch", "burstable", Some(3600), T0)
        .created(2, "notes", "persistent", None, T0);
    let expire = |s: &Stream, principal: &Value, now: u64, last_used: Option<u64>| {
        let mut payload = json!({"name": "scratch", "id": id(1), "now_unix_ms": now});
        if let Some(at) = last_used {
            payload["last_used_unix_ms"] = json!(at);
        }
        s.decide(principal, "ExpireDatabase", payload)
    };
    let idle = T0 + 3_600_000;

    assert_eq!(code_of(expire(&s, &member(), idle, None)), "unauthorized");
    assert_eq!(
        code_of(expire(&s, &system(), idle - 1, None)),
        "not_expired"
    );
    let events = expire(&s, &system(), idle, None).unwrap();
    assert_eq!(
        events[0]["payload"],
        json!({"id": id(1), "name": "scratch", "reason": "idle"})
    );
    // A recent query (the data plane's last use) keeps it alive.
    assert_eq!(
        code_of(expire(&s, &system(), idle, Some(T0 + 1_000))),
        "not_expired"
    );
    assert!(expire(&s, &system(), idle + 1_000, Some(T0 + 1_000)).is_ok());
    // So does an extension.
    let mut extended = Stream(s.0.clone());
    extended.push(
        "DatabaseExtended",
        json!({"id": id(1), "name": "scratch", "idle_ttl_seconds": 3600}),
        T0 + 10_000,
    );
    assert_eq!(
        code_of(expire(&extended, &system(), idle, None)),
        "not_expired"
    );

    // Use never pushes past the 7-day lifetime.
    let lifetime = T0 + 7 * DAY_MS;
    assert_eq!(
        expire(&s, &system(), lifetime, Some(lifetime - 1)).unwrap()[0]["payload"]["reason"],
        "lifetime"
    );

    // Pinned by id, idempotent, and only for burstable databases.
    let wrong = s.decide(
        &system(),
        "ExpireDatabase",
        json!({"name": "scratch", "id": id(9), "now_unix_ms": idle}),
    );
    assert_eq!(code_of(wrong), "database_not_found");
    let persistent = s.decide(
        &system(),
        "ExpireDatabase",
        json!({"name": "notes", "id": id(2), "now_unix_ms": lifetime}),
    );
    assert_eq!(code_of(persistent), "not_burstable");
    s.expired(1, "scratch", idle);
    assert_eq!(
        expire(&s, &system(), idle + 5, None).unwrap(),
        Vec::<Value>::new()
    );
}

#[test]
fn the_projection_lists_entries_and_keeps_fifty_expired() {
    let mut s = Stream::default();
    for n in 0..52 {
        s.created(n, &format!("tmp-{n:02}"), "burstable", Some(60), T0);
    }
    for n in 0..52 {
        s.expired(n, &format!("tmp-{n:02}"), T0 + 60_000 + n);
    }
    s.created(100, "notes", "persistent", None, T0 + 5);
    s.push(
        "DatabaseExtended",
        json!({"id": id(51), "name": "tmp-51", "idle_ttl_seconds": 60}),
        T0 + 1,
    );

    let list = s.query("ListDatabases", json!({})).unwrap();
    let names: Vec<&str> = list["databases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 51, "50 expired and 1 active");
    assert_eq!(names[0], "notes");
    assert!(!names.contains(&"tmp-00") && !names.contains(&"tmp-01"));
    assert!(names.contains(&"tmp-02"));

    assert_eq!(
        s.query("GetDatabase", json!({"name": "notes"})).unwrap(),
        json!({"id": id(100), "name": "notes", "kind": "persistent", "state": "active",
               "created_unix_ms": T0 + 5, "idle_ttl_seconds": null,
               "extended_unix_ms": null, "expired_unix_ms": null})
    );
    let tmp = s.query("GetDatabase", json!({"name": "tmp-51"})).unwrap();
    assert_eq!(tmp["state"], "expired");
    assert_eq!(tmp["extended_unix_ms"], T0 + 1);
    assert_eq!(
        s.query("GetDatabase", json!({"name": "tmp-00"}))
            .unwrap_err(),
        "database_not_found"
    );
    assert_eq!(
        s.query("ListDatabases", json!({"x": 1})).unwrap_err(),
        "validation_failed"
    );
}

#[test]
fn foreign_history_is_refused() {
    let mut s = Stream::default();
    s.push("SomethingElse", json!({}), T0);
    assert_eq!(
        code_of(s.decide(&member(), "DeleteDatabase", json!({"name": "ok"}))),
        "unknown_event"
    );
    let mut s = Stream::default();
    s.push("DatabaseDeleted", json!({"id": id(1), "name": "never"}), T0);
    assert_eq!(
        code_of(s.decide(&member(), "DeleteDatabase", json!({"name": "ok"}))),
        "invalid_history"
    );
}
