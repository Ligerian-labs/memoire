//! The default memory scope: inside a git repository, the project scope of
//! its `origin` remote (see [`memoire_memory::scope`]), so every clone of
//! the same repository, over SSH or HTTPS, on any machine, gets the same
//! scope.

use memoire_memory::scope::from_remote;
use std::path::Path;
use std::process::{Command, Stdio};

/// The project scope of the git repository that contains `dir`, from
/// `git remote get-url origin`; `None` when git is missing, `dir` is not in
/// a repository, it has no `origin`, or the URL has no project scope.
pub fn project_scope(dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["remote", "get-url", "origin"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    from_remote(text.lines().next()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    fn init(dir: &Path) {
        git(dir, &["-c", "init.defaultBranch=main", "init", "-q"]);
    }

    #[test]
    fn project_scope_reads_origin() {
        let dir = tempfile::tempdir().unwrap();
        init(dir.path());
        git(
            dir.path(),
            &["remote", "add", "origin", "git@github.com:Acme/Widgets.git"],
        );
        let expected = Some("project:github.com/acme/widgets".to_string());
        assert_eq!(project_scope(dir.path()), expected);

        let sub = dir.path().join("src/deep");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(project_scope(&sub), expected);
    }

    #[test]
    fn project_scope_is_none_without_origin_or_repository() {
        let no_remote = tempfile::tempdir().unwrap();
        init(no_remote.path());
        assert_eq!(project_scope(no_remote.path()), None);

        let plain = tempfile::tempdir().unwrap();
        assert_eq!(project_scope(plain.path()), None);

        assert_eq!(project_scope(&plain.path().join("missing")), None);
    }
}
