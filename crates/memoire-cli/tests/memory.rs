//! `memoire memory` against the in-process gateway (`support::fake`), whose
//! query route runs batches like the data plane: transactions, defensive
//! connections and the statement authorizer.
mod support;

use serde_json::{Value, json};
use std::path::Path;
use support::{
    Memoire, Run,
    fake::{Fake, MEMBER_KEY, TEAM},
};

/// A fake, and a working directory outside any git checkout.
fn world() -> (Fake, tempfile::TempDir) {
    (Fake::start(), tempfile::tempdir().unwrap())
}

/// A CLI logged in with `key`, run in `dir` as agent `name` in `session`.
fn agent_with(fake: &Fake, dir: &Path, key: &str, name: &str, session: &str) -> Memoire {
    let memoire = Memoire::new()
        .dir(dir)
        .env("MEMOIRE_AGENT", name)
        .env("MEMOIRE_SESSION", session);
    memoire
        .run(&["login", "--server", &fake.url], &format!("{key}\n"))
        .ok();
    memoire
}

fn agent(fake: &Fake, dir: &Path, name: &str, session: &str) -> Memoire {
    agent_with(fake, dir, MEMBER_KEY, name, session)
}

fn with_json<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut args = args.to_vec();
    args.push("--json");
    args
}

/// Runs `memoire ARGS --json`, which must succeed; its JSON.
fn ok(memoire: &Memoire, args: &[&str]) -> Value {
    memoire.run(&with_json(args), "").ok().json()
}

/// Runs `memoire ARGS --json`, which must fail with `code`; its error
/// object.
fn refused(memoire: &Memoire, args: &[&str], code: i32) -> Value {
    let run = memoire.run(&with_json(args), "");
    assert_eq!(
        run.code, code,
        "stdout: {}\nstderr: {}",
        run.stdout, run.stderr
    );
    assert_eq!(run.stdout, "", "a failure prints nothing on stdout");
    run.error()["error"].clone()
}

/// `memory remember --title TITLE EXTRA…`; the memory.
fn remember(memoire: &Memoire, title: &str, extra: &[&str]) -> Value {
    let mut args = vec!["memory", "remember", "--title", title];
    args.extend_from_slice(extra);
    ok(memoire, &args)
}

fn id_of(memory: &Value) -> String {
    memory["id"].as_str().unwrap().to_owned()
}

fn sqlite(fake: &Fake, db: &str) -> rusqlite::Connection {
    rusqlite::Connection::open(fake.database_path(db)).unwrap()
}

fn count(fake: &Fake, sql: &str) -> i64 {
    sqlite(fake, "memory")
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

/// Rows in every store table, to show that a refused write left nothing.
fn footprint(fake: &Fake) -> Vec<i64> {
    [
        "memoire_memory_memories",
        "memoire_memory_revisions",
        "memoire_memory_events",
        "memoire_memory_fts",
    ]
    .iter()
    .map(|table| count(fake, &format!("SELECT count(*) FROM {table}")))
    .collect()
}

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A string the secret scanner flags, put together at run time so this
/// file holds nothing that looks like a credential.
fn fake_secret() -> String {
    format!("{}{}", ["mz", "k_"].concat(), "q7Rx".repeat(6))
}

#[test]
fn init_creates_a_persistent_store_once() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    let first = ok(&a, &["memory", "init"]);
    assert_eq!(first["created"], true);
    assert_eq!(first["database"]["name"], "memory");
    assert_eq!(first["database"]["kind"], "persistent");
    assert_eq!(first["schema_version"], 1);
    assert_eq!(first["migrated_from"], 0);
    let store_id = first["store_id"].as_str().unwrap();
    assert!(
        store_id.starts_with("mst_") && store_id.len() == 30,
        "{store_id}"
    );
    let body: Value = serde_json::from_slice(
        &fake
            .requests()
            .iter()
            .find(|r| r.method == "POST" && r.path == "/v1/memoire/databases")
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(body, json!({"name": "memory", "kind": "persistent"}));

    let again = ok(&a, &["memory", "init"]);
    assert_eq!(again["created"], false);
    assert_eq!(again["migrated_from"], 1);
    assert_eq!(again["store_id"], store_id);
    let tables = count(
        &fake,
        "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'memoire_memory_%' AND type = 'table'",
    );

    // A kind flag that disagrees is noted, and the database kept.
    let run = a.run(&["memory", "init", "--burstable", "--json"], "").ok();
    assert!(
        run.stderr
            .contains("already exists as a persistent database"),
        "{}",
        run.stderr
    );
    assert_eq!(run.json()["database"]["kind"], "persistent");
    assert_eq!(
        count(
            &fake,
            "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'memoire_memory_%' AND type = 'table'"
        ),
        tables
    );

    let scratch = ok(
        &a,
        &[
            "memory",
            "--db",
            "scratch",
            "init",
            "--burstable",
            "--ttl",
            "2h",
        ],
    );
    assert_eq!(scratch["created"], true);
    assert_eq!(scratch["database"]["kind"], "burstable");
    assert_eq!(scratch["database"]["idle_ttl_seconds"], 7_200);
    assert_ne!(scratch["store_id"], store_id);
    // An expired database is created again, as a new store.
    fake.expire("scratch");
    let renewed = ok(&a, &["memory", "--db", "scratch", "init", "--burstable"]);
    assert_eq!(renewed["created"], true);
    assert_eq!(renewed["migrated_from"], 0);

    let run = a.run(&["memory", "init"], "").ok();
    assert!(
        run.stdout.contains("memory store     schema 1, up to date"),
        "{}",
        run.stdout
    );
}

#[test]
fn verbs_need_a_database_and_a_store() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    let id = "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8";
    let other = "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S9";
    let output = dir.path().join("out.jsonl");
    let output = output.to_str().unwrap();
    let verbs: [&[&str]; 12] = [
        &["memory", "recall", "vpn"],
        &["memory", "show", id],
        &["memory", "remember", "--title", "A title"],
        &["memory", "revise", id, "--title", "New", "--reason", "why"],
        &["memory", "confirm", id, "--evidence", "ran it"],
        &["memory", "dispute", id, "--reason", "wrong"],
        &["memory", "resolve", id, "--reason", "fixed"],
        &[
            "memory",
            "supersede",
            id,
            "--by",
            other,
            "--reason",
            "newer",
        ],
        &["memory", "forget", id, "--reason", "old"],
        &["memory", "forget", id, "--reason", "leak", "--purge"],
        &["memory", "log", "--unseen"],
        &["memory", "export", "-o", output],
    ];
    for verb in verbs {
        let error = refused(&a, verb, 5);
        assert_eq!(error["code"], "database_not_found", "{verb:?}");
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("memoire memory init")
        );
    }
    // Only init creates the database.
    assert!(
        !fake
            .requests()
            .iter()
            .any(|r| r.method == "POST" && r.path == "/v1/memoire/databases")
    );
    a.run(&["db", "create", "memory"], "").ok();
    for verb in verbs {
        let error = refused(&a, verb, 6);
        assert_eq!(error["code"], "memory_not_initialized", "{verb:?}");
        assert_eq!(error["classification"], "conflict");
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("memoire memory init")
        );
    }
    // A refused export leaves no file.
    assert!(!dir.path().join("out.jsonl").exists());
    // init adopts the plain database.
    assert_eq!(ok(&a, &["memory", "init"])["created"], false);
    ok(&a, &["memory", "recall"]);
    ok(&a, &["memory", "log"]);
}

#[test]
fn a_store_of_another_version_is_refused() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let set = |version: &str| {
        a.run(
            &[
                "query",
                "memory",
                "UPDATE memoire_memory_meta SET value = ? WHERE key = 'schema_version'",
                "-p",
                &format!("\"{version}\""),
                "--write",
            ],
            "",
        )
        .ok();
    };
    set("99");
    for verb in [&["memory", "recall"][..], &["memory", "init"]] {
        let error = refused(&a, verb, 6);
        assert_eq!(error["code"], "memory_schema_newer");
        assert_eq!(error["store_version"], 99);
        assert_eq!(error["supported_version"], 1);
    }
    set("0");
    let error = refused(&a, &["memory", "remember", "--title", "x"], 6);
    assert_eq!(error["code"], "memory_schema_older");
    assert_eq!(error["store_version"], 0);
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("memoire memory init")
    );
}

#[test]
fn remember_records_what_show_returns() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let memory = remember(&a, "  Deploys need the VPN  ", &[]);
    let id = id_of(&memory);
    assert!(id.starts_with("mem_") && id.len() == 30, "{id}");
    let keys: Vec<&str> = memory
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut expected = vec![
        "id",
        "rev",
        "kind",
        "scope",
        "title",
        "body",
        "tags",
        "evidence",
        "status",
        "supersedes",
        "superseded_by",
        "author",
        "confirmers",
        "open_disputes",
        "created_at",
        "updated_at",
    ];
    expected.sort_unstable();
    let mut keys = keys;
    keys.sort_unstable();
    assert_eq!(keys, expected);
    assert_eq!(memory["title"], "Deploys need the VPN");
    assert_eq!(memory["kind"], "fact");
    assert_eq!(memory["status"], "inferred");
    assert_eq!(memory["scope"], "global");
    assert_eq!(memory["rev"], 1);
    assert_eq!(memory["body"], "");
    assert_eq!(memory["tags"], json!([]));
    assert_eq!(
        memory["author"],
        json!({"principal": "key_fake_member", "agent": "agent-a", "session": "s-1"})
    );
    assert_eq!(
        (
            memory["confirmers"].clone(),
            memory["open_disputes"].clone()
        ),
        (json!(0), json!(0))
    );
    assert_eq!(memory["created_at"], memory["updated_at"]);
    assert!(memory["created_at"].as_str().unwrap().ends_with('Z'));

    let shown = ok(&a, &["memory", "show", &id]);
    assert_eq!(
        shown,
        json!({"memory": memory, "confirmations": [], "disputes": []})
    );
    // Ids are accepted in lowercase too.
    let lower = format!("mem_{}", id[4..].to_ascii_lowercase());
    assert_eq!(ok(&a, &["memory", "show", &lower])["memory"]["id"], id);

    let file = dir.path().join("body.md");
    std::fs::write(&file, "Run `vpn up` first.\nThen deploy.\n").unwrap();
    let from_file = remember(
        &a,
        "Deploy procedure for staging",
        &[
            "--body-file",
            file.to_str().unwrap(),
            "--kind",
            "procedure",
            "--scope",
            "client:acme",
            "--tag",
            "deploy",
            "--tag",
            "vpn",
            "--tag",
            "deploy",
            "--evidence",
            "docs/deploy.md",
            "--evidence",
            "https://ci.example/runs/42",
        ],
    );
    assert_eq!(from_file["body"], "Run `vpn up` first.\nThen deploy.\n");
    assert_eq!(from_file["kind"], "procedure");
    assert_eq!(from_file["scope"], "client:acme");
    assert_eq!(from_file["tags"], json!(["deploy", "vpn"]));
    assert_eq!(
        from_file["evidence"],
        json!(["docs/deploy.md", "https://ci.example/runs/42"])
    );
    let shown = ok(&a, &["memory", "show", &id_of(&from_file), "--history"]);
    assert_eq!(shown["memory"], from_file);
    let revision = &shown["revisions"][0];
    assert_eq!(revision["rev"], 1);
    assert_eq!(revision["reason"], "remembered");
    assert_eq!(revision["tags"], json!(["deploy", "vpn"]));
    assert_eq!(revision["author_agent"], "agent-a");

    let run = a
        .run(
            &[
                "memory",
                "remember",
                "--title",
                "From stdin",
                "--body-file",
                "-",
                "--json",
            ],
            "piped body",
        )
        .ok();
    assert_eq!(run.json()["body"], "piped body");

    let run = a.run(&["memory", "show", &id], "").ok();
    assert!(
        run.stdout.starts_with(&format!("id             {id}\n")),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("status         inferred\n"),
        "{}",
        run.stdout
    );

    let error = refused(&a, &["memory", "show", "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8"], 5);
    assert_eq!(error["code"], "memory_not_found");
    assert_eq!(error["id"], "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8");
}

#[test]
fn the_default_scope_comes_from_the_git_remote() {
    let (fake, outside) = world();
    let checkout = tempfile::tempdir().unwrap();
    git(checkout.path(), &["init", "-q"]);
    git(
        checkout.path(),
        &["remote", "add", "origin", "git@github.com:Acme/Widgets.git"],
    );
    let inside = agent(&fake, checkout.path(), "agent-a", "s-1");
    let elsewhere = agent(&fake, outside.path(), "agent-b", "s-2");
    ok(&inside, &["memory", "init"]);

    let project = remember(&inside, "Widgets build with make", &[]);
    assert_eq!(project["scope"], "project:github.com/acme/widgets");
    let global = remember(&elsewhere, "Prefer small pull requests", &[]);
    assert_eq!(global["scope"], "global");
    let other = remember(
        &elsewhere,
        "Other project fact",
        &["--scope", "project:github.com/acme/other"],
    );

    let found = ok(&inside, &["memory", "recall"]);
    assert_eq!(
        found["scopes"],
        json!(["project:github.com/acme/widgets", "global"])
    );
    let ids: Vec<&Value> = found["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| &m["id"])
        .collect();
    assert_eq!(ids, [&global["id"], &project["id"]]);
    assert_eq!(
        ok(&elsewhere, &["memory", "recall"])["scopes"],
        json!(["global"])
    );
    let all = ok(&elsewhere, &["memory", "recall", "--all-scopes"]);
    assert_eq!(all["scopes"], Value::Null);
    assert_eq!(all["memories"].as_array().unwrap().len(), 3);
    assert_eq!(all["memories"][0]["id"], other["id"]);
}

#[test]
fn near_duplicates_are_refused_unless_forced() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let first = remember(&a, "Deploys require the VPN to be connected", &[]);
    remember(&a, "Cache cargo registry in CI", &[]);
    let before = footprint(&fake);

    let error = refused(
        &a,
        &[
            "memory",
            "remember",
            "--title",
            "Deploy requires VPN connection",
        ],
        6,
    );
    assert_eq!(error["code"], "memory_duplicate");
    assert_eq!(error["classification"], "conflict");
    let candidates = error["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["id"], first["id"]);
    assert_eq!(candidates[0]["title"], first["title"]);
    assert_eq!(candidates[0]["status"], "inferred");
    assert_eq!(candidates[0]["similarity"], 0.75);
    assert_eq!(footprint(&fake), before);

    let forced = remember(&a, "Deploy requires VPN connection", &["--force"]);
    assert_ne!(forced["id"], first["id"]);
    let elsewhere = remember(
        &a,
        "Deploy requires VPN connection",
        &["--scope", "client:acme"],
    );
    assert_eq!(elsewhere["scope"], "client:acme");
    // A title of stopwords only skips the check.
    remember(&a, "It is as it was", &[]);
    remember(&a, "It is as it was", &[]);
}

#[test]
fn revise_changes_only_the_given_fields() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    let b = agent(&fake, dir.path(), "agent-b", "s-2");
    ok(&a, &["memory", "init"]);
    let memory = remember(
        &a,
        "Staging deploys need the VPN",
        &[
            "--body",
            "Connect first.",
            "--tag",
            "deploy",
            "--evidence",
            "docs/a.md",
        ],
    );
    let id = id_of(&memory);

    let revised = ok(
        &b,
        &[
            "memory",
            "revise",
            &id,
            "--title",
            "Staging deploys need the office VPN",
            "--reason",
            "more precise",
        ],
    );
    assert_eq!(revised["rev"], 2);
    assert_eq!(revised["title"], "Staging deploys need the office VPN");
    for field in ["body", "kind", "scope", "tags", "evidence", "created_at"] {
        assert_eq!(revised[field], memory[field], "{field}");
    }
    assert_eq!(revised["author"]["agent"], "agent-b");
    assert_ne!(revised["updated_at"], memory["updated_at"]);

    let c = agent(&fake, dir.path(), "agent-c", "s-3");
    let confirmed = ok(&c, &["memory", "confirm", &id, "--evidence", "ran it"]);
    assert_eq!(confirmed["status"], "confirmed");
    let tagged = ok(
        &a,
        &[
            "memory",
            "revise",
            &id,
            "--add-tag",
            "vpn",
            "--add-tag",
            "deploy",
            "--reason",
            "tagging",
        ],
    );
    assert_eq!(tagged["tags"], json!(["deploy", "vpn"]));
    assert_eq!(
        tagged["status"], "confirmed",
        "a tag change keeps the confirmation"
    );
    assert_eq!(tagged["confirmers"], 1);
    let evidenced = ok(
        &a,
        &[
            "memory",
            "revise",
            &id,
            "--add-evidence",
            "docs/b.md",
            "--reason",
            "source",
        ],
    );
    assert_eq!(evidenced["evidence"], json!(["docs/a.md", "docs/b.md"]));
    assert_eq!(evidenced["status"], "confirmed");
    let replaced = ok(
        &a,
        &["memory", "revise", &id, "--tag", "ops", "--reason", "retag"],
    );
    assert_eq!(replaced["tags"], json!(["ops"]));

    let rewritten = ok(
        &a,
        &[
            "memory",
            "revise",
            &id,
            "--body",
            "Connect to the office VPN first.",
            "--reason",
            "clearer",
        ],
    );
    assert_eq!(
        rewritten["status"], "inferred",
        "new text needs a new confirmation"
    );
    assert_eq!(rewritten["confirmers"], 0);
    assert_eq!(rewritten["rev"], 6);

    let error = refused(
        &a,
        &[
            "memory",
            "revise",
            &id,
            "--title",
            "Staging deploys need the office VPN",
            "--reason",
            "same",
        ],
        7,
    );
    assert_eq!(error["code"], "memory_unchanged");
    refused(
        &a,
        &[
            "memory",
            "revise",
            &id,
            "--add-tag",
            "ops",
            "--reason",
            "same",
        ],
        7,
    );

    let history = ok(&a, &["memory", "show", &id, "--history"]);
    let revisions = history["revisions"].as_array().unwrap();
    let revs: Vec<i64> = revisions
        .iter()
        .map(|r| r["rev"].as_i64().unwrap())
        .collect();
    assert_eq!(revs, [1, 2, 3, 4, 5, 6]);
    assert_eq!(revisions[0]["title"], "Staging deploys need the VPN");
    assert_eq!(revisions[1]["reason"], "more precise");
    assert_eq!(revisions[1]["author_session"], "s-2");
    let statuses: Vec<&str> = revisions
        .iter()
        .map(|r| r["status"].as_str().unwrap())
        .collect();
    assert_eq!(
        statuses,
        [
            "inferred",
            "inferred",
            "confirmed",
            "confirmed",
            "confirmed",
            "inferred"
        ]
    );
    assert_eq!(history["confirmations"][0]["counts"], false);
    assert_eq!(history["memory"], rewritten);
    // The search row follows the text.
    let found = ok(&a, &["memory", "recall", "office"]);
    assert_eq!(found["memories"][0]["id"], id);
    let events = count(
        &fake,
        &format!(
            "SELECT count(*) FROM memoire_memory_events WHERE memory_id = '{id}' AND verb = 'revise'"
        ),
    );
    assert_eq!(events, 5);

    // Superseded memories are history.
    let newer = id_of(&remember(&a, "Every deploy goes through the bastion", &[]));
    ok(
        &a,
        &[
            "memory",
            "supersede",
            &id,
            "--by",
            &newer,
            "--reason",
            "bastion now",
        ],
    );
    let error = refused(
        &a,
        &["memory", "revise", &id, "--title", "x", "--reason", "y"],
        6,
    );
    assert_eq!(error["code"], "memory_not_current");
    assert_eq!(error["status"], "superseded");
    assert_eq!(error["superseded_by"], newer);
    let error = refused(
        &a,
        &[
            "memory",
            "revise",
            "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8",
            "--title",
            "x",
            "--reason",
            "y",
        ],
        5,
    );
    assert_eq!(error["code"], "memory_not_found");
}

#[test]
fn a_writer_that_read_a_stale_rev_conflicts_and_leaves_nothing() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    let b = agent(&fake, dir.path(), "agent-b", "s-2");
    ok(&a, &["memory", "init"]);
    let id = id_of(&remember(&a, "Release checklist lives in the wiki", &[]));
    let before = footprint(&fake);

    // B's whole revise runs between A's read and A's write.
    let (sender, receiver) = std::sync::mpsc::channel::<Run>();
    let racing = id.clone();
    fake.before_next_write(move || {
        let run = b.run(
            &[
                "memory",
                "revise",
                &racing,
                "--title",
                "Release checklist lives in the handbook",
                "--reason",
                "moved",
                "--json",
            ],
            "",
        );
        sender.send(run).unwrap();
    });
    let error = refused(
        &a,
        &[
            "memory",
            "revise",
            &id,
            "--title",
            "Release checklist zanzibar",
            "--body",
            "zanzibar",
            "--reason",
            "zanzibar edit",
        ],
        6,
    );
    let b_run = receiver.recv().unwrap().ok();
    assert_eq!(b_run.json()["rev"], 2);
    assert_eq!(error["code"], "memory_conflict");
    assert_eq!(error["id"], id);
    assert_eq!(error["current_rev"], 2);
    assert!(error["message"].as_str().unwrap().contains("rev 2"));

    let shown = ok(&a, &["memory", "show", &id, "--history"]);
    assert_eq!(
        shown["memory"]["title"],
        "Release checklist lives in the handbook"
    );
    assert_eq!(shown["memory"]["rev"], 2);
    assert_eq!(shown["revisions"].as_array().unwrap().len(), 2);
    // B's batch added one revision and one event; A's added nothing.
    assert_eq!(
        footprint(&fake),
        [before[0], before[1] + 1, before[2] + 1, before[3]]
    );
    assert_eq!(
        count(
            &fake,
            "SELECT count(*) FROM memoire_memory_fts WHERE memoire_memory_fts MATCH 'zanzibar'"
        ),
        0
    );
    assert_eq!(
        count(
            &fake,
            "SELECT count(*) FROM memoire_memory_events WHERE summary = 'zanzibar edit'"
        ),
        0
    );
    assert_eq!(
        count(
            &fake,
            "SELECT count(*) FROM memoire_memory_revisions WHERE title LIKE '%zanzibar%'"
        ),
        0
    );
}

#[test]
fn concurrent_revisions_serialize_without_gaps() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-0");
    ok(&a, &["memory", "init"]);
    let id = id_of(&remember(&a, "Burst target", &[]));
    let writers: Vec<Memoire> = (1..=6)
        .map(|i| agent(&fake, dir.path(), &format!("agent-{i}"), &format!("s-{i}")))
        .collect();
    let codes: Vec<i32> = std::thread::scope(|scope| {
        let handles: Vec<_> = writers
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let id = &id;
                scope.spawn(move || {
                    let title = format!("Burst target revised by writer {i}");
                    let run = w.run(
                        &[
                            "memory", "revise", id, "--title", &title, "--reason", "burst",
                            "--json",
                        ],
                        "",
                    );
                    if run.code == 6 {
                        assert_eq!(
                            run.error()["error"]["code"],
                            "memory_conflict",
                            "{}",
                            run.stderr
                        );
                    }
                    run.code
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(codes.iter().all(|c| *c == 0 || *c == 6), "{codes:?}");
    let successes = codes.iter().filter(|c| **c == 0).count() as i64;
    assert!(successes >= 1);
    let shown = ok(&a, &["memory", "show", &id, "--history"]);
    assert_eq!(shown["memory"]["rev"].as_i64().unwrap(), 1 + successes);
    let revs: Vec<i64> = shown["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["rev"].as_i64().unwrap())
        .collect();
    assert_eq!(revs, (1..=1 + successes).collect::<Vec<_>>());
}

#[test]
fn guards_refuse_secrets_and_oversized_fields_before_writing() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let id = id_of(&remember(&a, "A harmless memory", &[]));
    let before = footprint(&fake);
    let writes_before = fake.requests().len();
    let secret = fake_secret();
    let leaked = format!("token {secret} in text");
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (vec!["memory", "remember", "--title", &leaked], "title"),
        (
            vec!["memory", "remember", "--title", "t", "--body", &leaked],
            "body",
        ),
        (
            vec!["memory", "remember", "--title", "t", "--evidence", &leaked],
            "evidence",
        ),
        (
            vec!["memory", "revise", &id, "--title", "t", "--reason", &leaked],
            "reason",
        ),
        (
            vec![
                "memory",
                "revise",
                &id,
                "--add-evidence",
                &leaked,
                "--reason",
                "r",
            ],
            "evidence",
        ),
        (
            vec!["memory", "--agent", &leaked, "remember", "--title", "t"],
            "agent",
        ),
        (
            vec!["memory", "confirm", &id, "--evidence", &leaked],
            "evidence",
        ),
        (
            vec!["memory", "dispute", &id, "--reason", &leaked],
            "reason",
        ),
        (
            vec![
                "memory",
                "dispute",
                &id,
                "--reason",
                "r",
                "--evidence",
                &leaked,
            ],
            "evidence",
        ),
        (
            vec!["memory", "resolve", &id, "--reason", &leaked],
            "reason",
        ),
        (
            vec!["memory", "supersede", &id, "--by", &id, "--reason", &leaked],
            "reason",
        ),
        (vec!["memory", "forget", &id, "--reason", &leaked], "reason"),
        (
            vec!["memory", "forget", &id, "--reason", &leaked, "--purge"],
            "reason",
        ),
    ];
    for (args, field) in cases {
        let run = a.run(&with_json(&args), "");
        assert_eq!(run.code, 7, "{args:?}: {}", run.stderr);
        let error = &run.error()["error"];
        assert_eq!(error["code"], "memory_secret_detected");
        assert_eq!(error["pattern"], "memoire_api_key");
        assert_eq!(error["field"], field);
        assert!(!run.stderr.contains(&secret) && !run.stdout.contains(&secret));
        let human = a.run(&args, "");
        assert!(!human.stderr.contains(&secret));
    }
    // The guard runs before any request.
    assert_eq!(fake.requests().len(), writes_before);
    assert_eq!(footprint(&fake), before);

    let long_title = "t".repeat(121);
    let long_body = "b".repeat(8193);
    let mut many_tags = vec!["memory", "remember", "--title", "Tags"];
    let tags: Vec<String> = (0..11).map(|i| format!("t{i}")).collect();
    for tag in &tags {
        many_tags.extend(["--tag", tag]);
    }
    let mut many_evidence = vec!["memory", "remember", "--title", "Evidence"];
    for tag in &tags {
        many_evidence.extend(["--evidence", tag]);
    }
    for (args, field, limit) in [
        (
            vec!["memory", "remember", "--title", &long_title],
            "title",
            120,
        ),
        (
            vec!["memory", "remember", "--title", "t", "--body", &long_body],
            "body",
            8192,
        ),
        (many_tags, "tags", 10),
        (many_evidence, "evidence", 10),
    ] {
        let error = refused(&a, &args, 7);
        assert_eq!(error["code"], "memory_too_large", "{field}");
        assert_eq!(error["field"], field);
        assert_eq!(error["limit"], limit);
    }
    // Ten tags already there and one more.
    let mut ten = vec!["memory", "revise", &id, "--reason", "tags"];
    for tag in &tags[..10] {
        ten.extend(["--tag", tag]);
    }
    ok(&a, &ten);
    let error = refused(
        &a,
        &[
            "memory",
            "revise",
            &id,
            "--add-tag",
            "eleventh",
            "--reason",
            "r",
        ],
        7,
    );
    assert_eq!(error["field"], "tags");
    assert_eq!(
        refused(&a, &["memory", "remember", "--title", "   "], 2)["code"],
        "invalid_argument"
    );
    assert_eq!(footprint(&fake)[1], before[1] + 1);
}

/// Fixture for ranking: titles and bodies of one shape, so bm25 scores
/// every title match alike and the order comes from trust and recency.
#[test]
fn recall_ranks_by_relevance_trust_and_recency() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let make = |title: &str| {
        id_of(&remember(
            &a,
            title,
            &["--body", "plain words here only", "--force"],
        ))
    };
    let four = make("Vpn note golf");
    let two = make("Vpn note alpha");
    let one = make("Vpn note bravo");
    let fresh = make("Vpn note charlie");
    let old = make("Vpn note delta");
    let disputed = make("Vpn note echo");
    let body_only = id_of(&remember(
        &a,
        "Plain note foxtrot",
        &["--body", "vpn words here only", "--force"],
    ));
    let unrelated = make("Office plants need water");
    // Enough memories without the term that its IDF stays positive (FTS5
    // floors the IDF of a term in most rows to almost nothing).
    for filler in [
        "Lunch is at noon",
        "Printer on floor two",
        "Badges open the side door",
        "Standup starts at nine",
        "Coffee machine descales on Fridays",
        "Parking spots are shared",
        "Plants get watered Mondays",
        "Wiki moved to the new host",
    ] {
        make(filler);
    }

    // Confirmers are the same key in distinct sessions.
    let checkers: Vec<Memoire> = (0..4)
        .map(|i| agent(&fake, dir.path(), "checker", &format!("c-{i}")))
        .collect();
    let confirm = |checker: &Memoire, id: &str| {
        ok(checker, &["memory", "confirm", id, "--evidence", "checked"]);
    };
    for checker in &checkers {
        confirm(checker, &four);
    }
    confirm(&checkers[0], &four);
    confirm(&checkers[0], &two);
    confirm(&checkers[1], &two);
    confirm(&checkers[1], &two);
    confirm(&checkers[0], &one);
    // The author cannot confirm their own text.
    let error = refused(&a, &["memory", "confirm", &one, "--evidence", "mine"], 7);
    assert_eq!(error["code"], "memory_self_confirm");
    ok(
        &checkers[3],
        &["memory", "dispute", &disputed, "--reason", "wrong"],
    );

    // Ages far apart, so the real clock cannot reorder them. Confirmations
    // and disputes leave `updated_at` alone, so only ages are set here.
    let db = sqlite(&fake, "memory");
    for id in [&four, &two, &one, &fresh, &disputed, &body_only, &unrelated] {
        db.execute(
            "UPDATE memoire_memory_memories SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 days') WHERE id = ?1",
            [id],
        )
        .unwrap();
    }
    db.execute(
        "UPDATE memoire_memory_memories SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-2000 days') WHERE id = ?1",
        [&old],
    )
    .unwrap();

    let found = ok(&a, &["memory", "recall", "VPN"]);
    assert_eq!(found["query"], "VPN");
    assert_eq!(found["db"], "memory");
    let memories = found["memories"].as_array().unwrap();
    let ids: Vec<&str> = memories.iter().map(|m| m["id"].as_str().unwrap()).collect();
    // A body match weighs less than a title match, but bm25 saturates term
    // frequency, so it keeps more than the disputed penalty's half.
    assert_eq!(
        ids,
        [&four, &two, &one, &fresh, &old, &body_only, &disputed]
    );
    let scores: Vec<f64> = memories
        .iter()
        .map(|m| m["score"].as_f64().unwrap())
        .collect();
    assert!(scores.windows(2).all(|w| w[0] > w[1]), "{scores:?}");
    let confirmers: Vec<i64> = memories
        .iter()
        .map(|m| m["confirmers"].as_i64().unwrap())
        .collect();
    assert_eq!(confirmers, [4, 2, 1, 0, 0, 0, 0]);
    assert_eq!(memories[6]["open_disputes"], 1);
    // Trust and recency as documented: equal relevance, so score ratios.
    let ratio = |a: usize, b: usize| scores[a] / scores[b];
    assert!((ratio(0, 3) - 1.6).abs() < 1e-6, "{}", ratio(0, 3));
    assert!((ratio(1, 3) - 1.3).abs() < 1e-6);
    assert!((ratio(6, 3) - 0.5).abs() < 1e-6);
    assert!(ratio(5, 3) < 1.0, "a body match ranks below a title match");
    let recency = |days: f64| 0.75 + 0.25 / (1.0 + days / 90.0);
    assert!(
        (ratio(4, 3) - recency(2000.0) / recency(1.0)).abs() < 1e-4,
        "{}",
        ratio(4, 3)
    );

    let run = a.run(&["memory", "recall", "VPN", "--limit", "2"], "").ok();
    assert!(run.stdout.starts_with("ID "), "{}", run.stdout);
    assert!(
        run.stdout.contains("confirmed ×4  fact  global"),
        "{}",
        run.stdout
    );
    assert_eq!(run.stdout.lines().count(), 4);
    assert!(run.stderr.contains("Searched global"), "{}", run.stderr);
}

#[test]
fn recall_filters_and_lists() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let deploy = id_of(&remember(
        &a,
        "Deploy needs the VPN",
        &["--kind", "procedure", "--tag", "deploy", "--tag", "vpn"],
    ));
    let pitfall = id_of(&remember(
        &a,
        "VPN drops idle sessions",
        &["--kind", "pitfall", "--tag", "vpn"],
    ));
    let client = id_of(&remember(
        &a,
        "Acme VPN uses its own profile",
        &["--scope", "client:acme", "--tag", "vpn"],
    ));
    let replaced = id_of(&remember(&a, "Old VPN gateway address", &["--tag", "vpn"]));
    ok(
        &a,
        &[
            "memory",
            "supersede",
            &replaced,
            "--by",
            &deploy,
            "--reason",
            "moved",
        ],
    );
    let quoted = id_of(&remember(&a, "Title of the quoted runbook", &[]));
    let axis = id_of(&remember(&a, "Z axis calibration", &[]));
    let ids = |value: Value| -> Vec<String> {
        value["memories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_owned())
            .collect()
    };
    let sorted = |mut list: Vec<String>| {
        list.sort();
        list
    };

    assert_eq!(
        sorted(ids(ok(&a, &["memory", "recall", "vpn"]))),
        sorted(vec![deploy.clone(), pitfall.clone()])
    );
    assert_eq!(
        ids(ok(&a, &["memory", "recall", "vpn", "--kind", "pitfall"])),
        [pitfall.as_str()]
    );
    assert_eq!(
        sorted(ids(ok(
            &a,
            &[
                "memory",
                "recall",
                "vpn",
                "--kind",
                "pitfall",
                "--kind",
                "procedure"
            ]
        ))),
        sorted(vec![deploy.clone(), pitfall.clone()])
    );
    assert_eq!(
        ids(ok(
            &a,
            &["memory", "recall", "vpn", "--tag", "vpn", "--tag", "deploy"]
        )),
        [deploy.as_str()]
    );
    assert_eq!(
        ids(ok(
            &a,
            &["memory", "recall", "vpn", "--status", "superseded"]
        )),
        [replaced.as_str()]
    );
    let scoped = ok(&a, &["memory", "recall", "vpn", "--scope", "client:acme"]);
    assert_eq!(scoped["scopes"], json!(["client:acme"]));
    assert_eq!(ids(scoped), [client.as_str()]);
    assert_eq!(
        ids(ok(&a, &["memory", "recall", "vpn", "--all-scopes"])).len(),
        3
    );
    assert_eq!(
        ids(ok(
            &a,
            &["memory", "recall", "vpn", "--all-scopes", "--limit", "1"]
        ))
        .len(),
        1
    );

    // Without a query: newest first, no score.
    let newest = ok(&a, &["memory", "recall", "--all-scopes"]);
    assert!(newest["query"].is_null());
    assert!(newest["memories"][0].get("score").is_none());
    assert_eq!(
        ids(newest),
        // Superseding wrote a revision of `deploy`, so it is newer.
        [
            axis.clone(),
            quoted.clone(),
            deploy.clone(),
            client.clone(),
            pitfall.clone()
        ]
    );

    // Query syntax in the text is just words: `-z` excludes nothing,
    // `title:` names no column, quotes and parentheses are not syntax.
    let hostile = ok(
        &a,
        &["memory", "recall", r#""quoted" AND (x OR y) -z title:*"#],
    );
    assert_eq!(
        sorted(ids(hostile)),
        sorted(vec![quoted.clone(), axis.clone()])
    );
    for text in ["NEAR(vpn drops, 1)", "vpn*", "^vpn", "café", "\"unbalanced"] {
        ok(&a, &["memory", "recall", text]);
    }
    let error = refused(&a, &["memory", "recall", "*** ()"], 2);
    assert_eq!(error["code"], "invalid_query");
    let run = a.run(&["memory", "recall", "nothingmatches"], "").ok();
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains("No memories"), "{}", run.stderr);
}

#[test]
fn the_store_shares_a_database_with_application_tables() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    a.run(&["db", "create", "shop", "--persistent"], "").ok();
    let query = |sql: &str, write: bool| {
        let mut args = vec!["query", "shop", sql, "--json"];
        if write {
            args.push("--write");
        }
        a.run(&args, "").ok().json()
    };
    query(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, item TEXT NOT NULL)",
        true,
    );
    query(
        "INSERT INTO orders (item) VALUES ('widget'), ('gadget')",
        true,
    );
    let init = ok(&a, &["memory", "--db", "shop", "init"]);
    assert_eq!(init["created"], false);
    let memory = remember(&a, "Orders table is append only", &["--db", "shop"]);
    ok(
        &a,
        &[
            "memory",
            "--db",
            "shop",
            "revise",
            &id_of(&memory),
            "--add-tag",
            "orders",
            "--reason",
            "tag",
        ],
    );
    let found = ok(&a, &["memory", "recall", "orders", "--db", "shop"]);
    assert_eq!(found["db"], "shop");
    assert_eq!(found["memories"][0]["id"], memory["id"]);

    assert_eq!(
        query("SELECT id, item FROM orders ORDER BY id", false)["rows"],
        json!([[1, "widget"], [2, "gadget"]])
    );
    assert_eq!(
        query("SELECT count(*) FROM memoire_memory_memories", false)["rows"],
        json!([[1]])
    );
    let tables = query(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'memoire_memory_%' AND name NOT LIKE 'sqlite_%'",
        false,
    );
    assert_eq!(tables["rows"], json!([["orders"]]));
}

#[test]
fn a_read_only_key_recalls_and_shows_but_cannot_write() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let id = id_of(&remember(&a, "Readers can see this", &[]));
    fake.add_key_as(
        "mzk_fake_reader_key",
        "key_fake_reader",
        Some(TEAM),
        &["memoire.databases.read"],
    );
    let reader = agent_with(&fake, dir.path(), "mzk_fake_reader_key", "reader", "r-1");
    let whoami_calls = || {
        fake.requests()
            .iter()
            .filter(|r| r.path.ends_with("/whoami"))
            .count()
    };
    let before = whoami_calls();
    assert_eq!(
        ok(&reader, &["memory", "recall", "readers"])["memories"][0]["id"],
        id
    );
    assert_eq!(
        ok(&reader, &["memory", "show", &id, "--history"])["memory"]["id"],
        id
    );
    assert_eq!(
        whoami_calls(),
        before,
        "read verbs never ask who is calling"
    );
    let error = refused(
        &reader,
        &["memory", "remember", "--title", "Readers cannot write"],
        4,
    );
    assert_eq!(error["code"], "unauthorized");
    refused(
        &reader,
        &["memory", "revise", &id, "--title", "No", "--reason", "try"],
        4,
    );

    // The history verbs only read too.
    let before = whoami_calls();
    assert_eq!(verbs_of(&ok(&reader, &["memory", "log"])), ["remember"]);
    assert_eq!(
        verbs_of(&ok(&reader, &["memory", "log", "--unseen"])),
        ["remember"]
    );
    let output = dir.path().join("reader.jsonl");
    assert_eq!(
        ok(
            &reader,
            &["memory", "export", "-o", output.to_str().unwrap()]
        )["count"],
        1
    );
    assert_eq!(whoami_calls(), before);
    let other = id_of(&remember(&a, "Another memory for readers", &[]));
    ok(&a, &["memory", "dispute", &id, "--reason", "open one"]);
    for verb in [
        &["memory", "confirm", id.as_str(), "--evidence", "e"][..],
        &["memory", "dispute", &id, "--reason", "r"],
        &["memory", "resolve", &id, "--reason", "r"],
        &["memory", "supersede", &id, "--by", &other, "--reason", "r"],
        &["memory", "forget", &id, "--reason", "r"],
        &["memory", "forget", &id, "--reason", "r", "--purge"],
    ] {
        assert_eq!(
            refused(&reader, verb, 4)["code"],
            "unauthorized",
            "{verb:?}"
        );
    }
    let shown = show(&a, &id);
    assert_eq!(shown["memory"]["rev"], 1);
    assert_eq!(shown["disputes"].as_array().unwrap().len(), 1);
    assert_eq!(shown["confirmations"], json!([]));
}

/// A second key of the team, with read and write, and its principal.
const SECOND_KEY: &str = "mzk_fake_second_key";
const SECOND: &str = "key_fake_second";

fn second_key(fake: &Fake) {
    fake.add_key_as(
        SECOND_KEY,
        SECOND,
        Some(TEAM),
        &["memoire.databases.read", "memoire.databases.write"],
    );
}

fn confirm(memoire: &Memoire, id: &str) -> Value {
    ok(memoire, &["memory", "confirm", id, "--evidence", "checked"])
}

fn show(memoire: &Memoire, id: &str) -> Value {
    ok(memoire, &["memory", "show", id, "--history"])
}

#[test]
fn confirmations_count_once_per_principal_and_session() {
    let (fake, dir) = world();
    second_key(&fake);
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    let a_elsewhere = agent(&fake, dir.path(), "agent-a", "s-2");
    // Same session name, another principal: another confirmer.
    let b = agent_with(&fake, dir.path(), SECOND_KEY, "agent-b", "s-1");
    ok(&a, &["memory", "init"]);
    let id = id_of(&remember(&a, "Nightly backups finish by six", &[]));

    let error = refused(&a, &["memory", "confirm", &id, "--evidence", "mine"], 7);
    assert_eq!(error["code"], "memory_self_confirm");
    assert_eq!(
        count(&fake, "SELECT count(*) FROM memoire_memory_confirmations"),
        0
    );

    let first = confirm(&a_elsewhere, &id);
    assert_eq!(
        (first["status"].clone(), first["confirmers"].clone()),
        (json!("confirmed"), json!(1))
    );
    assert_eq!(first["rev"], 1, "a confirmation writes no revision");
    let second = confirm(&b, &id);
    assert_eq!(second["confirmers"], 2);
    let again = confirm(&b, &id);
    assert_eq!(
        again["confirmers"], 2,
        "a repeat by the same pair counts once"
    );

    let shown = show(&a, &id);
    let confirmations = shown["confirmations"].as_array().unwrap();
    assert_eq!(confirmations.len(), 3);
    assert!(
        confirmations
            .iter()
            .all(|c| c["counts"] == true && c["rev"] == 1)
    );
    assert_eq!(confirmations[1]["principal"], SECOND);
    assert_eq!(confirmations[1]["evidence"], json!(["checked"]));
    assert_eq!(shown["revisions"].as_array().unwrap().len(), 1);
    assert_eq!(
        count(
            &fake,
            "SELECT count(*) FROM memoire_memory_events WHERE verb = 'confirm' AND summary = 'confirmed rev 1'"
        ),
        3
    );
    let run = a.run(&["memory", "confirm", &id], "");
    assert_eq!(run.code, 2, "{}", run.stderr);
    let run = a.run(&["memory", "show", &id], "").ok();
    assert!(
        run.stdout.contains("status         confirmed ×2\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("Confirmations\nREV  BY"),
        "{}",
        run.stdout
    );
}

#[test]
fn the_content_author_is_who_wrote_the_text() {
    let (fake, dir) = world();
    second_key(&fake);
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    let b = agent_with(&fake, dir.path(), SECOND_KEY, "agent-b", "s-2");
    ok(&a, &["memory", "init"]);
    let id = id_of(&remember(&a, "Migrations run in one transaction", &[]));
    confirm(&b, &id);

    // B retags A's text: B wrote the latest revision, A still wrote the text.
    let tagged = ok(
        &b,
        &[
            "memory",
            "revise",
            &id,
            "--add-tag",
            "db",
            "--reason",
            "tag",
        ],
    );
    assert_eq!(tagged["author"]["principal"], SECOND);
    assert_eq!(tagged["status"], "confirmed");
    assert_eq!(tagged["confirmers"], 1, "B's confirmation still counts");
    let error = refused(&a, &["memory", "confirm", &id, "--evidence", "mine"], 7);
    assert_eq!(error["code"], "memory_self_confirm");

    // New text, by B: the old confirmations no longer count.
    let rewritten = ok(
        &b,
        &[
            "memory",
            "revise",
            &id,
            "--body",
            "Each migration file is one transaction.",
            "--reason",
            "detail",
        ],
    );
    assert_eq!(rewritten["status"], "inferred");
    assert_eq!(rewritten["confirmers"], 0);
    let shown = show(&a, &id);
    assert_eq!(shown["confirmations"][0]["counts"], false);
    assert_eq!(shown["confirmations"][0]["rev"], 1);
    // Now B is the content author, and A may confirm.
    refused(&b, &["memory", "confirm", &id, "--evidence", "mine"], 7);
    let confirmed = confirm(&a, &id);
    assert_eq!(
        (confirmed["status"].clone(), confirmed["confirmers"].clone()),
        (json!("confirmed"), json!(1))
    );
}

#[test]
fn disputes_flag_a_memory_until_resolved() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    let c = agent(&fake, dir.path(), "agent-c", "s-3");
    let d = agent(&fake, dir.path(), "agent-d", "s-4");
    ok(&a, &["memory", "init"]);
    let id = id_of(&remember(&a, "The staging cluster has three nodes", &[]));
    confirm(&c, &id);

    let disputed = ok(
        &d,
        &[
            "memory",
            "dispute",
            &id,
            "--reason",
            "it has four now",
            "--evidence",
            "kubectl get nodes",
        ],
    );
    assert_eq!(disputed["status"], "disputed");
    assert_eq!(disputed["open_disputes"], 1);
    let found = ok(&a, &["memory", "recall", "staging cluster"]);
    assert_eq!(found["memories"][0]["id"], id);
    assert_eq!(found["memories"][0]["status"], "disputed");
    let still = confirm(&d, &id);
    assert_eq!(
        still["status"], "disputed",
        "a confirmation does not close a dispute"
    );
    assert_eq!(still["confirmers"], 2);

    let resolved = ok(
        &c,
        &["memory", "resolve", &id, "--reason", "checked, three"],
    );
    assert_eq!(resolved["status"], "confirmed");
    assert_eq!(resolved["open_disputes"], 0);
    let shown = show(&a, &id);
    let dispute = &shown["disputes"][0];
    assert_eq!(dispute["reason"], "it has four now");
    assert_eq!(dispute["evidence"], json!(["kubectl get nodes"]));
    assert_eq!(dispute["resolution"], "checked, three");
    assert!(dispute["resolved_at"].is_string());
    let error = refused(&c, &["memory", "resolve", &id, "--reason", "again"], 6);
    assert_eq!(error["code"], "memory_no_open_dispute");

    // New text while disputed stays disputed, then resolves to inferred.
    ok(&d, &["memory", "dispute", &id, "--reason", "four again"]);
    let revised = ok(
        &a,
        &[
            "memory",
            "revise",
            &id,
            "--title",
            "The staging cluster has four nodes",
            "--reason",
            "grew",
        ],
    );
    assert_eq!(revised["status"], "disputed");
    assert_eq!(show(&a, &id)["revisions"][1]["status"], "disputed");
    let resolved = ok(&c, &["memory", "resolve", &id, "--reason", "text fixed"]);
    assert_eq!(resolved["status"], "inferred");
    let events: Vec<String> = ok(&a, &["memory", "log"])["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["verb"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        events,
        [
            "resolve", "revise", "dispute", "resolve", "confirm", "dispute", "confirm", "remember"
        ]
    );
}

#[test]
fn supersede_links_both_memories_and_refuses_cycles() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let old = id_of(&remember(&a, "Builds run on Jenkins", &[]));
    let new = id_of(&remember(&a, "Builds run on GitHub Actions", &[]));
    let done = ok(
        &a,
        &[
            "memory",
            "supersede",
            &old,
            "--by",
            &new,
            "--reason",
            "migrated",
        ],
    );
    assert_eq!(done["superseded"]["id"], old);
    assert_eq!(done["superseded"]["status"], "superseded");
    assert_eq!(done["superseded"]["superseded_by"], new);
    assert_eq!(done["superseded"]["rev"], 2);
    assert_eq!(done["by"]["id"], new);
    assert_eq!(done["by"]["supersedes"], old);
    assert_eq!(done["by"]["status"], "inferred");
    assert_eq!(done["by"]["rev"], 2);
    let history = show(&a, &old);
    assert_eq!(history["revisions"][1]["status"], "superseded");
    assert_eq!(history["revisions"][1]["reason"], "migrated");
    assert_eq!(show(&a, &new)["revisions"][1]["supersedes"], old);
    let summary = &ok(&a, &["memory", "log", "--limit", "1"])["events"][0];
    assert_eq!(summary["verb"], "supersede");
    assert_eq!(summary["summary"], format!("by {new}: migrated"));

    let recalled = |args: &[&str]| -> Vec<String> {
        let mut all = vec!["memory", "recall", "builds"];
        all.extend_from_slice(args);
        ok(&a, &all)["memories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(recalled(&[]), [new.as_str()]);
    assert_eq!(recalled(&["--status", "superseded"]), [old.as_str()]);

    let code = |args: &[&str], exit: i32| refused(&a, args, exit)["code"].clone();
    assert_eq!(
        code(
            &[
                "memory",
                "supersede",
                &new,
                "--by",
                &old,
                "--reason",
                "back"
            ],
            6
        ),
        "memory_cycle"
    );
    assert_eq!(
        code(
            &[
                "memory",
                "supersede",
                &new,
                "--by",
                &new,
                "--reason",
                "self"
            ],
            6
        ),
        "memory_cycle"
    );
    let other = id_of(&remember(&a, "Release notes go in the changelog", &[]));
    let error = refused(
        &a,
        &[
            "memory",
            "supersede",
            &old,
            "--by",
            &other,
            "--reason",
            "twice",
        ],
        6,
    );
    assert_eq!(error["code"], "memory_not_current");
    assert_eq!(error["superseded_by"], new);
    let gone = id_of(&remember(&a, "Temporary note to forget", &[]));
    ok(&a, &["memory", "forget", &gone, "--reason", "obsolete"]);
    let error = refused(
        &a,
        &[
            "memory",
            "supersede",
            &other,
            "--by",
            &gone,
            "--reason",
            "x",
        ],
        6,
    );
    assert_eq!(
        (
            error["code"].clone(),
            error["id"].clone(),
            error["status"].clone()
        ),
        (json!("memory_not_current"), json!(gone), json!("forgotten"))
    );

    // A by B, B by C, then C by A closes a longer cycle.
    let [x, y, z] = ["Chain alpha one", "Chain bravo two", "Chain charlie three"]
        .map(|t| id_of(&remember(&a, t, &[])));
    ok(
        &a,
        &["memory", "supersede", &x, "--by", &y, "--reason", "1"],
    );
    ok(
        &a,
        &["memory", "supersede", &y, "--by", &z, "--reason", "2"],
    );
    assert_eq!(
        code(&["memory", "supersede", &z, "--by", &x, "--reason", "3"], 6),
        "memory_cycle"
    );

    for verb in [
        &[
            "memory",
            "revise",
            old.as_str(),
            "--title",
            "x",
            "--reason",
            "y",
        ][..],
        &["memory", "confirm", &old, "--evidence", "e"],
    ] {
        let error = refused(&a, verb, 6);
        assert_eq!(error["code"], "memory_not_current");
        assert_eq!(error["superseded_by"], new);
    }
    let run = a
        .run(
            &[
                "memory",
                "supersede",
                &other,
                "--by",
                &new,
                "--reason",
                "merge",
            ],
            "",
        )
        .ok();
    assert!(
        run.stdout
            .starts_with(&format!("Superseded {other}  Release notes")),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout
            .contains(&format!("\nby         {new}  Builds run on GitHub Actions")),
        "{}",
        run.stdout
    );
}

#[test]
fn forget_leaves_a_tombstone() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let id = id_of(&remember(&a, "Temporary hostname is tmp-seven", &[]));
    let forgotten = ok(&a, &["memory", "forget", &id, "--reason", "decommissioned"]);
    assert_eq!(forgotten["status"], "forgotten");
    assert_eq!(forgotten["rev"], 2);
    assert_eq!(
        ok(&a, &["memory", "recall", "hostname"])["memories"],
        json!([])
    );
    let found = ok(
        &a,
        &["memory", "recall", "hostname", "--status", "forgotten"],
    );
    assert_eq!(found["memories"][0]["id"], id);
    assert_eq!(show(&a, &id)["memory"]["status"], "forgotten");
    let event = &ok(&a, &["memory", "log"])["events"][0];
    assert_eq!(
        (
            event["verb"].clone(),
            event["summary"].clone(),
            event["rev"].clone()
        ),
        (json!("forget"), json!("decommissioned"), json!(2))
    );
    for verb in [
        &["memory", "forget", id.as_str(), "--reason", "again"][..],
        &["memory", "dispute", &id, "--reason", "r"],
        &["memory", "resolve", &id, "--reason", "r"],
        &["memory", "revise", &id, "--title", "t", "--reason", "r"],
    ] {
        assert_eq!(
            refused(&a, verb, 6)["code"],
            "memory_not_current",
            "{verb:?}"
        );
    }
}

/// Every value of every table of the database file, as lowercase bytes.
fn dump(fake: &Fake) -> Vec<(String, Vec<u8>)> {
    let db = sqlite(fake, "memory");
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut values = Vec::new();
    for table in tables {
        let mut statement = db.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
        let columns = statement.column_count();
        let mut rows = statement.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for i in 0..columns {
                let bytes = match row.get_ref(i).unwrap() {
                    rusqlite::types::ValueRef::Text(b) | rusqlite::types::ValueRef::Blob(b) => {
                        b.to_ascii_lowercase()
                    }
                    _ => continue,
                };
                values.push((table.clone(), bytes));
            }
        }
    }
    values
}

/// Every term the search index holds, from FTS5's own vocabulary, so the
/// answer does not depend on how the index pages compress terms.
fn vocabulary(fake: &Fake) -> Vec<String> {
    let db = sqlite(fake, "memory");
    db.execute_batch(
        "CREATE VIRTUAL TABLE temp.vocab USING fts5vocab('main', 'memoire_memory_fts', 'row')",
    )
    .unwrap();
    db.prepare("SELECT term FROM temp.vocab ORDER BY term")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Whether `terms` holds `word` itself or a term it starts.
fn indexed(terms: &[String], word: &str) -> bool {
    terms.iter().any(|t| t.starts_with(word))
}

#[test]
fn purge_leaves_no_trace_of_the_memory() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    let c = agent(&fake, dir.path(), "agent-c", "s-3");
    ok(&a, &["memory", "init"]);
    let keep = id_of(&remember(&a, "Unrelated memory stays", &["--body", "kept"]));
    let older = id_of(&remember(&a, "Older location note", &[]));
    let leaked = id_of(&remember(
        &a,
        "Zorblax deploy key location",
        &[
            "--body",
            "fnordik is where it lives",
            "--tag",
            "grabthar",
            "--evidence",
            "notes/plughk.md",
        ],
    ));
    ok(
        &c,
        &["memory", "confirm", &leaked, "--evidence", "frobnoz run"],
    );
    ok(
        &c,
        &["memory", "dispute", &leaked, "--reason", "wibblek is wrong"],
    );
    ok(
        &a,
        &[
            "memory",
            "revise",
            &leaked,
            "--add-tag",
            "other",
            "--reason",
            "quuxly cleanup",
        ],
    );
    ok(
        &a,
        &[
            "memory",
            "supersede",
            &older,
            "--by",
            &leaked,
            "--reason",
            "the zorblax note says it better",
        ],
    );
    // Reasons quote what they link, both ways: X replaced something, and
    // something replaces X.
    let newest = id_of(&remember(&a, "Newest location note", &[]));
    ok(
        &a,
        &[
            "memory",
            "supersede",
            &leaked,
            "--by",
            &newest,
            "--reason",
            "fnordik moved here",
        ],
    );
    // A second memory whose first text a revise replaced before the purge.
    let revised = id_of(&remember(
        &a,
        "Blorptik relay setting",
        &["--body", "snarfwex holds the value"],
    ));
    ok(
        &a,
        &[
            "memory",
            "revise",
            &revised,
            "--title",
            "Relay setting moved",
            "--body",
            "the value lives elsewhere now",
            "--reason",
            "moved",
        ],
    );
    // The vocabulary drops replaced terms at once; their bytes may stay in
    // the index segments until a merge, which only a purge forces.
    let terms = vocabulary(&fake);
    for word in ["blorptik", "snarfwex"] {
        assert!(
            !indexed(&terms, word),
            "{word} is no longer a search term after the revise"
        );
    }

    // Words the porter tokenizer keeps whole, one term each, for the
    // fields the index holds: title, body, tag.
    let indexed_sentinels = ["zorblax", "fnordik", "grabthar"];
    for word in indexed_sentinels {
        assert!(
            terms.iter().any(|t| t == word),
            "{word} is in the index: {terms:?}"
        );
    }
    let sentinels = [
        "zorblax", "fnordik", "grabthar", "plughk", "frobnoz", "wibblek", "quuxly", "blorptik",
        "snarfwex",
    ];
    let found = |values: &[(String, Vec<u8>)], word: &str| {
        values
            .iter()
            .find(|(_, bytes)| bytes.windows(word.len()).any(|w| w == word.as_bytes()))
            .map(|(t, _)| t.clone())
    };
    let before = dump(&fake);
    for word in sentinels {
        assert!(found(&before, word).is_some(), "{word} was written");
    }

    let purged = ok(
        &a,
        &[
            "memory",
            "forget",
            &leaked,
            "--reason",
            "grabthar leaked a path",
            "--purge",
        ],
    );
    assert_eq!(purged, json!({"id": leaked, "purged": true}));
    ok(
        &a,
        &[
            "memory",
            "forget",
            &revised,
            "--reason",
            "also leaked",
            "--purge",
        ],
    );
    let terms = vocabulary(&fake);
    for word in indexed_sentinels.iter().chain(&["blorptik", "snarfwex"]) {
        assert!(!terms.iter().any(|t| t == word), "{word} left in the index");
        assert!(
            !indexed(&terms, word),
            "a term starting {word} left in the index"
        );
    }
    for word in ["kept", "older"] {
        assert!(
            indexed(&terms, word),
            "{word} of a kept memory is still indexed"
        );
    }
    let after = dump(&fake);
    for word in sentinels {
        assert_eq!(found(&after, word), None, "{word} survived");
    }
    let db = sqlite(&fake, "memory");
    type EventRow = (String, Option<i64>, Option<String>, Option<String>);
    let events: Vec<EventRow> = db
        .prepare("SELECT verb, rev, scope, summary FROM memoire_memory_events WHERE memory_id = ?1")
        .unwrap()
        .query_map([&leaked], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        events,
        [("purge".to_owned(), None, None, None)],
        "the purge keeps no caller text"
    );
    // What other memories said about linking to it is cut to the link.
    let summary: String = db
        .query_row(
            "SELECT summary FROM memoire_memory_events WHERE memory_id = ?1 AND verb = 'supersede'",
            [&older],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(summary, format!("by {leaked}"));
    let reasons: Vec<(String, i64, String)> = db
        .prepare(
            "SELECT memory_id, rev, reason FROM memoire_memory_revisions \
             WHERE memory_id IN (?1, ?2) ORDER BY memory_id, rev",
        )
        .unwrap()
        .query_map([&older, &newest], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut expected = vec![
        (older.clone(), 1, "remembered".to_owned()),
        (older.clone(), 2, "purged".to_owned()),
        (newest.clone(), 1, "remembered".to_owned()),
        (newest.clone(), 2, "purged".to_owned()),
    ];
    expected.sort();
    assert_eq!(reasons, expected);
    let newest_now = show(&a, &newest)["memory"].clone();
    assert_eq!(newest_now["supersedes"], Value::Null);
    assert_eq!(newest_now["status"], "inferred");
    let older = show(&a, &older)["memory"].clone();
    assert_eq!(
        older["status"], "inferred",
        "no longer superseded by a memory that is gone"
    );
    assert_eq!(older["superseded_by"], Value::Null);
    assert_eq!(show(&a, &keep)["memory"]["body"], "kept");

    let error = refused(&a, &["memory", "show", &leaked], 5);
    assert_eq!(error["code"], "memory_not_found");
    assert!(
        error["message"].as_str().unwrap().contains("was purged at"),
        "{error}"
    );
    let error = refused(
        &a,
        &[
            "memory",
            "forget",
            "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8",
            "--reason",
            "x",
            "--purge",
        ],
        5,
    );
    assert_eq!(error["code"], "memory_not_found");
    let run = a
        .run(
            &["memory", "forget", &keep, "--reason", "done", "--purge"],
            "",
        )
        .ok();
    assert_eq!(run.stdout, format!("Purged {keep} and its history\n"));
}

#[test]
fn a_stale_writer_cannot_bring_back_a_purged_memory() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    // Another session than the author's, so it may confirm.
    let c = agent(&fake, dir.path(), "agent-c", "s-3");
    ok(&a, &["memory", "init"]);
    for verb in ["revise", "confirm"] {
        let b = agent(&fake, dir.path(), "agent-b", "s-2");
        let id = id_of(&remember(&a, &format!("Shortlived {verb} kumquat"), &[]));
        let racing = id.clone();
        let (sender, receiver) = std::sync::mpsc::channel::<Run>();
        fake.before_next_write(move || {
            sender
                .send(b.run(
                    &[
                        "memory", "forget", &racing, "--reason", "leak", "--purge", "--json",
                    ],
                    "",
                ))
                .unwrap();
        });
        let args: Vec<&str> = if verb == "revise" {
            vec![
                "memory",
                "revise",
                &id,
                "--title",
                "Resurrected kumquat",
                "--reason",
                "edit",
            ]
        } else {
            vec!["memory", "confirm", &id, "--evidence", "kumquat check"]
        };
        let error = refused(&c, &args, 5);
        receiver.recv().unwrap().ok();
        assert_eq!(error["code"], "memory_not_found", "{verb}");
        assert!(
            error["message"].as_str().unwrap().contains("purged"),
            "{error}"
        );
        for table in [
            "memoire_memory_revisions",
            "memoire_memory_confirmations",
            "memoire_memory_disputes",
        ] {
            assert_eq!(
                count(
                    &fake,
                    &format!("SELECT count(*) FROM {table} WHERE memory_id = '{id}'")
                ),
                0,
                "{verb} {table}"
            );
        }
        assert_eq!(
            count(
                &fake,
                &format!("SELECT count(*) FROM memoire_memory_memories WHERE id = '{id}'")
            ),
            0
        );
        assert_eq!(
            count(
                &fake,
                "SELECT count(*) FROM memoire_memory_fts WHERE memoire_memory_fts MATCH 'kumquat'"
            ),
            0
        );
    }
}

fn verbs_of(log: &Value) -> Vec<String> {
    log["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["verb"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn log_lists_events_and_unseen_reads_each_once() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    let b = agent(&fake, dir.path(), "agent-b", "s-2");
    ok(&a, &["memory", "init"]);
    let first = id_of(&remember(&a, "Logs rotate daily", &[]));
    remember(&a, "Acme wants weekly reports", &["--scope", "client:acme"]);
    ok(
        &a,
        &[
            "memory",
            "revise",
            &first,
            "--add-tag",
            "ops",
            "--reason",
            "tag",
        ],
    );

    let log = ok(&a, &["memory", "log"]);
    assert_eq!(verbs_of(&log), ["revise", "remember", "remember"]);
    let seqs: Vec<i64> = log["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["seq"].as_i64().unwrap())
        .collect();
    assert!(seqs.windows(2).all(|w| w[0] > w[1]), "{seqs:?}");
    let event = &log["events"][0];
    assert_eq!(event["memory_id"], first);
    assert_eq!(event["rev"], 2);
    assert_eq!(event["scope"], "global");
    assert_eq!(
        (
            event["principal"].clone(),
            event["agent"].clone(),
            event["session"].clone()
        ),
        (json!("key_fake_member"), json!("agent-a"), json!("s-1"))
    );
    assert_eq!(event["summary"], "tag");
    let scoped = ok(&a, &["memory", "log", "--scope", "client:acme"]);
    assert_eq!(scoped["events"].as_array().unwrap().len(), 1);
    assert_eq!(scoped["events"][0]["summary"], "Acme wants weekly reports");
    assert_eq!(
        ok(&a, &["memory", "log", "--limit", "1"])["events"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    sqlite(&fake, "memory")
        .execute("UPDATE memoire_memory_events SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-2 days') WHERE seq = ?1", [seqs[2]])
        .unwrap();
    assert_eq!(
        verbs_of(&ok(&a, &["memory", "log", "--since", "24h"])),
        ["revise", "remember"]
    );
    assert_eq!(
        verbs_of(&ok(&a, &["memory", "log", "--since", "3d"])).len(),
        3
    );
    let run = a.run(&["memory", "log"], "").ok();
    assert!(run.stdout.starts_with("SEQ  WHEN"), "{}", run.stdout);
    assert!(run.stdout.contains("  revise    "), "{}", run.stdout);

    let unseen = |memoire: &Memoire, extra: &[&str]| {
        let mut args = vec!["memory", "log", "--unseen"];
        args.extend_from_slice(extra);
        ok(memoire, &args)
    };
    let all = unseen(&a, &[]);
    assert_eq!(verbs_of(&all).len(), 3);
    assert_eq!(
        all["unseen"],
        json!({"after": 0, "through": seqs[0], "skipped": 0})
    );
    let none = unseen(&a, &[]);
    assert_eq!(verbs_of(&none), Vec::<String>::new());
    assert_eq!(
        none["unseen"],
        json!({"after": seqs[0], "through": seqs[0], "skipped": 0})
    );
    let path = a.config.path().join("memoire/memory-cursors.json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
    }
    let file: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(file["version"], 1);
    assert_eq!(
        file["cursors"][format!("{} memory", fake.url)]["seq"],
        seqs[0]
    );

    // What a second agent writes shows once.
    remember(
        &b,
        "Acme invoices go out monthly",
        &["--scope", "client:acme"],
    );
    // A filtered read keeps its own cursor and leaves the unfiltered one.
    let acme = unseen(&a, &["--scope", "client:acme"]);
    assert_eq!(verbs_of(&acme), ["remember", "remember"]);
    assert_eq!(acme["unseen"]["after"], 0);
    assert!(file_has_key(
        &path,
        &format!("{} memory client:acme", fake.url)
    ));
    let news = unseen(&a, &[]);
    assert_eq!(verbs_of(&news), ["remember"]);
    assert_eq!(news["events"][0]["agent"], "agent-b");
    assert_eq!(verbs_of(&unseen(&a, &["--scope", "client:acme"])).len(), 0);

    // --limit keeps the newest and says how many older ones it skipped.
    for title in ["One more", "Two more", "Three more"] {
        remember(&b, &format!("{title} memory for the log"), &["--force"]);
    }
    let cut = unseen(&a, &["--limit", "1"]);
    assert_eq!(cut["events"][0]["summary"], "Three more memory for the log");
    assert_eq!(cut["unseen"]["skipped"], 2);
    assert_eq!(verbs_of(&unseen(&a, &[])).len(), 0);
    let run = a.run(&["memory", "log", "--unseen"], "").ok();
    assert!(run.stderr.contains("No new events."), "{}", run.stderr);

    // A database created again under the name is a new store.
    a.run(&["db", "rm", "memory"], "").ok();
    ok(&a, &["memory", "init"]);
    remember(&a, "Fresh store memory", &[]);
    let fresh = unseen(&a, &[]);
    assert_eq!(verbs_of(&fresh), ["remember"]);
    assert_eq!(fresh["unseen"]["after"], 0);

    std::fs::write(&path, "not json").unwrap();
    assert_eq!(
        refused(&a, &["memory", "log", "--unseen"], 1)["code"],
        "invalid_cursors"
    );
}

fn file_has_key(path: &Path, key: &str) -> bool {
    let file: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    file["cursors"].get(key).is_some()
}

#[test]
fn export_writes_one_memory_per_line() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let first = id_of(&remember(&a, "Exported fact", &["--tag", "x"]));
    let acme = id_of(&remember(&a, "Acme fact", &["--scope", "client:acme"]));
    let replaced = id_of(&remember(&a, "Replaced fact", &[]));
    ok(
        &a,
        &[
            "memory",
            "supersede",
            &replaced,
            "--by",
            &first,
            "--reason",
            "merged",
        ],
    );
    let gone = id_of(&remember(&a, "Forgotten fact", &[]));
    ok(&a, &["memory", "forget", &gone, "--reason", "wrong"]);

    let target = dir.path().join("memories.jsonl");
    std::fs::write(&target, "stale contents\n").unwrap();
    let target_text = target.to_str().unwrap();
    let done = ok(&a, &["memory", "export", "-o", target_text]);
    assert_eq!(
        done,
        json!({"path": target_text, "count": 3, "scopes": null})
    );
    let lines: Vec<Value> = std::fs::read_to_string(&target)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let ids: Vec<&str> = lines.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [&first, &acme, &replaced]);
    assert_eq!(lines[0], show(&a, &first)["memory"]);
    assert_eq!(lines[2]["status"], "superseded");
    assert!(lines.iter().all(|m| m.get("score").is_none()));
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(leftovers, ["memories.jsonl"], "no temporary file is left");

    let scoped = ok(
        &a,
        &[
            "memory",
            "export",
            "--scope",
            "client:acme",
            "-o",
            target_text,
        ],
    );
    assert_eq!(
        (scoped["count"].clone(), scoped["scopes"].clone()),
        (json!(1), json!(["client:acme"]))
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap().lines().count(), 1);

    // Pages of 500: a bulk of rows written straight into the file.
    sqlite(&fake, "memory")
        .execute_batch(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1100)
             INSERT INTO memoire_memory_memories (id, rev, content_rev, content_principal, content_session,
               kind, scope, title, body, tags, evidence, status, author_principal, author_agent,
               author_session, created_at, updated_at)
             SELECT printf('mem_%026d', i), 1, 1, 'key_bulk', '', 'fact', 'run:bulk', 'Bulk ' || i, '',
               '[]', '[]', 'inferred', 'key_bulk', '', '', '2026-11-19T10:00:00.000Z', '2026-11-19T10:00:00.000Z'
             FROM n",
        )
        .unwrap();
    let bulk = ok(&a, &["memory", "export", "-o", target_text]);
    assert_eq!(bulk["count"], 1103);
    let text = std::fs::read_to_string(&target).unwrap();
    let titles: Vec<String> = text
        .lines()
        .skip(3)
        .map(|l| {
            serde_json::from_str::<Value>(l).unwrap()["title"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(titles.len(), 1100);
    assert_eq!(titles[0], "Bulk 1");
    assert_eq!(titles[1099], "Bulk 1100");
    let run = a
        .run(
            &[
                "memory",
                "export",
                "--scope",
                "client:acme",
                "-o",
                target_text,
            ],
            "",
        )
        .ok();
    assert_eq!(run.stdout, format!("Exported 1 memory to {target_text}\n"));

    let error = refused(
        &a,
        &["memory", "--db", "absent", "export", "-o", target_text],
        5,
    );
    assert_eq!(error["code"], "database_not_found");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap().lines().count(),
        1,
        "a failed export keeps the old file"
    );
}

/// Confirmations and disputes: the rows written for memory `id`.
fn verdicts(fake: &Fake, id: &str) -> (i64, i64) {
    (
        count(
            fake,
            &format!("SELECT count(*) FROM memoire_memory_confirmations WHERE memory_id = '{id}'"),
        ),
        count(
            fake,
            &format!("SELECT count(*) FROM memoire_memory_disputes WHERE memory_id = '{id}'"),
        ),
    )
}

#[test]
fn confirm_and_dispute_judge_only_the_text_that_was_read() {
    let (fake, dir) = world();
    second_key(&fake);
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    let b = agent_with(&fake, dir.path(), SECOND_KEY, "agent-b", "s-2");
    let c = agent(&fake, dir.path(), "agent-c", "s-3");
    ok(&a, &["memory", "init"]);
    let id = id_of(&remember(&a, "Cron jobs run in UTC", &[]));

    // A retag by someone else leaves the text of rev 1 standing.
    ok(
        &b,
        &[
            "memory",
            "revise",
            &id,
            "--add-tag",
            "cron",
            "--reason",
            "tag",
        ],
    );
    let confirmed = ok(
        &c,
        &[
            "memory",
            "confirm",
            &id,
            "--evidence",
            "crontab",
            "--rev",
            "1",
        ],
    );
    assert_eq!(
        (confirmed["status"].clone(), confirmed["rev"].clone()),
        (json!("confirmed"), json!(2))
    );
    // Recorded on the revision C checked, not the retag after it.
    assert_eq!(show(&a, &id)["confirmations"][0]["rev"], 1);
    assert_eq!(
        count(
            &fake,
            &format!(
                "SELECT count(*) FROM memoire_memory_events WHERE memory_id = '{id}' AND summary = 'confirmed rev 1'"
            )
        ),
        1
    );

    // New text: a verdict on an older rev is refused and writes nothing.
    ok(
        &a,
        &[
            "memory",
            "revise",
            &id,
            "--body",
            "Except the backup job.",
            "--reason",
            "detail",
        ],
    );
    let before = verdicts(&fake, &id);
    for args in [
        &[
            "memory",
            "confirm",
            id.as_str(),
            "--evidence",
            "crontab",
            "--rev",
            "2",
        ][..],
        &[
            "memory", "dispute", &id, "--reason", "not UTC", "--rev", "1",
        ],
    ] {
        let error = refused(&c, args, 6);
        assert_eq!(error["code"], "memory_conflict", "{args:?}");
        assert_eq!(error["id"], id);
        assert_eq!(error["current_rev"], 3);
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("changed since rev"),
            "{error}"
        );
    }
    assert_eq!(verdicts(&fake, &id), before);
    for args in [
        &[
            "memory",
            "confirm",
            id.as_str(),
            "--evidence",
            "e",
            "--rev",
            "4",
        ][..],
        &["memory", "dispute", &id, "--reason", "r", "--rev", "9"],
    ] {
        let error = refused(&c, args, 2);
        assert_eq!(error["code"], "invalid_argument");
        assert!(
            error["message"].as_str().unwrap().contains("current rev 3"),
            "{error}"
        );
    }
    assert_eq!(verdicts(&fake, &id), before);
    let current = ok(
        &c,
        &[
            "memory",
            "confirm",
            &id,
            "--evidence",
            "crontab",
            "--rev",
            "3",
        ],
    );
    assert_eq!(current["confirmers"], 1);

    // B rewrites the text between C's read and C's write.
    let raced = id_of(&remember(&a, "Queue workers restart nightly", &[]));
    let racing = raced.clone();
    let (sender, receiver) = std::sync::mpsc::channel::<Run>();
    fake.before_next_write(move || {
        sender
            .send(b.run(
                &[
                    "memory",
                    "revise",
                    &racing,
                    "--title",
                    "Queue workers restart hourly",
                    "--reason",
                    "changed",
                    "--json",
                ],
                "",
            ))
            .unwrap();
    });
    let error = refused(
        &c,
        &["memory", "confirm", &raced, "--evidence", "saw it"],
        6,
    );
    assert_eq!(receiver.recv().unwrap().ok().json()["rev"], 2);
    assert_eq!(error["code"], "memory_conflict");
    assert_eq!(error["current_rev"], 2);
    assert_eq!(
        verdicts(&fake, &raced),
        (0, 0),
        "no confirmation on text C never read"
    );
    assert_eq!(show(&a, &raced)["memory"]["status"], "inferred");

    // Without a session every run of a key is one writer, and the error
    // says how to tell runs apart.
    let sessionless = agent(&fake, dir.path(), "agent-e", "");
    let own = id_of(&remember(&sessionless, "Sessionless note on caching", &[]));
    let error = refused(
        &sessionless,
        &["memory", "confirm", &own, "--evidence", "mine"],
        7,
    );
    assert_eq!(error["code"], "memory_self_confirm");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("MEMOIRE_SESSION is not set"), "{message}");
    assert!(message.contains("unique to each run"), "{message}");
    let error = refused(&a, &["memory", "confirm", &id, "--evidence", "mine"], 7);
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("in this session"),
        "{error}"
    );
}

fn updated_at(memoire: &Memoire, id: &str) -> (String, i64) {
    let memory = show(memoire, id)["memory"].clone();
    (
        memory["updated_at"].as_str().unwrap().to_owned(),
        memory["rev"].as_i64().unwrap(),
    )
}

#[test]
fn confirm_dispute_and_resolve_refresh_recency() {
    let (fake, dir) = world();
    second_key(&fake);
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    let b = agent_with(&fake, dir.path(), SECOND_KEY, "agent-b", "s-2");
    let c = agent(&fake, dir.path(), "agent-c", "s-3");
    ok(&a, &["memory", "init"]);
    let older = id_of(&remember(&a, "Printers need a driver update", &[]));
    let newer = id_of(&remember(&a, "Scanners save to the shared drive", &[]));
    let listed = |memoire: &Memoire| -> Vec<String> {
        ok(memoire, &["memory", "recall"])["memories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(listed(&a), [newer.clone(), older.clone()]);

    let (before, rev) = updated_at(&a, &older);
    let confirmed = confirm(&c, &older);
    assert!(
        confirmed["updated_at"].as_str().unwrap() > before.as_str(),
        "{confirmed}"
    );
    assert_eq!(confirmed["rev"], rev);
    assert_eq!(
        confirmed["author"]["agent"], "agent-a",
        "a confirmation is not a revision"
    );
    assert_eq!(show(&a, &older)["revisions"].as_array().unwrap().len(), 1);
    // Checked a moment ago: first in the newest-first listing.
    assert_eq!(listed(&a), [older.clone(), newer.clone()]);

    let (before, _) = updated_at(&a, &newer);
    let disputed = ok(&c, &["memory", "dispute", &newer, "--reason", "it moved"]);
    let after_dispute = disputed["updated_at"].as_str().unwrap().to_owned();
    assert!(after_dispute > before);
    assert_eq!(disputed["rev"], 1);
    let resolved = ok(&c, &["memory", "resolve", &newer, "--reason", "it did not"]);
    assert!(resolved["updated_at"].as_str().unwrap() > after_dispute.as_str());
    assert_eq!(resolved["rev"], 1);
    assert_eq!(show(&a, &newer)["revisions"].as_array().unwrap().len(), 1);

    // Refused confirmations leave the time alone.
    let unchanged = |memoire: &Memoire, args: &[&str], exit: i32, id: &str| {
        let before = updated_at(&a, id);
        refused(memoire, args, exit);
        assert_eq!(updated_at(&a, id), before, "{args:?}");
    };
    unchanged(
        &a,
        &["memory", "confirm", &older, "--evidence", "mine"],
        7,
        &older,
    );
    ok(
        &a,
        &[
            "memory",
            "revise",
            &older,
            "--body",
            "Version 4 or later.",
            "--reason",
            "detail",
        ],
    );
    unchanged(
        &c,
        &[
            "memory",
            "confirm",
            &older,
            "--evidence",
            "old",
            "--rev",
            "1",
        ],
        6,
        &older,
    );

    // The race: B rewrites the text between C's read and C's write; the
    // time is B's revision's, not C's refused confirmation's.
    let racing = newer.clone();
    let (sender, receiver) = std::sync::mpsc::channel::<Run>();
    fake.before_next_write(move || {
        sender
            .send(b.run(
                &[
                    "memory",
                    "revise",
                    &racing,
                    "--title",
                    "Scanners save to the archive",
                    "--reason",
                    "moved",
                    "--json",
                ],
                "",
            ))
            .unwrap();
    });
    refused(
        &c,
        &["memory", "confirm", &newer, "--evidence", "saw it"],
        6,
    );
    let revised = receiver.recv().unwrap().ok().json();
    assert_eq!(
        updated_at(&a, &newer),
        (revised["updated_at"].as_str().unwrap().to_owned(), 2)
    );
}

/// Before the next write query the fake handles, `writer` runs `args`
/// (with `--json`) to the end; its run arrives on the receiver.
fn race(fake: &Fake, writer: Memoire, args: Vec<String>) -> std::sync::mpsc::Receiver<Run> {
    let (sender, receiver) = std::sync::mpsc::channel::<Run>();
    fake.before_next_write(move || {
        let mut args: Vec<&str> = args.iter().map(String::as_str).collect();
        args.push("--json");
        sender.send(writer.run(&args, "")).unwrap();
    });
    receiver
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_owned()).collect()
}

#[test]
fn a_purge_rederives_what_was_linked_after_it_read() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let left = id_of(&remember(&a, "Left behind by a purge", &[]));
    let purged = id_of(&remember(&a, "Replacement about to be purged", &[]));
    // The supersede lands after the purge read its links.
    let receiver = race(
        &fake,
        agent(&fake, dir.path(), "agent-b", "s-2"),
        strings(&[
            "memory",
            "supersede",
            &left,
            "--by",
            &purged,
            "--reason",
            "moved",
        ]),
    );
    ok(
        &a,
        &["memory", "forget", &purged, "--reason", "leak", "--purge"],
    );
    assert_eq!(
        receiver.recv().unwrap().ok().json()["superseded"]["status"],
        "superseded"
    );
    let now = show(&a, &left)["memory"].clone();
    assert_eq!(now["superseded_by"], Value::Null);
    assert_eq!(now["status"], "inferred", "not hidden for good");
    let found = ok(&a, &["memory", "recall", "left behind"]);
    assert_eq!(found["memories"][0]["id"], left);
}

#[test]
fn a_write_never_carries_back_a_link_to_a_purged_memory() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let old = id_of(&remember(&a, "Old proxy address", &[]));
    let current = id_of(&remember(&a, "New proxy address", &["--force"]));
    ok(
        &a,
        &[
            "memory",
            "supersede",
            &old,
            "--by",
            &current,
            "--reason",
            "moved",
        ],
    );
    assert_eq!(show(&a, &current)["memory"]["supersedes"], old);
    // `old` is purged between the revise's read and its write.
    let receiver = race(
        &fake,
        agent(&fake, dir.path(), "agent-b", "s-2"),
        strings(&["memory", "forget", &old, "--reason", "leak", "--purge"]),
    );
    let revised = ok(
        &a,
        &[
            "memory",
            "revise",
            &current,
            "--add-tag",
            "net",
            "--reason",
            "tag",
        ],
    );
    receiver.recv().unwrap().ok();
    assert_eq!(revised["supersedes"], Value::Null, "no dangling link");
    let history = show(&a, &current);
    let last = history["revisions"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["supersedes"], Value::Null);
    assert_eq!(
        count(
            &fake,
            &format!("SELECT count(*) FROM memoire_memory_memories WHERE supersedes = '{old}'")
        ),
        0
    );
}

#[test]
fn two_purges_racing_leave_the_first_ones_event() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let id = id_of(&remember(&a, "Purged twice at once", &[]));
    let receiver = race(
        &fake,
        agent(&fake, dir.path(), "agent-b", "s-2"),
        strings(&["memory", "forget", &id, "--reason", "first", "--purge"]),
    );
    let error = refused(
        &a,
        &["memory", "forget", &id, "--reason", "second", "--purge"],
        5,
    );
    receiver.recv().unwrap().ok();
    assert_eq!(error["code"], "memory_not_found");
    assert!(
        error["message"].as_str().unwrap().contains("was purged at"),
        "{error}"
    );
    let db = sqlite(&fake, "memory");
    let events: Vec<(String, String)> = db
        .prepare("SELECT verb, agent FROM memoire_memory_events WHERE memory_id = ?1")
        .unwrap()
        .query_map([&id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(events, [("purge".to_owned(), "agent-b".to_owned())]);
}

#[test]
fn a_cycle_closed_by_a_concurrent_supersede_is_refused_in_the_write() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let [x, y, z, p] = [
        "Ring xray one",
        "Ring yankee two",
        "Ring zulu three",
        "Ring papa four",
    ]
    .map(|t| id_of(&remember(&a, t, &["--force"])));
    ok(
        &a,
        &["memory", "supersede", &y, "--by", &z, "--reason", "1"],
    );
    ok(
        &a,
        &["memory", "supersede", &p, "--by", &x, "--reason", "2"],
    );
    // X by Y passes its read (Y→Z does not reach X); Z by P lands before
    // its write and touches neither X's nor Y's rev.
    let receiver = race(
        &fake,
        agent(&fake, dir.path(), "agent-b", "s-2"),
        strings(&["memory", "supersede", &z, "--by", &p, "--reason", "3"]),
    );
    let error = refused(
        &a,
        &["memory", "supersede", &x, "--by", &y, "--reason", "4"],
        6,
    );
    receiver.recv().unwrap().ok();
    assert_eq!(error["code"], "memory_cycle", "{error}");
    let now = show(&a, &x);
    assert_eq!(now["memory"]["status"], "inferred");
    assert_eq!(now["memory"]["superseded_by"], Value::Null);
    // Its own rev 1 and the rev P→X gave it; nothing of the refused write.
    assert_eq!(now["revisions"].as_array().unwrap().len(), 2);
}

#[test]
fn verdicts_racing_a_supersede_or_forget_are_refused() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    let c = agent(&fake, dir.path(), "agent-c", "s-3");
    ok(&a, &["memory", "init"]);
    let other = id_of(&remember(&a, "Successor note for the race", &[]));

    // A confirmation pending while the memory is superseded.
    let confirmed = id_of(&remember(&a, "Confirm target superseded meanwhile", &[]));
    let receiver = race(
        &fake,
        agent(&fake, dir.path(), "agent-b", "s-2"),
        strings(&[
            "memory",
            "supersede",
            &confirmed,
            "--by",
            &other,
            "--reason",
            "newer",
        ]),
    );
    let error = refused(
        &c,
        &["memory", "confirm", &confirmed, "--evidence", "checked"],
        6,
    );
    receiver.recv().unwrap().ok();
    assert_eq!(error["code"], "memory_not_current", "{error}");
    assert_eq!(error["superseded_by"], other);
    assert_eq!(verdicts(&fake, &confirmed), (0, 0));

    // A dispute pending while the memory is forgotten.
    let disputed = id_of(&remember(&a, "Dispute target forgotten meanwhile", &[]));
    let receiver = race(
        &fake,
        agent(&fake, dir.path(), "agent-b", "s-2"),
        strings(&["memory", "forget", &disputed, "--reason", "obsolete"]),
    );
    let error = refused(
        &c,
        &["memory", "dispute", &disputed, "--reason", "wrong"],
        6,
    );
    receiver.recv().unwrap().ok();
    assert_eq!(
        (error["code"].clone(), error["status"].clone()),
        (json!("memory_not_current"), json!("forgotten"))
    );
    assert_eq!(verdicts(&fake, &disputed), (0, 0));

    // A resolution pending while the memory is forgotten.
    let resolved = id_of(&remember(&a, "Resolve target forgotten meanwhile", &[]));
    ok(&c, &["memory", "dispute", &resolved, "--reason", "wrong"]);
    let receiver = race(
        &fake,
        agent(&fake, dir.path(), "agent-b", "s-2"),
        strings(&["memory", "forget", &resolved, "--reason", "obsolete"]),
    );
    let error = refused(&c, &["memory", "resolve", &resolved, "--reason", "fine"], 6);
    receiver.recv().unwrap().ok();
    assert_eq!(error["code"], "memory_not_current", "{error}");
    assert_eq!(
        show(&a, &resolved)["disputes"][0]["resolved_at"],
        Value::Null
    );
}

#[test]
fn scope_and_kind_are_scanned_for_secrets() {
    let (fake, outside) = world();
    let a = agent(&fake, outside.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let id = id_of(&remember(&a, "Harmless memory for scans", &[]));
    // Key-shaped, in the lowercase a scope or kind allows.
    let key = format!("{}{}", ["mz", "k_"].concat(), "q7rx".repeat(6));
    let anthropic = format!("{}{}", ["sk", "-ant-"].concat(), "a1b2".repeat(6));
    let scope = format!("project:github.com/acme/{key}");
    let before = footprint(&fake);
    for (args, field) in [
        (
            vec![
                "memory",
                "remember",
                "--title",
                "t",
                "--scope",
                scope.as_str(),
            ],
            "scope",
        ),
        (
            vec![
                "memory",
                "remember",
                "--title",
                "t",
                "--kind",
                anthropic.as_str(),
            ],
            "kind",
        ),
        (
            vec!["memory", "revise", &id, "--scope", &scope, "--reason", "r"],
            "scope",
        ),
        (
            vec![
                "memory", "revise", &id, "--kind", &anthropic, "--reason", "r",
            ],
            "kind",
        ),
    ] {
        let error = refused(&a, &args, 7);
        assert_eq!(error["code"], "memory_secret_detected", "{args:?}");
        assert_eq!(error["field"], field);
    }
    // A scope taken from the git remote is scanned too.
    let checkout = tempfile::tempdir().unwrap();
    git(checkout.path(), &["init", "-q"]);
    git(
        checkout.path(),
        &[
            "remote",
            "add",
            "origin",
            &format!("git@github.com:acme/{key}.git"),
        ],
    );
    let inside = agent(&fake, checkout.path(), "agent-a", "s-1");
    let error = refused(
        &inside,
        &["memory", "remember", "--title", "From the checkout"],
        7,
    );
    assert_eq!(
        (error["code"].clone(), error["field"].clone()),
        (json!("memory_secret_detected"), json!("scope"))
    );
    assert_eq!(footprint(&fake), before);
}

#[cfg(unix)]
#[test]
fn json_paths_that_are_not_utf8_do_not_panic() {
    use std::os::unix::ffi::OsStrExt as _;
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    remember(&a, "Exported with an odd name", &[]);
    let odd = dir
        .path()
        .join(std::ffi::OsStr::from_bytes(b"out-\xff.jsonl"));
    let run = a.run_os(
        &[
            "memory".as_ref(),
            "export".as_ref(),
            "-o".as_ref(),
            odd.as_os_str(),
            "--json".as_ref(),
        ],
        "",
    );
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.json()["count"], 1);
    assert!(run.json()["path"].as_str().unwrap().ends_with(".jsonl"));
    assert!(odd.exists());
}

/// `memory remember TITLE --supersedes OLD --reason REASON EXTRA…`.
fn replace_args<'a>(
    title: &'a str,
    old: &'a str,
    reason: &'a str,
    extra: &[&'a str],
) -> Vec<&'a str> {
    let mut args = vec![
        "memory",
        "remember",
        "--title",
        title,
        "--supersedes",
        old,
        "--reason",
        reason,
    ];
    args.extend_from_slice(extra);
    args
}

fn ids_of(found: &Value) -> Vec<String> {
    found["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(id_of)
        .collect()
}

#[test]
fn remember_supersedes_replaces_a_memory_in_one_step() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let old = id_of(&remember(
        &a,
        "Acme deploys need the VPN connected",
        &[
            "--scope",
            "client:acme",
            "--kind",
            "pitfall",
            "--tag",
            "deploy",
            "--tag",
            "vpn",
            "--evidence",
            "e1",
            "--body",
            "Connect first, then run make deploy.",
        ],
    ));
    // A near-duplicate of the old title, and no --force.
    let title = "Acme deploys need the VPN connected first";
    let reason = "the gateway moved behind the VPN";
    let new = ok(&a, &replace_args(title, &old, reason, &[]));
    let new_id = id_of(&new);
    assert_ne!(new_id, old);
    assert_eq!(
        [
            &new["scope"],
            &new["kind"],
            &new["tags"],
            &new["body"],
            &new["evidence"],
            &new["status"],
            &new["supersedes"],
            &new["superseded_by"],
            &new["rev"],
            &new["title"],
        ],
        [
            &json!("client:acme"),
            &json!("pitfall"),
            &json!(["deploy", "vpn"]),
            &json!(""),
            &json!([]),
            &json!("inferred"),
            &json!(old),
            &Value::Null,
            &json!(1),
            &json!(title),
        ]
    );

    let old_now = show(&a, &old);
    assert_eq!(old_now["memory"]["status"], "superseded");
    assert_eq!(old_now["memory"]["superseded_by"], new_id);
    assert_eq!(old_now["memory"]["rev"], 2);
    let revisions = old_now["revisions"].as_array().unwrap();
    assert_eq!(revisions.len(), 2);
    assert_eq!(
        (
            revisions[1]["rev"].clone(),
            revisions[1]["status"].clone(),
            revisions[1]["superseded_by"].clone(),
            revisions[1]["reason"].clone(),
            revisions[1]["body"].clone(),
        ),
        (
            json!(2),
            json!("superseded"),
            json!(new_id),
            json!(reason),
            json!("Connect first, then run make deploy.")
        )
    );
    let new_now = show(&a, &new_id);
    let revisions = new_now["revisions"].as_array().unwrap();
    assert_eq!(revisions.len(), 1);
    assert_eq!(
        (
            revisions[0]["reason"].clone(),
            revisions[0]["supersedes"].clone()
        ),
        (json!(reason), json!(old))
    );

    for args in [
        &["memory", "recall", "--scope", "client:acme"][..],
        &[
            "memory",
            "recall",
            "acme deploys vpn",
            "--scope",
            "client:acme",
        ],
    ] {
        assert_eq!(ids_of(&ok(&a, args)), [new_id.as_str()], "{args:?}");
    }

    let events = ok(&a, &["memory", "log", "--limit", "2"])["events"].clone();
    assert_eq!(
        (
            events[0]["verb"].clone(),
            events[0]["memory_id"].clone(),
            events[0]["summary"].clone(),
            events[0]["rev"].clone(),
        ),
        (
            json!("supersede"),
            json!(old),
            json!(format!("by {new_id}: {reason}")),
            json!(2)
        )
    );
    assert_eq!(
        (
            events[1]["verb"].clone(),
            events[1]["memory_id"].clone(),
            events[1]["summary"].clone(),
            events[1]["rev"].clone(),
        ),
        (json!("remember"), json!(new_id), json!(title), json!(1))
    );
    assert!(events[0]["seq"].as_i64() > events[1]["seq"].as_i64());
}

#[test]
fn remember_supersedes_keeps_what_the_flags_do_not_give() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let old_title = "Acme deploys need the VPN connected";
    let acme = [
        "--scope",
        "client:acme",
        "--kind",
        "pitfall",
        "--tag",
        "deploy",
        "--tag",
        "vpn",
    ];
    let old = id_of(&remember(&a, old_title, &acme));
    // The same title in another scope: only a replace into that scope
    // meets it.
    let beta = id_of(&remember(&a, old_title, &["--scope", "client:beta"]));
    let title = "Acme deploys need the VPN connected first";
    let error = refused(
        &a,
        &replace_args(title, &old, "moved", &["--scope", "client:beta"]),
        6,
    );
    assert_eq!(error["code"], "memory_duplicate");
    assert_eq!(error["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(error["candidates"][0]["id"], beta);

    let new = ok(
        &a,
        &replace_args(
            title,
            &old,
            "moved",
            &[
                "--scope",
                "client:gamma",
                "--kind",
                "decision",
                "--tag",
                "network",
                "--body",
                "Its own body.",
                "--evidence",
                "e2",
            ],
        ),
    );
    assert_eq!(
        (
            new["scope"].clone(),
            new["kind"].clone(),
            new["tags"].clone(),
            new["body"].clone(),
            new["evidence"].clone(),
            new["supersedes"].clone(),
        ),
        (
            json!("client:gamma"),
            json!("decision"),
            json!(["network"]),
            json!("Its own body."),
            json!(["e2"]),
            json!(old)
        )
    );

    // Without --json: the memory as usual, and a note naming the old one.
    let second = id_of(&remember(&a, "Beta builds use the shared runner", &acme));
    let run = a
        .run(
            &[
                "memory",
                "remember",
                "--title",
                "Beta builds use the dedicated runner",
                "--supersedes",
                &second,
                "--reason",
                "runner split",
                "--kind",
                "fact",
            ],
            "",
        )
        .ok();
    assert!(run.stdout.contains("supersedes"), "{}", run.stdout);
    assert!(run.stdout.contains(&second), "{}", run.stdout);
    assert!(run.stderr.contains(&second), "{}", run.stderr);
    assert_eq!(run.stderr.lines().count(), 1, "{}", run.stderr);
    let replaced = show(&a, &second)["memory"]["superseded_by"].clone();
    let current = ok(&a, &["memory", "show", replaced.as_str().unwrap()])["memory"].clone();
    assert_eq!(
        (
            current["kind"].clone(),
            current["scope"].clone(),
            current["tags"].clone()
        ),
        (
            json!("fact"),
            json!("client:acme"),
            json!(["deploy", "vpn"])
        )
    );
}

#[test]
fn remember_supersedes_still_checks_other_memories_for_duplicates() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let old = id_of(&remember(&a, "Acme deploys need the VPN connected", &[]));
    let third = id_of(&remember(
        &a,
        "Acme deploys need the VPN connection",
        &["--force"],
    ));
    let before = footprint(&fake);
    let title = "Acme deploys need the VPN connected first";
    let error = refused(&a, &replace_args(title, &old, "clearer", &[]), 6);
    assert_eq!(error["code"], "memory_duplicate");
    let candidates: Vec<&str> = error["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(candidates, [third.as_str()]);
    assert_eq!(footprint(&fake), before);
    let old_now = show(&a, &old)["memory"].clone();
    assert_eq!(
        (old_now["status"].clone(), old_now["rev"].clone()),
        (json!("inferred"), json!(1))
    );

    let new = ok(&a, &replace_args(title, &old, "clearer", &["--force"]));
    assert_eq!(new["supersedes"], old);
    assert_eq!(show(&a, &old)["memory"]["status"], "superseded");
}

#[test]
fn remember_supersedes_refusals_write_nothing() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    let c = agent(&fake, dir.path(), "agent-c", "s-3");
    ok(&a, &["memory", "init"]);
    let replaced = id_of(&remember(&a, "Builds run on Jenkins", &[]));
    let by = id_of(&remember(&a, "Builds run on GitHub Actions", &[]));
    ok(
        &a,
        &[
            "memory",
            "supersede",
            &replaced,
            "--by",
            &by,
            "--reason",
            "migrated",
        ],
    );
    let forgotten = id_of(&remember(&a, "Temporary hostname is tmp-seven", &[]));
    ok(&a, &["memory", "forget", &forgotten, "--reason", "gone"]);
    let purged = id_of(&remember(&a, "Scratch note to purge", &[]));
    ok(
        &a,
        &["memory", "forget", &purged, "--reason", "leak", "--purge"],
    );
    let current = id_of(&remember(&a, "Release notes go in the changelog", &[]));
    let before = footprint(&fake);

    let replace = |old: &str, code: i32| {
        refused(
            &a,
            &replace_args("A replacement note", old, "newer", &[]),
            code,
        )
    };
    let error = replace(&replaced, 6);
    assert_eq!(
        (
            error["code"].clone(),
            error["status"].clone(),
            error["superseded_by"].clone()
        ),
        (json!("memory_not_current"), json!("superseded"), json!(by))
    );
    let error = replace(&forgotten, 6);
    assert_eq!(
        (error["code"].clone(), error["status"].clone()),
        (json!("memory_not_current"), json!("forgotten"))
    );
    let error = replace("mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8", 5);
    assert_eq!(error["code"], "memory_not_found");
    let error = replace(&purged, 5);
    assert_eq!(error["code"], "memory_not_found");
    assert!(
        error["message"].as_str().unwrap().contains("was purged at"),
        "{error}"
    );
    for args in [
        &[
            "memory",
            "remember",
            "--title",
            "t",
            "--supersedes",
            current.as_str(),
        ][..],
        &["memory", "remember", "--title", "t", "--reason", "r"],
        &[
            "memory",
            "remember",
            "--title",
            "t",
            "--supersedes",
            "mem_x",
            "--reason",
            "r",
        ],
    ] {
        let run = a.run(&with_json(args), "");
        assert_eq!(run.code, 2, "{args:?}: {}", run.stderr);
        assert_eq!(run.stdout, "");
    }
    let secret = fake_secret();
    let error = refused(
        &a,
        &replace_args("A replacement note", &current, &secret, &[]),
        7,
    );
    assert_eq!(
        (error["code"].clone(), error["field"].clone()),
        (json!("memory_secret_detected"), json!("reason"))
    );
    assert!(!error.to_string().contains(&secret));
    assert_eq!(footprint(&fake), before);

    // A disputed memory is current, so it may be replaced.
    ok(&c, &["memory", "dispute", &current, "--reason", "outdated"]);
    let new = ok(
        &a,
        &replace_args("Release notes go in the docs site", &current, "moved", &[]),
    );
    assert_eq!(new["supersedes"], current);
    assert_eq!(show(&a, &current)["memory"]["status"], "superseded");
}

#[test]
fn a_replace_racing_another_writer_is_refused_whole() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let rival = || agent(&fake, dir.path(), "agent-b", "s-2");

    // Revised between the replace's read and its write.
    let old = id_of(&remember(&a, "Proxy listens on port 3128", &[]));
    let before = footprint(&fake);
    let receiver = race(
        &fake,
        rival(),
        strings(&[
            "memory",
            "revise",
            &old,
            "--add-tag",
            "net",
            "--reason",
            "tag",
        ]),
    );
    let error = refused(
        &a,
        &replace_args("Proxy listens on port 8080", &old, "moved", &[]),
        6,
    );
    receiver.recv().unwrap().ok();
    assert_eq!(
        (
            error["code"].clone(),
            error["id"].clone(),
            error["current_rev"].clone()
        ),
        (json!("memory_conflict"), json!(old), json!(2))
    );
    // Only the revise's revision and event: nothing of the replacement.
    let (m, r, e, f) = (before[0], before[1], before[2], before[3]);
    assert_eq!(footprint(&fake), [m, r + 1, e + 1, f]);
    assert_eq!(
        count(
            &fake,
            &format!(
                "SELECT count(*) FROM memoire_memory_revisions \
                 WHERE supersedes = '{old}' OR superseded_by IS NOT NULL"
            )
        ),
        0
    );
    let old_now = show(&a, &old)["memory"].clone();
    assert_eq!(
        (old_now["status"].clone(), old_now["superseded_by"].clone()),
        (json!("inferred"), Value::Null)
    );

    // Superseded meanwhile by a plain supersede.
    let old = id_of(&remember(&a, "Mirror syncs every hour", &[]));
    let by = id_of(&remember(&a, "Cache warms at midnight", &[]));
    let receiver = race(
        &fake,
        rival(),
        strings(&[
            "memory",
            "supersede",
            &old,
            "--by",
            &by,
            "--reason",
            "newer",
        ]),
    );
    let error = refused(
        &a,
        &replace_args("Mirror syncs every ten minutes", &old, "faster", &[]),
        6,
    );
    receiver.recv().unwrap().ok();
    assert_eq!(
        (error["code"].clone(), error["superseded_by"].clone()),
        (json!("memory_not_current"), json!(by))
    );
    assert_eq!(
        count(
            &fake,
            "SELECT count(*) FROM memoire_memory_memories WHERE title LIKE 'Mirror syncs every ten%'"
        ),
        0
    );

    // Purged meanwhile.
    let old = id_of(&remember(&a, "Scratch bucket is tmp-eight", &[]));
    let receiver = race(
        &fake,
        rival(),
        strings(&["memory", "forget", &old, "--reason", "leak", "--purge"]),
    );
    let error = refused(
        &a,
        &replace_args("Scratch bucket is tmp-nine", &old, "renamed", &[]),
        5,
    );
    receiver.recv().unwrap().ok();
    assert_eq!(error["code"], "memory_not_found");
    assert!(
        error["message"].as_str().unwrap().contains("purged"),
        "{error}"
    );
    assert_eq!(
        count(
            &fake,
            "SELECT count(*) FROM memoire_memory_memories WHERE title LIKE 'Scratch bucket%'"
        ),
        0
    );
}

#[test]
fn a_purge_after_a_replace_clears_the_link_both_ways() {
    let (fake, dir) = world();
    let a = agent(&fake, dir.path(), "agent-a", "s-1");
    ok(&a, &["memory", "init"]);
    let holds = |word: &str| {
        dump(&fake)
            .iter()
            .any(|(_, bytes)| bytes.windows(word.len()).any(|w| w == word.as_bytes()))
    };
    let reason_of = |id: &str, rev: i64| -> String {
        sqlite(&fake, "memory")
            .query_row(
                "SELECT reason FROM memoire_memory_revisions WHERE memory_id = ?1 AND rev = ?2",
                rusqlite::params![id, rev],
                |r| r.get(0),
            )
            .unwrap()
    };

    // The old one is purged: the new one no longer says what it replaced.
    let old = id_of(&remember(&a, "Ledger exports run on Fridays", &[]));
    let new = id_of(&ok(
        &a,
        &replace_args(
            "Ledger exports run on Mondays",
            &old,
            "zibblort moved the schedule",
            &[],
        ),
    ));
    assert!(holds("zibblort"));
    ok(
        &a,
        &["memory", "forget", &old, "--reason", "leak", "--purge"],
    );
    let now = show(&a, &new)["memory"].clone();
    assert_eq!(
        (now["supersedes"].clone(), now["status"].clone()),
        (Value::Null, json!("inferred"))
    );
    assert_eq!(reason_of(&new, 1), "purged");
    assert!(!holds("zibblort"), "the reason survived the purge");

    // The new one is purged: the old one is current again.
    let old = id_of(&remember(&a, "Invoices are numbered per year", &[]));
    let new = id_of(&ok(
        &a,
        &replace_args(
            "Invoices are numbered per month",
            &old,
            "quazzle asked for it",
            &[],
        ),
    ));
    ok(
        &a,
        &["memory", "forget", &new, "--reason", "leak", "--purge"],
    );
    let now = show(&a, &old)["memory"].clone();
    assert_eq!(
        (now["superseded_by"].clone(), now["status"].clone()),
        (Value::Null, json!("inferred"))
    );
    assert!(ids_of(&ok(&a, &["memory", "recall"])).contains(&old));
    assert_eq!(reason_of(&old, 2), "purged");
    let summary: String = sqlite(&fake, "memory")
        .query_row(
            "SELECT summary FROM memoire_memory_events WHERE memory_id = ?1 AND verb = 'supersede'",
            [&old],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(summary, format!("by {new}"));
    assert!(!holds("quazzle"), "the reason survived the purge");
}
