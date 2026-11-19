//! `memoire skill install`: no login, no network, only files.
mod support;

use serde_json::json;
use std::path::{Path, PathBuf};
use support::Memoire;

/// The skill as the repository holds it.
fn skill() -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/memoire/SKILL.md"))
        .unwrap()
}

fn installed(run: &support::Run) -> Vec<(PathBuf, bool)> {
    run.json()["installed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                PathBuf::from(i["path"].as_str().unwrap()),
                i["changed"].as_bool().unwrap(),
            )
        })
        .collect()
}

#[test]
fn install_writes_the_skill_for_claude_and_is_idempotent() {
    // Memoire's HOME is its private configuration directory; no login.
    let memoire = Memoire::new();
    let home = memoire.config.path().to_owned();
    let target = home.join(".claude/skills/memoire/SKILL.md");
    let run = memoire
        .run(&["skill", "install", "--claude", "--json"], "")
        .ok();
    assert_eq!(installed(&run), [(target.clone(), true)]);
    assert_eq!(std::fs::read(&target).unwrap(), skill());
    assert!(!memoire.credentials().exists());

    let again = memoire
        .run(&["skill", "install", "--claude", "--json"], "")
        .ok();
    assert_eq!(installed(&again), [(target.clone(), false)]);

    let human = memoire.run(&["skill", "install", "--claude"], "").ok();
    assert_eq!(human.stdout, format!("Unchanged {}\n", target.display()));
}

#[test]
fn codex_dir_and_the_environment_choose_the_targets() {
    let memoire = Memoire::new();
    let home = memoire.config.path().to_owned();
    let extra = tempfile::tempdir().unwrap();
    let run = memoire
        .run(
            &[
                "skill",
                "install",
                "--codex",
                "--dir",
                extra.path().to_str().unwrap(),
                "--json",
            ],
            "",
        )
        .ok();
    let codex = home.join(".codex/skills/memoire/SKILL.md");
    let dir = extra.path().join("memoire/SKILL.md");
    assert_eq!(
        installed(&run),
        [(codex.clone(), true), (dir.clone(), true)]
    );
    assert_eq!(std::fs::read(&codex).unwrap(), skill());
    assert_eq!(std::fs::read(&dir).unwrap(), skill());

    let claude_dir = tempfile::tempdir().unwrap();
    let codex_home = tempfile::tempdir().unwrap();
    let configured = Memoire::new()
        .env("CLAUDE_CONFIG_DIR", claude_dir.path().to_str().unwrap())
        .env("CODEX_HOME", codex_home.path().to_str().unwrap());
    let run = configured
        .run(&["skill", "install", "--claude", "--codex", "--json"], "")
        .ok();
    let claude = claude_dir.path().join("skills/memoire/SKILL.md");
    let codex = codex_home.path().join("skills/memoire/SKILL.md");
    assert_eq!(
        installed(&run),
        [(claude.clone(), true), (codex.clone(), true)]
    );
    assert_eq!(std::fs::read(&claude).unwrap(), skill());
    assert!(!configured.config.path().join(".claude").exists());
}

#[test]
fn a_different_skill_is_kept_unless_forced() {
    let memoire = Memoire::new();
    let target = memoire
        .config
        .path()
        .join(".claude/skills/memoire/SKILL.md");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "my own notes\n").unwrap();
    let dir = tempfile::tempdir().unwrap();

    let run = memoire.run(
        &[
            "skill",
            "install",
            "--claude",
            "--dir",
            dir.path().to_str().unwrap(),
            "--json",
        ],
        "",
    );
    assert_eq!(run.code, 6, "{}", run.stderr);
    assert_eq!(
        run.error(),
        json!({"error": {"code": "skill_exists", "classification": "conflict",
                         "path": target.to_str().unwrap(),
                         "message": run.error()["error"]["message"]}})
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "my own notes\n");
    assert!(
        !dir.path().join("memoire").exists(),
        "a refusal writes no target"
    );

    let run = memoire
        .run(&["skill", "install", "--claude", "--force", "--json"], "")
        .ok();
    assert_eq!(installed(&run), [(target.clone(), true)]);
    assert_eq!(std::fs::read(&target).unwrap(), skill());
}

#[test]
fn a_target_is_required() {
    let run = Memoire::new().run(&["skill", "install"], "");
    assert_eq!(run.code, 2, "{}", run.stderr);
    let run = Memoire::new().run(&["skill", "install", "--force", "--json"], "");
    assert_eq!(run.code, 2);
    assert_eq!(run.error()["error"]["code"], "invalid_usage");
}

#[test]
fn an_alias_of_one_destination_installs_it_once() {
    // From ~/.claude, `--dir skills` names the --claude target again,
    // which no comparison of the paths as given can see.
    let home = Memoire::new();
    let claude = home.config.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let memoire = Memoire::new()
        .dir(&claude)
        .env("CLAUDE_CONFIG_DIR", claude.to_str().unwrap());
    let run = memoire
        .run(
            &["skill", "install", "--claude", "--dir", "skills", "--json"],
            "",
        )
        .ok();
    let target = claude.join("skills/memoire/SKILL.md");
    assert_eq!(
        installed(&run),
        [
            (target.clone(), true),
            (PathBuf::from("skills/memoire/SKILL.md"), false),
        ]
    );
    assert_eq!(std::fs::read(&target).unwrap(), skill());
    let files: Vec<_> = std::fs::read_dir(target.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(files, ["SKILL.md"], "one file, no temporary left");
}
