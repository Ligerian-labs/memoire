//! The catalog through the gateway: `GET|POST /v1/memoire/databases`,
//! `GET|DELETE …/{name}` and `POST …/{name}/extend`, as a member, through
//! team API keys and across teams, on a real host and PostgreSQL.
mod common;

use common::*;
use serde_json::{Value, json};

/// A member's requests in one team.
struct Client<'a> {
    gateway: &'a Gateway,
    auth: String,
    team: Option<String>,
}

impl Client<'_> {
    fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut headers = vec![("authorization", self.auth.as_str())];
        if let Some(team) = &self.team {
            headers.push(("merezha-organization", team.as_str()));
        }
        self.gateway.request(
            method,
            &format!("/v1/memoire{path}"),
            &headers,
            body.as_ref(),
        )
    }

    fn create(&self, body: Value) -> (u16, Value) {
        self.call("POST", "/databases", Some(body))
    }
}

fn code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or("")
}

fn names(page: &Value) -> Vec<&str> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect()
}

#[test]
fn the_catalog_serves_the_database_routes() {
    let Some(db) = database() else { return };
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), r#"data_dir = "data""#);
    let gateway = Gateway::start(&config, &db);
    let owner = register(&gateway, "owner@memoire.test");
    let team = create_team(&gateway, &owner, "catalog-team");
    let me = Client {
        gateway: &gateway,
        auth: owner.bearer(),
        team: Some(team.clone()),
    };

    // No team: every database route needs one.
    let teamless = Client {
        gateway: &gateway,
        auth: owner.bearer(),
        team: None,
    };
    let (status, body) = teamless.call("GET", "/databases", None);
    assert_eq!((status, code(&body)), (403, "tenant_required"), "{body}");
    let (status, body) = teamless.create(json!({"name": "scratch", "kind": "burstable"}));
    assert_eq!((status, code(&body)), (403, "tenant_required"), "{body}");
    let (status, _) = gateway.get("/v1/memoire/databases", None);
    assert_eq!(status, 401);

    // Create a burstable database.
    let (status, head, raw) = gateway.raw(
        "POST",
        "/v1/memoire/databases",
        &[
            ("authorization", me.auth.as_str()),
            ("merezha-organization", team.as_str()),
        ],
        Some((
            "application/json",
            br#"{"name":"scratch","kind":"burstable","idle_ttl_seconds":3600}"#,
        )),
    );
    let scratch: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(status, 201, "{scratch}");
    assert!(
        head.to_ascii_lowercase()
            .contains("cache-control: no-store"),
        "{head}"
    );
    let id = scratch["id"].as_str().unwrap();
    assert!(id.starts_with("db_") && id.len() == 35, "{id}");
    assert_eq!(scratch["name"], "scratch");
    assert_eq!(scratch["kind"], "burstable");
    assert_eq!(scratch["state"], "active");
    assert_eq!(scratch["idle_ttl_seconds"], 3600);
    assert_eq!(scratch["last_used_at"], Value::Null);
    let created: jiff::Timestamp = scratch["created_at"].as_str().unwrap().parse().unwrap();
    let at = |offset: i64| (created + jiff::SignedDuration::from_secs(offset)).to_string();
    assert_eq!(scratch["expires_at"], at(3600));
    assert_eq!(scratch["expires_at_max"], at(7 * 24 * 3600));

    let (status, body) = me.create(json!({"name": "scratch", "kind": "persistent"}));
    assert_eq!((status, code(&body)), (409, "database_exists"), "{body}");
    assert_eq!(body["error"]["classification"], "conflict");

    // Validation reports every field, never the input.
    let (status, body) = me.create(json!({"name": "Bad Name", "kind": "forever", "ttl": 1}));
    assert_eq!(status, 422, "{body}");
    assert_eq!(code(&body), "validation_failed");
    let mut details: Vec<(String, String)> = body["error"]["details"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["path"].as_str().unwrap().into(),
                d["code"].as_str().unwrap().into(),
            )
        })
        .collect();
    details.sort();
    assert_eq!(
        details,
        [
            ("/kind", "invalid"),
            ("/name", "invalid"),
            ("/ttl", "unknown_field")
        ]
        .map(|(p, c)| (p.to_owned(), c.to_owned()))
    );
    let (status, body) =
        me.create(json!({"name": "short-ttl", "kind": "burstable", "idle_ttl_seconds": 59}));
    assert_eq!(status, 422);
    assert_eq!(
        body["error"]["details"][0],
        json!({"path": "/idle_ttl_seconds", "code": "out_of_range"})
    );
    let (_, body) =
        me.create(json!({"name": "notes", "kind": "persistent", "idle_ttl_seconds": 60}));
    assert_eq!(body["error"]["details"][0]["path"], "/idle_ttl_seconds");
    let (_, body) = me.create(json!({"kind": "persistent"}));
    assert_eq!(
        body["error"]["details"][0],
        json!({"path": "/name", "code": "required"})
    );
    let (status, _, _) = gateway.raw(
        "POST",
        "/v1/memoire/databases",
        &[
            ("authorization", me.auth.as_str()),
            ("merezha-organization", team.as_str()),
        ],
        Some(("text/plain", b"{}")),
    );
    assert_eq!(status, 415);

    // A persistent database, then the list, sorted by name and paged.
    let (status, notes) =
        me.create(json!({"name": "notes", "kind": "persistent", "idle_ttl_seconds": null}));
    assert_eq!(status, 201, "{notes}");
    assert_eq!(
        (
            &notes["idle_ttl_seconds"],
            &notes["expires_at"],
            &notes["expires_at_max"]
        ),
        (&Value::Null, &Value::Null, &Value::Null)
    );
    let (status, page) = me.call("GET", "/databases", None);
    assert_eq!(status, 200, "{page}");
    assert_eq!(names(&page), ["notes", "scratch"]);
    assert_eq!(page["next_cursor"], Value::Null);
    let (_, first) = me.call("GET", "/databases?limit=1", None);
    assert_eq!(names(&first), ["notes"]);
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    let (_, second) = me.call("GET", &format!("/databases?limit=1&cursor={cursor}"), None);
    assert_eq!(names(&second), ["scratch"]);
    assert_eq!(second["next_cursor"], Value::Null);
    let (status, body) = me.call(
        "GET",
        &format!("/databases?state=all&cursor={cursor}"),
        None,
    );
    assert_eq!((status, code(&body)), (422, "invalid_page"));
    let (status, body) = me.call("GET", "/databases?limit=500", None);
    assert_eq!((status, code(&body)), (422, "validation_failed"));

    // Detail.
    let (status, body) = me.call("GET", "/databases/scratch", None);
    assert_eq!(status, 200);
    assert_eq!(body, scratch);
    for missing in ["/databases/nothing-here", "/databases/Bad%20Name"] {
        let (status, body) = me.call("GET", missing, None);
        assert_eq!(
            (status, code(&body)),
            (404, "database_not_found"),
            "{missing}"
        );
    }

    // Extend restarts the clock; with a TTL it changes it too.
    let (status, extended) = me.call("POST", "/databases/scratch/extend", Some(json!({})));
    assert_eq!(status, 200, "{extended}");
    assert_ne!(extended["last_used_at"], Value::Null);
    assert_eq!(extended["idle_ttl_seconds"], 3600);
    let (status, extended) = me.call(
        "POST",
        "/databases/scratch/extend",
        Some(json!({"idle_ttl_seconds": 604800})),
    );
    assert_eq!(status, 200, "{extended}");
    assert_eq!(extended["idle_ttl_seconds"], 604800);
    assert_eq!(
        extended["expires_at"], extended["expires_at_max"],
        "capped at the lifetime"
    );
    let (status, _, _) = gateway.raw(
        "POST",
        "/v1/memoire/databases/scratch/extend",
        &[
            ("authorization", me.auth.as_str()),
            ("merezha-organization", team.as_str()),
        ],
        None,
    );
    assert_eq!(status, 200, "an empty body is {{}}");
    let (status, body) = me.call("POST", "/databases/notes/extend", Some(json!({})));
    assert_eq!((status, code(&body)), (409, "not_burstable"));
    let (status, body) = me.call(
        "POST",
        "/databases/scratch/extend",
        Some(json!({"idle_ttl_seconds": 10})),
    );
    assert_eq!((status, code(&body)), (422, "validation_failed"));
    let (status, body) = me.call("POST", "/databases/nothing-here/extend", Some(json!({})));
    assert_eq!((status, code(&body)), (404, "database_not_found"));

    // Another team never sees this one's databases.
    let other = register(&gateway, "other@memoire.test");
    let other_team = create_team(&gateway, &other, "other-team");
    let them = Client {
        gateway: &gateway,
        auth: other.bearer(),
        team: Some(other_team.clone()),
    };
    let (_, page) = them.call("GET", "/databases", None);
    assert_eq!(names(&page), Vec::<&str>::new());
    let (status, body) = them.call("GET", "/databases/notes", None);
    assert_eq!((status, code(&body)), (404, "database_not_found"));
    let (status, body) = them.call("DELETE", "/databases/notes", None);
    assert_eq!((status, code(&body)), (404, "database_not_found"));
    let (status, _) = them.create(json!({"name": "notes", "kind": "persistent"}));
    assert_eq!(status, 201, "names are per team");
    // Naming a team one does not belong to is refused by identity.
    let (status, body) = Client {
        gateway: &gateway,
        auth: other.bearer(),
        team: Some(team.clone()),
    }
    .call("GET", "/databases", None);
    let (_, whoami) = Client {
        gateway: &gateway,
        auth: other.bearer(),
        team: Some(team.clone()),
    }
    .call("GET", "/whoami", None);
    assert_eq!(
        status, 404,
        "{body} {whoami} team={team} other={other_team}"
    );

    // Delete, then the name is free again with a new id.
    let (status, body) = me.call("DELETE", "/databases/scratch", None);
    assert_eq!(status, 204, "{body}");
    let (status, _) = me.call("GET", "/databases/scratch", None);
    assert_eq!(status, 404);
    let (status, body) = me.call("DELETE", "/databases/scratch", None);
    assert_eq!((status, code(&body)), (404, "database_not_found"));
    let (status, again) = me.create(json!({"name": "scratch", "kind": "burstable"}));
    assert_eq!(status, 201);
    assert_ne!(again["id"], scratch["id"]);
    assert_eq!(again["idle_ttl_seconds"], 86400, "the default TTL");

    // Expiry is the sweeper's: a member calling the command directly is
    // refused by the guest, and a second catalog stream cannot be opened.
    let (status, body) = gateway.request(
        "POST",
        "/v1/application/commands",
        &[("authorization", me.auth.as_str()), ("merezha-organization", team.as_str())],
        Some(&json!({"tag": "ExpireDatabase", "stream": team,
                     "payload": {"name": "scratch", "id": again["id"], "now_unix_ms": 4_000_000_000_000u64}})),
    );
    assert_eq!((status, code(&body)), (403, "unauthorized"), "{body}");
    let (status, body) = gateway.request(
        "POST",
        "/v1/application/commands",
        &[("authorization", me.auth.as_str()), ("merezha-organization", team.as_str())],
        Some(&json!({"tag": "CreateDatabase", "stream": "second",
                     "payload": {"id": format!("db_{:032x}", 7), "name": "sneaky", "kind": "persistent"}})),
    );
    assert_eq!((status, code(&body)), (403, "unauthorized"), "{body}");

    // The catalog is durable: a restarted gateway serves the same entries.
    assert!(gateway.stop().success());
    let gateway = Gateway::start(&config, &db);
    let me = Client {
        gateway: &gateway,
        auth: owner.bearer(),
        team: Some(team.clone()),
    };
    let (_, page) = me.call("GET", "/databases", None);
    assert_eq!(names(&page), ["notes", "scratch"]);
}

/// The 50 active databases quota, and concurrent creates in one team: the
/// stream moves under them and the gateway decides again.
#[test]
fn the_quota_holds_under_concurrent_creates() {
    let Some(db) = database() else { return };
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::start(&config(dir.path(), r#"data_dir = "data""#), &db);
    let owner = register(&gateway, "quota@memoire.test");
    let team = create_team(&gateway, &owner, "quota-team");
    let auth = owner.bearer();

    let results: Vec<(u16, Value)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..5)
            .map(|worker| {
                let (gateway, auth, team) = (&gateway, auth.as_str(), team.as_str());
                scope.spawn(move || {
                    (0..11)
                        .map(|n| {
                            gateway.request(
                                "POST",
                                "/v1/memoire/databases",
                                &[("authorization", auth), ("merezha-organization", team)],
                                Some(&json!({"name": format!("db-{worker}-{n}"), "kind": "persistent"})),
                            )
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap())
            .collect()
    });
    let created = results.iter().filter(|(s, _)| *s == 201).count();
    let limited: Vec<_> = results.iter().filter(|(s, _)| *s != 201).collect();
    assert_eq!(created, 50, "{limited:?}");
    assert_eq!(limited.len(), 5);
    for (status, body) in limited {
        assert_eq!((*status, code(body)), (422, "database_limit"), "{body}");
    }
    let me = Client {
        gateway: &gateway,
        auth: auth.clone(),
        team: Some(team.clone()),
    };
    let (_, page) = me.call("GET", "/databases?limit=100", None);
    assert_eq!(page["items"].as_array().unwrap().len(), 50);

    // Deleting one frees a slot.
    let victim = names(&page)[0].to_owned();
    assert_eq!(
        me.call("DELETE", &format!("/databases/{victim}"), None).0,
        204
    );
    assert_eq!(
        me.create(json!({"name": "one-more", "kind": "persistent"}))
            .0,
        201
    );
    let (status, body) = me.create(json!({"name": "too-many", "kind": "persistent"}));
    assert_eq!((status, code(&body)), (422, "database_limit"));
}

/// The five permissions: the team roles hold them, API keys carry a
/// subset, and a machine key acts through its service account's role.
#[test]
fn permissions_follow_roles_and_keys() {
    let Some(db) = database() else { return };
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::start(&config(dir.path(), r#"data_dir = "data""#), &db);
    let owner = register(&gateway, "keys@memoire.test");
    let team = create_team(&gateway, &owner, "keys-team");
    let owner_auth = owner.bearer();
    let as_owner = [("authorization", owner_auth.as_str())];

    // Identity's catalog lists memoire's permissions.
    let (status, catalog) = gateway.request("GET", "/v1/identity/permissions", &as_owner, None);
    assert_eq!(status, 200, "{catalog}");
    for permission in MEMOIRE_PERMISSIONS {
        assert!(
            catalog.to_string().contains(permission),
            "{permission}: {catalog}"
        );
    }

    // The owner's read-only key reads and nothing else.
    let (status, body) = api_key(&gateway, &owner, &team, &["memoire.databases.read"], None);
    assert_eq!(status, 201, "{body}");
    let reader = Client {
        gateway: &gateway,
        auth: format!("Bearer {}", body["key"].as_str().unwrap()),
        team: None,
    };
    let (status, page) = reader.call("GET", "/databases", None);
    assert_eq!(status, 200, "{page}");
    let (status, body) = reader.create(json!({"name": "nope", "kind": "persistent"}));
    assert_eq!((status, code(&body)), (403, "unauthorized"), "{body}");

    // A full key creates, reads, extends and deletes.
    let (_, body) = api_key(&gateway, &owner, &team, MEMOIRE_PERMISSIONS, None);
    let full = Client {
        gateway: &gateway,
        auth: format!("Bearer {}", body["key"].as_str().unwrap()),
        team: None,
    };
    assert_eq!(
        full.create(json!({"name": "tmp", "kind": "burstable"})).0,
        201
    );
    assert_eq!(full.call("POST", "/databases/tmp/extend", None).0, 200);
    let (status, body) = reader.call("POST", "/databases/tmp/extend", None);
    assert_eq!((status, code(&body)), (403, "unauthorized"), "{body}");
    let (status, body) = reader.call("DELETE", "/databases/tmp", None);
    assert_eq!((status, code(&body)), (403, "unauthorized"), "{body}");
    assert_eq!(full.call("DELETE", "/databases/tmp", None).0, 204);

    // A key holding only a change's permission gets its answer too.
    let only = |permission: &str| {
        let (status, body) = api_key(&gateway, &owner, &team, &[permission], None);
        assert_eq!(status, 201, "{body}");
        Client {
            gateway: &gateway,
            auth: format!("Bearer {}", body["key"].as_str().unwrap()),
            team: None,
        }
    };
    let (status, body) =
        only("memoire.databases.create").create(json!({"name": "narrow", "kind": "burstable"}));
    assert_eq!((status, &body["name"]), (201, &json!("narrow")), "{body}");
    let (status, body) =
        only("memoire.databases.write").call("POST", "/databases/narrow/extend", None);
    assert_eq!((status, &body["name"]), (200, &json!("narrow")), "{body}");
    let (status, body) = only("memoire.databases.delete").call("DELETE", "/databases/narrow", None);
    assert_eq!(status, 204, "{body}");

    // A member invited with the `member` role holds all five.
    let (status, roles) = gateway.request(
        "GET",
        &format!("/v1/identity/organizations/{team}/roles"),
        &as_owner,
        None,
    );
    assert_eq!(status, 200, "{roles}");
    let role = |key: &str| {
        roles["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == key)
            .unwrap_or_else(|| panic!("no role {key}: {roles}"))["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let (status, body) = gateway.request(
        "POST",
        &format!("/v1/identity/organizations/{team}/invitations"),
        &as_owner,
        Some(&json!({"email": "invited@memoire.test", "role_ids": [role("member")]})),
    );
    assert_eq!(status, 201, "{body}");
    let token = mailed_token(&gateway, "invited@memoire.test");
    let (status, body) = gateway.request(
        "POST",
        "/v1/identity/invitations/accept",
        &[("origin", ORIGIN)],
        Some(&json!({"token": token, "transport": "bearer"})),
    );
    assert_eq!(status, 201, "{body}");
    let invited = Client {
        gateway: &gateway,
        auth: format!("Bearer {}", body["session"]["token"].as_str().unwrap()),
        team: Some(team.clone()),
    };
    let (status, body) = invited.create(json!({"name": "from-member", "kind": "persistent"}));
    assert_eq!(status, 201, "{body}");
    assert_eq!(
        invited.call("DELETE", "/databases/from-member", None).0,
        204
    );

    // A machine's key: the service account acts through the role it holds.
    let (status, body) = gateway.request(
        "POST",
        &format!("/v1/identity/organizations/{team}/service-accounts"),
        &as_owner,
        Some(&json!({"name": "ci-runner"})),
    );
    assert_eq!(status, 201, "{body}");
    let machine = body["service_account"]["id"].as_str().unwrap().to_owned();
    let (status, body) = api_key(&gateway, &owner, &team, MEMOIRE_PERMISSIONS, Some(&machine));
    assert_eq!(status, 201, "{body}");
    let agent = Client {
        gateway: &gateway,
        auth: format!("Bearer {}", body["key"].as_str().unwrap()),
        team: None,
    };
    let (without_role, body) = agent.call("GET", "/databases", None);
    eprintln!("machine key without a role assignment: {without_role} {body}");
    let (status, body) = gateway.request(
        "POST",
        &format!("/v1/identity/organizations/{team}/role-assignments"),
        &as_owner,
        Some(&json!({"service_account_id": machine, "role_id": role("member")})),
    );
    assert_eq!(status, 201, "{body}");
    let (status, body) = agent.call("GET", "/databases", None);
    assert_eq!(status, 200, "{body}");
    let (status, body) = agent.create(json!({"name": "from-machine", "kind": "burstable"}));
    assert_eq!(status, 201, "{body}");
    assert_eq!(without_role, 403, "a machine without a role holds nothing");
}

/// The web's credential: identity's session cookie, with the team set on
/// the session and the Origin and CSRF checks on changes.
#[test]
fn cookie_sessions_pass_origin_and_csrf_checks() {
    let Some(db) = database() else { return };
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::start(&config(dir.path(), r#"data_dir = "data""#), &db);
    let owner = register(&gateway, "web@memoire.test");
    let team = create_team(&gateway, &owner, "web-team");

    let body = json!({"email": "web@memoire.test", "password": PASSWORD});
    let (status, head, raw) = gateway.raw(
        "POST",
        "/v1/identity/sessions",
        &[("origin", ORIGIN)],
        Some(("application/json", body.to_string().as_bytes())),
    );
    let signed_in: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(status, 201, "{signed_in}");
    let csrf = signed_in["csrf_token"].as_str().unwrap().to_owned();
    let cookie = cookies(&head);
    assert!(cookie.contains("merezha_session="), "{head}");
    fn with<'a>(cookie: &'a str, extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
        let mut headers = vec![("cookie", cookie)];
        headers.extend_from_slice(extra);
        headers
    }
    let as_web = |extra: &[(&'static str, &str)]| -> Vec<(String, String)> {
        with(&cookie, extra)
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect()
    };

    // No team on the session yet, then one.
    let (status, body) = gateway.request("GET", "/v1/memoire/databases", &refs(&as_web(&[])), None);
    assert_eq!((status, code(&body)), (403, "tenant_required"), "{body}");
    let (status, body) = gateway.request(
        "PUT",
        "/v1/identity/session/organization",
        &refs(&as_web(&[
            ("origin", ORIGIN),
            ("x-csrf-token", csrf.as_str()),
        ])),
        Some(&json!({"organization_id": team})),
    );
    assert!(status < 300, "{status} {body}");
    let (status, _) = gateway.request("GET", "/v1/memoire/databases", &refs(&as_web(&[])), None);
    assert_eq!(status, 200);

    let create = json!({"name": "from-web", "kind": "persistent"});
    let (status, body) = gateway.request(
        "POST",
        "/v1/memoire/databases",
        &refs(&as_web(&[("origin", ORIGIN)])),
        Some(&create),
    );
    assert_eq!((status, code(&body)), (403, "csrf_failed"), "{body}");
    let (status, body) = gateway.request(
        "POST",
        "/v1/memoire/databases",
        &refs(&as_web(&[
            ("origin", "http://elsewhere.test"),
            ("x-csrf-token", csrf.as_str()),
        ])),
        Some(&create),
    );
    assert_eq!((status, code(&body)), (403, "origin_rejected"), "{body}");
    let (status, body) = gateway.request(
        "POST",
        "/v1/memoire/databases",
        &refs(&as_web(&[
            ("origin", ORIGIN),
            ("x-csrf-token", csrf.as_str()),
        ])),
        Some(&create),
    );
    assert_eq!(status, 201, "{body}");

    // A cookie and a bearer together are ambiguous.
    let bearer = owner.bearer();
    let (status, body) = gateway.request(
        "GET",
        "/v1/memoire/databases",
        &refs(&as_web(&[("authorization", bearer.as_str())])),
        None,
    );
    assert_eq!(
        (status, code(&body)),
        (400, "ambiguous_credentials"),
        "{body}"
    );
}

/// Borrowed header pairs.
fn refs(headers: &[(String, String)]) -> Vec<(&str, &str)> {
    headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}
