//! Where the gateway URL and the API key come from.
//!
//! `memoire login` stores both in `credentials.json` under the
//! configuration directory: `MEMOIRE_CONFIG_DIR`, else
//! `$XDG_CONFIG_HOME/memoire`, else `~/.config/memoire`. The directory is
//! mode 0700 and the file 0600; a file others can read is refused.
//! `MEMOIRE_SERVER` and `MEMOIRE_API_KEY` override what is stored, so an
//! agent can run without logging in.
use crate::error::{Error, Result, exit};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    io::Write as _,
    path::{Path, PathBuf},
};

const FILE: &str = "credentials.json";

/// What the process environment says, read once at startup.
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub config_dir: Option<PathBuf>,
    pub server: Option<String>,
    pub api_key: Option<Secret>,
    /// `MEMOIRE_MEMORY_SQL`: `1`, the memory verbs use the SQL convention
    /// over the query route without asking the gateway for its memory API;
    /// `0`, they use the API and never fall back. Unset, they ask.
    pub memory_sql: Option<bool>,
}

impl Env {
    pub fn from_process() -> Self {
        let var = |name| std::env::var(name).ok().filter(|v: &String| !v.is_empty());
        let config_dir = var("MEMOIRE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| var("XDG_CONFIG_HOME").map(|d| PathBuf::from(d).join("memoire")))
            .or_else(|| var("HOME").map(|d| PathBuf::from(d).join(".config/memoire")));
        Self {
            config_dir,
            server: var("MEMOIRE_SERVER"),
            api_key: var("MEMOIRE_API_KEY").map(Secret::new),
            memory_sql: var("MEMOIRE_MEMORY_SQL").and_then(|v| match v.as_str() {
                "1" => Some(true),
                "0" => Some(false),
                _ => None,
            }),
        }
    }

    pub(crate) fn dir(&self) -> Result<&Path> {
        self.config_dir.as_deref().ok_or_else(|| {
            Error::local(
                "no_config_dir",
                exit::FAILURE,
                "set HOME or MEMOIRE_CONFIG_DIR to say where credentials live",
            )
        })
    }

    pub fn credentials_path(&self) -> Result<PathBuf> {
        Ok(self.dir()?.join(FILE))
    }

    /// The stored credentials, `None` before the first login.
    pub fn stored(&self) -> Result<Option<Credentials>> {
        let Some(dir) = self.config_dir.as_deref() else {
            return Ok(None);
        };
        let path = dir.join(FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_error(&path, &e)),
        };
        check_private(&path)?;
        serde_json::from_str(&text).map(Some).map_err(|_| {
            Error::local(
                "invalid_credentials",
                exit::AUTH,
                format!("{} is not valid; run `memoire login` again", path.display()),
            )
        })
    }

    /// The gateway URL: `MEMOIRE_SERVER`, else the stored one.
    pub fn server(&self) -> Result<String> {
        if let Some(server) = &self.server {
            return Ok(server.clone());
        }
        self.stored()?.map(|c| c.server).ok_or_else(not_logged_in)
    }

    /// The gateway URL and API key: each from the environment, else stored.
    /// The stored key goes only to the stored server: with another
    /// `MEMOIRE_SERVER`, `MEMOIRE_API_KEY` is needed too.
    pub fn login(&self) -> Result<(String, Secret)> {
        let stored = if self.server.is_some() && self.api_key.is_some() {
            None
        } else {
            self.stored()?
        };
        if let Some(key) = &self.api_key {
            let server = self
                .server
                .clone()
                .or_else(|| stored.map(|c| c.server))
                .ok_or_else(not_logged_in)?;
            return Ok((server, key.clone()));
        }
        // The stored key travels with the stored URL, and only when
        // MEMOIRE_SERVER is unset or names that URL.
        let same = |stored: &str| {
            self.server
                .as_deref()
                .is_none_or(|s| s.trim_end_matches('/') == stored.trim_end_matches('/'))
        };
        stored
            .filter(|c| same(&c.server))
            .map(|c| (c.server, c.api_key))
            .ok_or_else(not_logged_in)
    }

    /// Writes the credentials atomically: a private temporary file in the
    /// same directory, then a rename over the old one.
    pub fn store(&self, credentials: &Credentials) -> Result<PathBuf> {
        let path = self.dir()?.join(FILE);
        let text = serde_json::to_string_pretty(credentials).expect("credentials serialize");
        write_private(&path, &text)?;
        Ok(path)
    }
}

/// Writes `text` and a newline to `path` atomically and privately: the
/// directory is made 0700, a 0600 temporary file beside the target is
/// written, then renamed over it.
pub(crate) fn write_private(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    create_private_dir(dir).map_err(|e| io_error(dir, &e))?;
    // tempfile creates the file with mode 0600 on Unix.
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|e| io_error(dir, &e))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.as_file().sync_all())
        .map_err(|e| io_error(path, &e))?;
    file.persist(path).map_err(|e| io_error(path, &e.error))?;
    Ok(())
}

fn not_logged_in() -> Error {
    Error::local(
        "not_logged_in",
        exit::AUTH,
        "no gateway credential: run `memoire login --server URL`, or set MEMOIRE_SERVER and MEMOIRE_API_KEY",
    )
}

pub fn io_error(path: &Path, error: &std::io::Error) -> Error {
    Error::local(
        "io_error",
        exit::FAILURE,
        format!("{}: {error}", path.display()),
    )
}

/// The stored login.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    pub server: String,
    pub api_key: Secret,
}

/// A credential. Its `Debug` never shows the value, and it has no `Display`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

#[cfg(unix)]
pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    // An existing directory keeps its mode; tighten it.
    let mode = std::fs::metadata(dir)?.permissions().mode();
    if mode & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode & 0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

fn check_private(path: &Path) -> Result<()> {
    match shared_mode(path)? {
        None => Ok(()),
        Some(mode) => Err(Error::local(
            "insecure_credentials",
            exit::AUTH,
            format!(
                "{} is readable by others (mode {mode:o}); run `chmod 600` on it or log in again",
                path.display(),
            ),
        )),
    }
}

/// The permission bits of `path` when others than its owner may use it;
/// `None` when it is private.
#[cfg(unix)]
pub(crate) fn shared_mode(path: &Path) -> Result<Option<u32>> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(path)
        .map_err(|e| io_error(path, &e))?
        .permissions()
        .mode();
    Ok((mode & 0o077 != 0).then_some(mode & 0o777))
}

#[cfg(not(unix))]
pub(crate) fn shared_mode(_path: &Path) -> Result<Option<u32>> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(dir: &Path) -> Env {
        Env {
            config_dir: Some(dir.join("memoire")),
            ..Env::default()
        }
    }

    #[test]
    fn stores_privately_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        assert_eq!(env.stored().unwrap(), None);
        assert_eq!(env.login().unwrap_err().code(), "not_logged_in");
        let credentials = Credentials {
            server: "https://memoire.example".into(),
            api_key: Secret::new("mzk_synthetic"),
        };
        let path = env.store(&credentials).unwrap();
        assert_eq!(env.stored().unwrap(), Some(credentials.clone()));
        let (server, key) = env.login().unwrap();
        assert_eq!(server, "https://memoire.example");
        assert_eq!(key.expose(), "mzk_synthetic");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(env.stored().unwrap_err().code(), "insecure_credentials");
        }
    }

    #[test]
    fn the_environment_overrides_what_is_stored() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = env(dir.path());
        env.store(&Credentials {
            server: "https://stored.example".into(),
            api_key: Secret::new("mzk_stored"),
        })
        .unwrap();
        env.api_key = Some(Secret::new("mzk_env"));
        let (server, key) = env.login().unwrap();
        assert_eq!(
            (server.as_str(), key.expose()),
            ("https://stored.example", "mzk_env")
        );
        env.server = Some("https://env.example".into());
        env.config_dir = None;
        assert_eq!(env.login().unwrap().0, "https://env.example");
    }

    #[test]
    fn the_stored_key_never_goes_to_another_server() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = env(dir.path());
        env.store(&Credentials {
            server: "https://stored.example".into(),
            api_key: Secret::new("mzk_stored"),
        })
        .unwrap();
        env.server = Some("https://stored.example/".into());
        assert_eq!(env.login().unwrap().1.expose(), "mzk_stored");
        env.server = Some("https://elsewhere.example".into());
        assert_eq!(env.login().unwrap_err().code(), "not_logged_in");
    }

    #[test]
    fn secrets_never_print() {
        let secret = Secret::new("mzk_synthetic");
        assert!(!format!("{secret:?}").contains("mzk_"));
        let credentials = Credentials {
            server: "s".into(),
            api_key: secret,
        };
        assert!(!format!("{credentials:?}").contains("mzk_"));
    }
}
