//! `memoire skill install`: writes the memoire skill, embedded in the
//! binary, where coding agents load skills from. Needs no login.
use crate::cli::SkillInstallArgs;
use crate::config::io_error;
use crate::error::{Error, Result, exit};
use crate::output::Out;
use serde_json::json;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The skill, as `skills/memoire/SKILL.md` holds it.
pub const SKILL: &str = include_str!("../../../skills/memoire/SKILL.md");

/// Where the skill goes below a skills directory.
const RELATIVE: [&str; 2] = ["memoire", "SKILL.md"];

/// What the targets resolve against, read once from the environment.
#[derive(Clone, Debug, Default)]
pub struct Homes {
    pub home: Option<PathBuf>,
    pub claude_config_dir: Option<PathBuf>,
    pub codex_home: Option<PathBuf>,
}

impl Homes {
    pub fn from_process() -> Self {
        let var = |name| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Self {
            home: var("HOME"),
            claude_config_dir: var("CLAUDE_CONFIG_DIR"),
            codex_home: var("CODEX_HOME"),
        }
    }

    fn under_home(&self, dir: &str, variable: &str) -> Result<PathBuf> {
        self.home.as_ref().map(|h| h.join(dir)).ok_or_else(|| {
            Error::local(
                "no_home",
                exit::FAILURE,
                format!("set HOME or {variable} to say where the skill goes"),
            )
        })
    }

    /// The SKILL.md paths the flags name, in flag order.
    pub fn targets(&self, args: &SkillInstallArgs) -> Result<Vec<PathBuf>> {
        let mut dirs = Vec::new();
        if args.claude {
            let base = match &self.claude_config_dir {
                Some(dir) => dir.clone(),
                None => self.under_home(".claude", "CLAUDE_CONFIG_DIR")?,
            };
            dirs.push(base.join("skills"));
        }
        if args.codex {
            let base = match &self.codex_home {
                Some(dir) => dir.clone(),
                None => self.under_home(".codex", "CODEX_HOME")?,
            };
            dirs.push(base.join("skills"));
        }
        if let Some(dir) = &args.dir {
            dirs.push(dir.clone());
        }
        // Two flags may name one destination (`--claude --dir
        // ~/.claude/skills`): each is checked, written and reported once.
        let mut targets: Vec<PathBuf> = Vec::new();
        for dir in dirs {
            let path = RELATIVE.iter().fold(dir, |path, part| path.join(part));
            if !targets.contains(&path) {
                targets.push(path);
            }
        }
        Ok(targets)
    }
}

/// Whether `path` already holds the skill: `Some(true)` identical,
/// `Some(false)` something else, `None` nothing.
fn current(path: &Path) -> Result<Option<bool>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes == SKILL.as_bytes())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(path, &e)),
    }
}

fn exists(path: &Path) -> Error {
    Error::local(
        "skill_exists",
        exit::CONFLICT,
        format!(
            "{} holds another SKILL.md; pass --force to replace it",
            path.display()
        ),
    )
    .with("path", path.display().to_string())
}

/// Writes the skill to `path` through a temporary file renamed over it;
/// without `replace`, a file that appeared there meanwhile is kept and
/// refused with `skill_exists`.
///
/// Whether it wrote: a file already there with the skill's own bytes is
/// this run's (one destination named twice, through an alias) or a
/// concurrent run's, and is left as it is.
fn write(path: &Path, replace: bool) -> Result<bool> {
    let dir = path.parent().expect("a target ends in memoire/SKILL.md");
    std::fs::create_dir_all(dir).map_err(|e| io_error(dir, &e))?;
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|e| io_error(dir, &e))?;
    file.write_all(SKILL.as_bytes())
        .and_then(|()| file.as_file().sync_all())
        .map_err(|e| io_error(path, &e))?;
    // Agents read the skill as the user; others may too, as with any
    // file the user writes.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o644))
            .map_err(|e| io_error(path, &e))?;
    }
    if replace {
        file.persist(path).map_err(|e| io_error(path, &e.error))?;
        return Ok(true);
    }
    match file.persist_noclobber(path) {
        Ok(_) => Ok(true),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => match current(path)? {
            Some(true) => Ok(false),
            _ => Err(exists(path)),
        },
        Err(e) => Err(io_error(path, &e.error)),
    }
}

/// `skill install`. Every target is checked before any is written, so a
/// refusal leaves all of them as they were.
pub fn install(out: &Out, homes: &Homes, args: &SkillInstallArgs) -> Result<()> {
    let mut plan = Vec::new();
    for path in homes.targets(args)? {
        match current(&path)? {
            Some(false) if !args.force => return Err(exists(&path)),
            Some(true) => plan.push((path, false, false)),
            Some(false) => plan.push((path, true, true)),
            // Nothing there: only --force may replace what appears meanwhile.
            None => plan.push((path, true, args.force)),
        }
    }
    for (path, changed, replace) in &mut plan {
        if *changed {
            *changed = write(path, *replace)?;
        }
    }
    if out.json {
        let installed: Vec<_> = plan
            .iter()
            .map(|(path, changed, _)| json!({"path": path.display().to_string(), "changed": changed}))
            .collect();
        return out.json(&json!({ "installed": installed }));
    }
    let lines: Vec<String> = plan
        .iter()
        .map(|(path, changed, _)| {
            format!(
                "{} {}",
                if *changed { "Installed" } else { "Unchanged" },
                path.display()
            )
        })
        .collect();
    out.line(&lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(claude: bool, codex: bool, dir: Option<&str>) -> SkillInstallArgs {
        SkillInstallArgs {
            claude,
            codex,
            dir: dir.map(PathBuf::from),
            force: false,
        }
    }

    #[test]
    fn targets_follow_the_flags_and_the_environment() {
        let homes = Homes {
            home: Some("/h".into()),
            ..Homes::default()
        };
        assert_eq!(
            homes.targets(&args(true, true, Some("/d"))).unwrap(),
            [
                PathBuf::from("/h/.claude/skills/memoire/SKILL.md"),
                PathBuf::from("/h/.codex/skills/memoire/SKILL.md"),
                PathBuf::from("/d/memoire/SKILL.md"),
            ]
        );
        let homes = Homes {
            home: None,
            claude_config_dir: Some("/c".into()),
            codex_home: Some("/x".into()),
        };
        assert_eq!(
            homes.targets(&args(true, true, None)).unwrap(),
            [
                PathBuf::from("/c/skills/memoire/SKILL.md"),
                PathBuf::from("/x/skills/memoire/SKILL.md"),
            ]
        );
        let homes = Homes {
            home: Some("/h".into()),
            ..Homes::default()
        };
        assert_eq!(
            homes
                .targets(&args(true, false, Some("/h/.claude/skills")))
                .unwrap(),
            [PathBuf::from("/h/.claude/skills/memoire/SKILL.md")],
            "one destination named twice is one target"
        );
        let none = Homes::default();
        assert_eq!(
            none.targets(&args(true, false, None)).unwrap_err().code(),
            "no_home"
        );
        assert!(none.targets(&args(false, false, Some("/d"))).is_ok());
    }

    #[test]
    fn a_file_that_appears_meanwhile_is_kept_unless_replacing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memoire/SKILL.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Written by someone else after the preflight saw nothing.
        std::fs::write(&path, "their own skill\n").unwrap();
        let error = write(&path, false).unwrap_err();
        let same = dir.path().join("same/memoire/SKILL.md");
        std::fs::create_dir_all(same.parent().unwrap()).unwrap();
        std::fs::write(&same, SKILL).unwrap();
        assert!(
            !write(&same, false).unwrap(),
            "the skill already there is left"
        );
        assert_eq!(error.code(), "skill_exists");
        assert_eq!(error.to_json()["error"]["path"], path.display().to_string());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "their own skill\n");
        assert!(write(&path, true).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), SKILL.as_bytes());
        let fresh = dir.path().join("other/memoire/SKILL.md");
        assert!(write(&fresh, false).unwrap());
        assert_eq!(std::fs::read(&fresh).unwrap(), SKILL.as_bytes());
    }

    #[test]
    fn the_skill_has_its_frontmatter() {
        assert!(SKILL.starts_with("---\n"), "{SKILL}");
        assert!(SKILL.contains("\nname: memoire\n"));
    }
}
