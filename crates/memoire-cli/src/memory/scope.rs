//! Memory scopes: the slug that labels a memory, such as `global`,
//! `client:acme` or `project:github.com/acme/widgets`.
//!
//! Inside a git repository the default scope is derived from the `origin`
//! remote, so every clone of the same repository, over SSH or HTTPS, on any
//! machine, gets the same scope. Local paths and remotes without a host and a
//! path have no project scope.

use std::path::Path;
use std::process::{Command, Stdio};

const MAX_LEN: usize = 128;

/// Whether `text` is a scope: `^[a-z0-9][a-z0-9._:/-]{0,127}$`.
pub fn valid(text: &str) -> bool {
    let bytes = text.as_bytes();
    let Some((first, rest)) = bytes.split_first() else {
        return false;
    };
    bytes.len() <= MAX_LEN
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && rest.iter().all(|b| {
            b.is_ascii_lowercase()
                || b.is_ascii_digit()
                || matches!(b, b'.' | b'_' | b':' | b'/' | b'-')
        })
}

/// `project:<host>/<path>` for a git remote URL; `None` for a local path, a
/// URL without a host or a path, or a result that is not a valid scope.
pub fn from_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let (host, path) = if url.contains("://") {
        split_url(url)?
    } else {
        split_scp(url)?
    };
    let path = clean_path(path);
    if host.is_empty() || path.is_empty() {
        return None;
    }
    let scope = format!("project:{}/{}", host.to_lowercase(), path.to_lowercase());
    valid(&scope).then_some(scope)
}

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

/// Host and raw path of a `scheme://[user@]host[:port]/path` URL.
fn split_url(url: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.eq_ignore_ascii_case("file") {
        return None;
    }
    let (authority, path) = rest.split_once('/')?;
    Some((strip_port(strip_userinfo(authority)), path))
}

/// Host and raw path of an scp-like `[user@]host:path` remote.
fn split_scp(url: &str) -> Option<(&str, &str)> {
    let colon = url.find(':')?;
    if url.find('/').is_some_and(|slash| slash < colon) {
        return None;
    }
    let host = strip_userinfo(&url[..colon]);
    let is_drive = host.len() == 1 && host.as_bytes()[0].is_ascii_alphabetic();
    (!is_drive).then_some((host, &url[colon + 1..]))
}

fn strip_userinfo(authority: &str) -> &str {
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
}

/// Drops a trailing `:<digits>`; an empty port is left in place and fails
/// the scope check later.
fn strip_port(host: &str) -> &str {
    match host.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    }
}

/// Removes surrounding slashes and one trailing `.git`.
fn clean_path(path: &str) -> &str {
    let path = path.trim_start_matches('/').trim_end_matches('/');
    let stem = match path.len().checked_sub(4).and_then(|at| path.get(at..)) {
        Some(tail) if tail.eq_ignore_ascii_case(".git") => &path[..path.len() - 4],
        _ => path,
    };
    stem.trim_end_matches('/')
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
    fn valid_accepts_and_refuses() {
        let longest = "a".repeat(128);
        for ok in [
            "global",
            "org:acme",
            "client:acme-corp",
            "run:01jabc",
            "project:github.com/acme/widgets",
            "0",
            "a_b",
            longest.as_str(),
        ] {
            assert!(valid(ok), "{ok:?} should be valid");
        }
        let too_long = "a".repeat(129);
        for bad in [
            "",
            too_long.as_str(),
            "Global",
            "-x",
            ".x",
            ":x",
            "a b",
            "é",
            "a\n",
        ] {
            assert!(!valid(bad), "{bad:?} should be refused");
        }
    }

    #[test]
    fn from_remote_table() {
        let table: &[(&str, Option<&str>)] = &[
            (
                "git@github.com:Ligerian-Labs/Memoire.git",
                Some("project:github.com/ligerian-labs/memoire"),
            ),
            (
                "https://github.com/acme/widgets",
                Some("project:github.com/acme/widgets"),
            ),
            (
                "https://github.com/acme/widgets.git",
                Some("project:github.com/acme/widgets"),
            ),
            (
                "  https://GitHub.com/Acme/Widgets.GIT/\n",
                Some("project:github.com/acme/widgets"),
            ),
            (
                "https://user:synthetic@gitlab.example.com:8443/group/sub/repo.git/",
                Some("project:gitlab.example.com/group/sub/repo"),
            ),
            (
                "ssh://git@host.example:2222/owner/repo.git",
                Some("project:host.example/owner/repo"),
            ),
            (
                "git://host.example/owner/repo",
                Some("project:host.example/owner/repo"),
            ),
            (
                "git+ssh://git@host.example/owner/repo",
                Some("project:host.example/owner/repo"),
            ),
            (
                "host.example:owner/repo",
                Some("project:host.example/owner/repo"),
            ),
            (
                "git@host.example:/abs/owner/repo.git",
                Some("project:host.example/abs/owner/repo"),
            ),
            (
                "https://dev.azure.com/org/proj/_git/repo",
                Some("project:dev.azure.com/org/proj/_git/repo"),
            ),
            (
                "git@github.com:acme/widgets.git.git",
                Some("project:github.com/acme/widgets.git"),
            ),
            ("", None),
            ("   ", None),
            ("/srv/git/repo.git", None),
            ("../repo", None),
            ("repo", None),
            ("github.com/acme/widgets", None),
            ("file:///srv/git/repo.git", None),
            ("FILE:///srv/x", None),
            (r"C:\repos\widgets", None),
            ("C:/repos/widgets", None),
            ("https://github.com", None),
            ("https://github.com/", None),
            ("https:///acme/widgets", None),
            ("git@github.com:", None),
            ("git@github.com:.git", None),
            ("git@github.com:acme/my repo.git", None),
            ("https://[2001:db8::1]/acme/widgets.git", None),
        ];
        for (input, expected) in table {
            assert_eq!(from_remote(input).as_deref(), *expected, "input {input:?}");
        }
    }

    #[test]
    fn from_remote_refuses_too_long() {
        assert_eq!(
            from_remote(&format!("https://github.com/{}", "a".repeat(200))),
            None
        );
    }

    #[test]
    fn ssh_and_https_give_the_same_scope() {
        assert_eq!(
            from_remote("git@github.com:Acme/Widgets.git"),
            from_remote("https://github.com/acme/widgets"),
        );
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
