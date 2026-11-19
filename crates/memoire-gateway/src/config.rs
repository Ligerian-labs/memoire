//! `[modules.memoire]`.
//!
//! ```toml
//! [modules.memoire]
//! data_dir = "/var/lib/memoire"     # required; relative paths resolve
//!                                   # against the configuration file
//! min_idle_ttl_seconds = 60         # shortest burstable idle TTL a client
//!                                   # may ask for (1 to 604800)
//! default_idle_ttl_seconds = 86400  # a burstable database's TTL when the
//!                                   # client names none (min to 604800)
//! ```
//!
//! The TTL keys exist so tests and deploy proofs can watch a burstable
//! database expire in seconds; production keeps the contract's defaults.
use memoire_api::limits::{DEFAULT_IDLE_TTL_SECONDS, MAX_LIFETIME_SECONDS, MIN_IDLE_TTL_SECONDS};
use merezha_platform::{ConfigIssue, ModuleSection};
use std::path::PathBuf;

const KNOWN: &[&str] = &[
    "data_dir",
    "min_idle_ttl_seconds",
    "default_idle_ttl_seconds",
];

/// The module's validated settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Root of the SQLite files: `<data_dir>/teams/<team>/<db>.sqlite`.
    pub data_dir: PathBuf,
    /// The shortest idle TTL a create or extend may ask for.
    pub min_idle_ttl_seconds: u64,
    /// The idle TTL of a burstable database created without one.
    pub default_idle_ttl_seconds: u64,
}

impl Settings {
    /// Reads the section, reporting every problem at once.
    ///
    /// # Errors
    ///
    /// `required` for a missing `data_dir`, `expected_string` when it is not
    /// a non-empty string, `expected_integer` or `out_of_range` for a TTL
    /// key, and `unknown_key` for any other key.
    pub fn parse(section: &ModuleSection) -> Result<Self, Vec<ConfigIssue>> {
        let mut issues = section.unknown_keys(KNOWN);
        let data_dir = match section.table.get("data_dir") {
            None => {
                issues.push(issue(section, "data_dir", "required"));
                None
            }
            Some(toml_value) => match toml_value.as_str().filter(|s| !s.is_empty()) {
                Some(path) => Some(section.base_dir().join(path)),
                None => {
                    issues.push(issue(section, "data_dir", "expected_string"));
                    None
                }
            },
        };
        let min_idle_ttl_seconds = seconds(
            section,
            "min_idle_ttl_seconds",
            MIN_IDLE_TTL_SECONDS,
            1,
            &mut issues,
        );
        let default_idle_ttl_seconds = seconds(
            section,
            "default_idle_ttl_seconds",
            DEFAULT_IDLE_TTL_SECONDS,
            min_idle_ttl_seconds,
            &mut issues,
        );
        match data_dir {
            Some(data_dir) if issues.is_empty() => Ok(Self {
                data_dir,
                min_idle_ttl_seconds,
                default_idle_ttl_seconds,
            }),
            _ => Err(issues),
        }
    }

    /// The directory holding every team's database files.
    pub fn teams_dir(&self) -> PathBuf {
        self.data_dir.join("teams")
    }

    /// Whether a requested idle TTL is in range.
    pub fn idle_ttl_in_range(&self, seconds: u64) -> bool {
        (self.min_idle_ttl_seconds..=MAX_LIFETIME_SECONDS).contains(&seconds)
    }
}

fn issue(section: &ModuleSection, key: &str, code: &'static str) -> ConfigIssue {
    ConfigIssue::new(section.key(key), code, section.origin(key))
}

/// An optional whole number of seconds in `min..=7 days`, `default` when
/// absent.
fn seconds(
    section: &ModuleSection,
    key: &str,
    default: u64,
    min: u64,
    issues: &mut Vec<ConfigIssue>,
) -> u64 {
    let Some(value) = section.table.get(key) else {
        return default;
    };
    match value.as_integer().map(u64::try_from) {
        Some(Ok(n)) if (min..=MAX_LIFETIME_SECONDS).contains(&n) => n,
        Some(_) => {
            issues.push(issue(section, key, "out_of_range"));
            default
        }
        None => {
            issues.push(issue(section, key, "expected_integer"));
            default
        }
    }
}
