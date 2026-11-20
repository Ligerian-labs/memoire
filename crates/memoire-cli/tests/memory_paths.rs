//! How `memoire memory` picks its path: the gateway's memory API, the SQL
//! fallback against a gateway without it, `MEMOIRE_MEMORY_SQL=1`, and what
//! a key holding only the memory permissions can do.
mod support;

use serde_json::{Value, json};
use std::path::Path;
use support::{
    Memoire, Run,
    fake::{Fake, MEMBER_KEY, TEAM},
};

/// A CLI logged in with `key`, in `dir`, as agent `name` in session `s-1`.
fn agent(fake: &Fake, dir: &Path, key: &str, name: &str) -> Memoire {
    let memoire = Memoire::new()
        .dir(dir)
        .env("MEMOIRE_AGENT", name)
        .env("MEMOIRE_SESSION", "s-1");
    memoire
        .run(&["login", "--server", &fake.url], &format!("{key}\n"))
        .ok();
    memoire
}

/// `memoire`'s login, agent and session, on the SQL path.
fn on_sql(memoire: &Memoire, dir: &Path, name: &str) -> Memoire {
    Memoire::new()
        .dir(dir)
        .env(
            "MEMOIRE_CONFIG_DIR",
            memoire.config.path().join("memoire").to_str().unwrap(),
        )
        .env("MEMOIRE_AGENT", name)
        .env("MEMOIRE_SESSION", "s-1")
        .env("MEMOIRE_MEMORY_SQL", "1")
}

fn json_run(memoire: &Memoire, args: &[&str]) -> Run {
    let mut args = args.to_vec();
    args.push("--json");
    memoire.run(&args, "")
}

fn ok(memoire: &Memoire, args: &[&str]) -> Value {
    json_run(memoire, args).ok().json()
}

/// Requests to paths ending in `suffix`, by `method`.
fn count(fake: &Fake, method: &str, suffix: &str) -> usize {
    fake.requests()
        .iter()
        .filter(|r| r.method == method && r.path.ends_with(suffix))
        .count()
}

/// Write queries the fake has run.
fn write_queries(fake: &Fake) -> usize {
    fake.requests()
        .iter()
        .filter(|r| {
            r.path.ends_with("/query")
                && serde_json::from_slice::<Value>(&r.body).is_ok_and(|b| b["mode"] == "write")
        })
        .count()
}

#[test]
fn a_gateway_without_the_memory_api_gets_the_sql_convention() {
    let fake = Fake::start_without_memory();
    let dir = tempfile::tempdir().unwrap();
    let a = agent(&fake, dir.path(), MEMBER_KEY, "agent-a");
    assert_eq!(ok(&a, &["memory", "init"])["migrated_from"], 0);
    assert_eq!(count(&fake, "GET", "/databases/memory/memory"), 1);

    // The write asks the API once, finds no route, and writes once.
    let writes = write_queries(&fake);
    let memory = ok(
        &a,
        &["memory", "remember", "--title", "Old gateways still work"],
    );
    assert_eq!(count(&fake, "POST", "/memory/memories"), 1);
    assert_eq!(write_queries(&fake), writes + 1);
    // Through SQL, the principal comes from whoami.
    assert_eq!(memory["author"]["principal"], "key_fake_member");
    let found = ok(&a, &["memory", "recall", "old gateways"]);
    assert_eq!(found["memories"][0]["id"], memory["id"]);
    let rows: i64 = rusqlite::Connection::open(fake.database_path("memory"))
        .unwrap()
        .query_row("SELECT count(*) FROM memoire_memory_memories", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1);
    // An answer of the gateway is not a missing route.
    let run = json_run(&a, &["memory", "--db", "nope", "recall"]);
    assert_eq!(run.code, 5);
    assert_eq!(run.error()["error"]["code"], "database_not_found");
    // MEMOIRE_MEMORY_SQL=0 pins the API: the missing route is reported,
    // and nothing goes over the query route.
    let pinned = agent(&fake, dir.path(), MEMBER_KEY, "agent-p").env("MEMOIRE_MEMORY_SQL", "0");
    let queries = count(&fake, "POST", "/query");
    let run = json_run(&pinned, &["memory", "recall"]);
    assert_eq!(run.code, 5, "{}", run.stderr);
    assert_eq!(run.error()["error"]["code"], "not_found");
    assert_eq!(count(&fake, "POST", "/query"), queries);
}

#[test]
fn a_memory_only_key_uses_the_api_and_nothing_else() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let member = agent(&fake, dir.path(), MEMBER_KEY, "member");
    ok(&member, &["memory", "init"]);
    fake.add_key_as(
        "mzk_fake_memory_key",
        "key_fake_memory",
        Some(TEAM),
        &["memoire.memory.read", "memoire.memory.write"],
    );
    let a = agent(&fake, dir.path(), "mzk_fake_memory_key", "agent-a");
    let whoami = count(&fake, "GET", "/whoami");

    let memory = ok(
        &a,
        &["memory", "remember", "--title", "Agents run on memory keys"],
    );
    let id = memory["id"].as_str().unwrap();
    assert_eq!(memory["author"]["principal"], "key_fake_memory");
    assert_eq!(memory["author"]["agent"], "agent-a");
    let found = ok(&a, &["memory", "recall", "memory keys"]);
    assert_eq!(found["memories"][0]["id"], id);
    let confirmed = ok(
        &member,
        &["memory", "confirm", id, "--evidence", "ran an agent"],
    );
    assert_eq!(confirmed["status"], "confirmed");
    ok(&a, &["memory", "show", id, "--history"]);
    ok(
        &a,
        &[
            "memory",
            "revise",
            id,
            "--add-tag",
            "keys",
            "--reason",
            "tag",
        ],
    );
    assert_eq!(
        ok(&a, &["memory", "log", "--unseen"])["unseen"]["first"],
        true
    );
    let output = dir.path().join("out.jsonl");
    assert_eq!(
        ok(&a, &["memory", "export", "-o", output.to_str().unwrap()])["count"],
        1
    );
    assert_eq!(
        count(&fake, "GET", "/whoami"),
        whoami,
        "the API path never asks"
    );

    // No SQL: neither a query nor the memory's SQL convention.
    assert_eq!(a.run(&["query", "memory", "SELECT 1"], "").code, 4);
    let sql = on_sql(&a, dir.path(), "agent-a");
    let run = json_run(&sql, &["memory", "recall"]);
    assert_eq!(run.code, 4, "{}", run.stderr);
    assert_eq!(run.error()["error"]["code"], "unauthorized");
    // Nor the catalog.
    assert_eq!(a.run(&["db", "ls"], "").code, 4);
}

#[test]
fn a_memory_only_key_initialises_a_store_in_a_database_that_exists() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let member = agent(&fake, dir.path(), MEMBER_KEY, "member");
    member.run(&["db", "create", "memory"], "").ok();
    fake.add_key_as(
        "mzk_fake_memory_key",
        "key_fake_memory",
        Some(TEAM),
        &["memoire.memory.read", "memoire.memory.write"],
    );
    let a = agent(&fake, dir.path(), "mzk_fake_memory_key", "agent-a");

    let init = ok(&a, &["memory", "init", "--burstable"]);
    assert_eq!(
        (&init["database"], &init["created"], &init["migrated_from"]),
        (&json!(null), &json!(false), &json!(0))
    );
    assert!(init["store_id"].as_str().unwrap().starts_with("mst_"));
    let run = a.run(&["memory", "init"], "").ok();
    assert!(
        run.stdout
            .starts_with("memory store     schema 1, up to date")
            && !run.stdout.contains("database"),
        "{}",
        run.stdout
    );
    assert_eq!(run.stderr, "", "no note on a kind it cannot see");
    ok(
        &a,
        &[
            "memory",
            "remember",
            "--title",
            "Stores start from memory keys",
        ],
    );
    // A full key's init is unchanged.
    let full = ok(&member, &["memory", "init"]);
    assert_eq!(
        (
            &full["database"]["name"],
            &full["created"],
            &full["migrated_from"]
        ),
        (&json!("memory"), &json!(false), &json!(1))
    );

    // A missing database needs a key that may create one.
    let run = json_run(&a, &["memory", "--db", "other", "init"]);
    assert_eq!(run.code, 5, "{}", run.stderr);
    let error = &run.error()["error"];
    assert_eq!(error["code"], "database_not_found");
    let message = error["message"].as_str().unwrap();
    assert!(
        message.contains("may not create") && message.contains("memoire memory init"),
        "{message}"
    );
    let listed = member.run(&["db", "ls", "--json"], "").ok().stdout;
    assert!(!listed.contains("\"other\""), "{listed}");
    assert_eq!(count(&fake, "POST", "/databases/other/memory/init"), 0);
}

/// The same command on both paths, against the same state: its exit code
/// and standard error.
fn both(memoire: &Memoire, sql: &Memoire, args: &[&str]) -> [(i32, String); 2] {
    let api = json_run(memoire, args);
    let sql = json_run(sql, args);
    assert_eq!(api.stdout, "");
    assert_eq!(sql.stdout, "");
    [(api.code, api.stderr), (sql.code, sql.stderr)]
}

#[test]
fn refusals_are_reported_byte_for_byte_alike_on_both_paths() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let a = agent(&fake, dir.path(), MEMBER_KEY, "agent-a");
    let sql = on_sql(&a, dir.path(), "agent-a");
    ok(&a, &["memory", "init"]);
    let id = ok(
        &a,
        &["memory", "remember", "--title", "Deploys need the VPN"],
    )["id"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(
        &a,
        &[
            "memory",
            "revise",
            &id,
            "--title",
            "Deploys need the VPN up",
            "--reason",
            "r",
        ],
    );
    let gone = ok(&a, &["memory", "remember", "--title", "Forgotten note"])["id"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(&a, &["memory", "forget", &gone, "--reason", "old"]);
    let purged = ok(&a, &["memory", "remember", "--title", "Purged note"])["id"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(
        &a,
        &["memory", "forget", &purged, "--reason", "leak", "--purge"],
    );
    let mut ten = vec!["memory", "revise", &id, "--reason", "tags"];
    let tags: Vec<String> = (0..10).map(|i| format!("t{i}")).collect();
    for tag in &tags {
        ten.extend(["--tag", tag]);
    }
    ok(&a, &ten);
    let secret = format!("{}{}", ["mz", "k_"].concat(), "q7Rx".repeat(6));
    let title = format!("token {secret}");

    // Each case: its code, its exit, the command, the route it reaches
    // and how many requests to that route it makes (0: refused here).
    let cases: [(&str, i32, Vec<&str>, &str, usize); 6] = [
        (
            "memory_conflict",
            6,
            vec!["memory", "confirm", &id, "--evidence", "e", "--rev", "1"],
            "/confirm",
            1,
        ),
        (
            "memory_duplicate",
            6,
            vec!["memory", "remember", "--title", "Deploys need the VPN up!"],
            "/memories",
            1,
        ),
        (
            "memory_not_current",
            6,
            vec!["memory", "revise", &gone, "--title", "x", "--reason", "r"],
            "/revise",
            1,
        ),
        (
            "memory_not_found",
            5,
            vec!["memory", "show", &purged],
            &purged,
            1,
        ),
        // A secret in a replacement's own title is refused before the
        // replaced memory is read: no request on either path.
        (
            "memory_secret_detected",
            7,
            vec![
                "memory",
                "remember",
                "--title",
                &title,
                "--supersedes",
                &id,
                "--reason",
                "r",
            ],
            "/memories",
            0,
        ),
        // Ten tags there and one more: only the store knows.
        (
            "memory_too_large",
            7,
            vec![
                "memory",
                "revise",
                &id,
                "--add-tag",
                "eleventh",
                "--reason",
                "r",
            ],
            "/revise",
            1,
        ),
    ];
    for (code, exit, args, route, sent) in cases {
        let all = fake.requests().len();
        let asked = fake
            .requests()
            .iter()
            .filter(|r| r.path.ends_with(route))
            .count();
        let [api, sql] = both(&a, &sql, &args);
        assert_eq!(api, sql, "{code}");
        assert_eq!(api.0, exit, "{code}: {}", api.1);
        let error: Value = serde_json::from_str(api.1.trim()).unwrap();
        assert_eq!(error["error"]["code"], code);
        assert!(!api.1.contains(&secret));
        // The refusal came from the gateway's memory route, or from
        // nowhere but this machine.
        assert_eq!(
            fake.requests()
                .iter()
                .filter(|r| r.path.ends_with(route))
                .count(),
            asked + sent,
            "{code}"
        );
        if sent == 0 {
            assert_eq!(fake.requests().len(), all, "{code}");
        }
    }
    let error: Value =
        serde_json::from_str(both(&a, &sql, &["memory", "show", &purged])[0].1.trim()).unwrap();
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("was purged at")
    );
}

#[test]
fn init_on_a_current_store_writes_nothing_so_a_reader_may_run_it() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let member = agent(&fake, dir.path(), MEMBER_KEY, "member");
    let first = ok(&member, &["memory", "init"]);
    fake.add_key_as(
        "mzk_fake_reader_key",
        "key_fake_reader",
        Some(TEAM),
        &["memoire.databases.read", "memoire.memory.read"],
    );
    let reader = agent(&fake, dir.path(), "mzk_fake_reader_key", "reader");
    let posts = count(&fake, "POST", "");
    let again = ok(&reader, &["memory", "init"]);
    assert_eq!(
        (
            &again["created"],
            &again["migrated_from"],
            &again["store_id"]
        ),
        (&json!(false), &json!(1), &first["store_id"])
    );
    assert_eq!(count(&fake, "POST", ""), posts, "nothing was written");
    assert_eq!(count(&fake, "GET", "/databases/memory/memory"), 2);
}

#[test]
fn a_writer_that_may_not_read_is_not_shown_the_duplicates() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let member = agent(&fake, dir.path(), MEMBER_KEY, "member");
    ok(&member, &["memory", "init"]);
    ok(
        &member,
        &["memory", "remember", "--title", "Backups run at midnight"],
    );
    fake.add_key_as(
        "mzk_fake_writer_key",
        "key_fake_writer",
        Some(TEAM),
        &["memoire.memory.write"],
    );
    let writer = agent(&fake, dir.path(), "mzk_fake_writer_key", "writer");
    let run = json_run(
        &writer,
        &["memory", "remember", "--title", "Backups run at midnight!"],
    );
    assert_eq!(run.code, 6, "{}", run.stderr);
    let error = &run.error()["error"];
    assert_eq!(
        (&error["code"], &error["candidates"]),
        (&json!("memory_duplicate"), &json!([]))
    );
    let message = error["message"].as_str().unwrap();
    assert!(
        !message.contains("()") && message.contains("--force"),
        "{message}"
    );
    let forced = ok(
        &writer,
        &[
            "memory",
            "remember",
            "--title",
            "Backups run at midnight!",
            "--force",
        ],
    );
    assert_eq!(forced["author"]["principal"], "key_fake_writer");
}

#[test]
fn the_first_answer_of_a_memory_route_settles_the_path() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let a = agent(&fake, dir.path(), MEMBER_KEY, "agent-a");
    a.run(&["db", "create", "memory"], "").ok();
    // The store answers, then its init route is missing: an error, never
    // a second try through SQL.
    fake.answer_once(
        "POST",
        "/v1/memoire/databases/memory/memory/init",
        404,
        json!({"error": {"code": "not_found", "classification": "permanent"}}),
    );
    let queries = count(&fake, "POST", "/query");
    let run = json_run(&a, &["memory", "init"]);
    assert_eq!(run.code, 5, "{}", run.stderr);
    assert_eq!(run.error()["error"]["code"], "not_found");
    assert_eq!(count(&fake, "GET", "/databases/memory/memory"), 1);
    assert_eq!(count(&fake, "POST", "/query"), queries);
}

#[test]
fn only_the_engines_input_refusals_are_rebuilt() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let a = agent(&fake, dir.path(), MEMBER_KEY, "agent-a");
    ok(&a, &["memory", "init"]);
    let refusal = json!({"error": {"code": "validation_failed", "classification": "permanent",
                                   "details": [{"path": "/scpoe", "code": "unknown_field"}]}});
    fake.answer_once(
        "GET",
        "/v1/memoire/databases/memory/memory/memories",
        422,
        refusal,
    );
    let run = json_run(&a, &["memory", "recall"]);
    assert_eq!(run.code, 7, "{}", run.stderr);
    let error = &run.error()["error"];
    assert_eq!(
        (&error["code"], &error["status"], &error["details"]),
        (
            &json!("validation_failed"),
            &json!(422),
            &json!([{"path": "/scpoe", "code": "unknown_field"}])
        )
    );
}

#[test]
fn init_reports_the_version_the_gateway_keeps_the_store_at() {
    let fake = Fake::start();
    let dir = tempfile::tempdir().unwrap();
    let a = agent(&fake, dir.path(), MEMBER_KEY, "agent-a");
    ok(&a, &["memory", "init"]);
    fake.answer_once(
        "GET",
        "/v1/memoire/databases/memory/memory",
        200,
        json!({"schema_version": 2, "supported_version": 2,
               "store_id": "mst_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8"}),
    );
    let init = ok(&a, &["memory", "init"]);
    assert_eq!(
        (&init["schema_version"], &init["migrated_from"]),
        (&json!(2), &json!(2))
    );
    fake.answer_once(
        "GET",
        "/v1/memoire/databases/memory/memory",
        200,
        json!({"schema_version": 2, "supported_version": 2,
               "store_id": "mst_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8"}),
    );
    let run = a.run(&["memory", "init"], "").ok();
    assert!(
        run.stdout.contains("schema 2, up to date"),
        "{}",
        run.stdout
    );
}
