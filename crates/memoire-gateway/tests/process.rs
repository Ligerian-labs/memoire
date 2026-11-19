//! The gateway process: startup, health, refusal of a bad configuration,
//! and `whoami` through identity.
mod common;

use common::*;
use merezha_postgres::TestDatabase;
use std::{
    io::Read as _,
    time::{Duration, Instant},
};

#[test]
fn boots_ready_and_serves_the_memoire_routes() {
    let Some(db) = TestDatabase::create() else {
        eprintln!("skipped: MEREZHA_TEST_DATABASE_URL is unset");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::start(&config(dir.path(), r#"data_dir = "data""#), &db);

    assert_eq!(gateway.get("/health/live", None).0, 200);
    assert_eq!(gateway.get("/health/ready", None).0, 200);

    let teams = dir.path().join("data/teams");
    assert!(teams.is_dir(), "the module creates its data directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&teams).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    let (status, body) = gateway.get("/v1/memoire/whoami", None);
    assert_eq!(status, 401, "{body}");
    assert_eq!(body["error"]["code"], "unauthenticated");

    // A malformed credential is refused by identity at the edge.
    let (status, body) = gateway.get("/v1/memoire/whoami", Some("mzk_not-a-key"));
    assert_eq!(status, 401, "{body}");

    let (status, _) = gateway.get("/v1/memoire/no-such-route", None);
    assert_eq!(status, 404);

    assert!(
        gateway.stop().success(),
        "SIGTERM stops the gateway cleanly"
    );
}

#[test]
fn refuses_to_start_without_a_data_dir() {
    let Some(db) = TestDatabase::create() else {
        eprintln!("skipped: MEREZHA_TEST_DATABASE_URL is unset");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut child = Gateway::spawn(&config(dir.path(), ""), &db);
    let mut pipe = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = pipe.read_to_string(&mut text);
        text
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the gateway started without a data_dir");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stderr = reader.join().unwrap();
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("invalid_config"), "{stderr}");
    assert!(stderr.contains("modules.memoire.data_dir"), "{stderr}");
}

/// `whoami` through identity: a verified member's bearer session without and
/// with a team, then a machine's API key, which carries its team.
#[test]
fn whoami_reports_members_teams_and_machine_keys() {
    let Some(db) = TestDatabase::create() else {
        eprintln!("skipped: MEREZHA_TEST_DATABASE_URL is unset");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::start(&config(dir.path(), r#"data_dir = "data""#), &db);
    let origin = [("origin", "http://localhost:4200")];

    let (status, body) = gateway.request(
        "POST",
        "/v1/identity/registrations",
        &origin,
        Some(&serde_json::json!({
            "email": "member@memoire.test", "password": "correct horse battery staple 42",
        })),
    );
    assert!(status < 300, "{status} {body}");
    assert_eq!(body["status"], "pending_verification");
    let token = mailed_token(&gateway, "member@memoire.test");
    let (status, body) = gateway.request(
        "POST",
        "/v1/identity/email-verifications/confirm",
        &origin,
        Some(&serde_json::json!({"token": token, "transport": "bearer"})),
    );
    assert_eq!(status, 201, "{body}");
    let session = body["session"]["token"].as_str().unwrap().to_owned();
    let user = body["user"]["id"].as_str().unwrap().to_owned();
    // A key expiry inside identity's allowed range: the session's own.
    let expires_at = body["session"]["idle_expires_at"].clone();
    let bearer = format!("Bearer {session}");
    let auth = [("authorization", bearer.as_str())];

    let (status, body) = gateway.get("/v1/memoire/whoami", Some(&session));
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        serde_json::json!({"subject": user, "kind": "user", "team_id": null})
    );

    let (status, body) = gateway.request(
        "POST",
        "/v1/identity/organizations",
        &auth,
        Some(&serde_json::json!({"name": "Test team", "slug": "test-team"})),
    );
    assert_eq!(status, 201, "{body}");
    let team = body["organization"]["id"].as_str().unwrap().to_owned();
    let (_, body) = gateway.request(
        "GET",
        "/v1/memoire/whoami",
        &[auth[0], ("merezha-organization", team.as_str())],
        None,
    );
    assert_eq!(body["team_id"], team.as_str());

    let (status, body) = gateway.request(
        "POST",
        &format!("/v1/identity/organizations/{team}/service-accounts"),
        &auth,
        Some(&serde_json::json!({"name": "ci-runner"})),
    );
    assert_eq!(status, 201, "{body}");
    let machine = body["service_account"]["id"].as_str().unwrap().to_owned();
    let (status, body) = gateway.request(
        "POST",
        &format!("/v1/identity/organizations/{team}/api-keys"),
        &auth,
        Some(&serde_json::json!({
            "name": "ci-runner", "permissions": ["identity.organization.read"],
            "expires_at": expires_at, "service_account_id": machine,
        })),
    );
    assert_eq!(status, 201, "{body}");
    let key = body["key"].as_str().unwrap().to_owned();
    assert!(key.starts_with("mzk_"));
    let key_id = body["api_key"]["id"].as_str().unwrap().to_owned();

    let (status, body) = gateway.get("/v1/memoire/whoami", Some(&key));
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        serde_json::json!({"subject": key_id, "kind": "api_key", "team_id": team})
    );
}
