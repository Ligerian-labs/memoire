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
//! max_database_bytes = 1073741824   # file size limit (65536 to 1 GiB)
//! idle_close_seconds = 300          # a burstable database's connections
//!                                   # close after this long idle (1 to 86400)
//! last_used_flush_seconds = 60      # last use reaches PostgreSQL this
//!                                   # often (1 to 3600)
//! sweep_interval_seconds = 60       # expiry sweep period (1 to 3600)
//! snapshot_interval_seconds = 21600 # persistent snapshot period
//!                                   # (1 to 604800)
//! snapshot_retention_seconds = 604800 # snapshots kept (1 to 31536000)
//! ```
//!
//! Everything but `data_dir` has the contract's value by default; the keys
//! exist so tests and deploy proofs can watch an expiry, a size limit or an
//! idle close happen in seconds.
use memoire_api::limits::{
    DEFAULT_IDLE_TTL_SECONDS, MAX_DATABASE_BYTES, MAX_LIFETIME_SECONDS, MIN_IDLE_TTL_SECONDS,
};
use merezha_platform::{ConfigIssue, ModuleSection};
use std::path::PathBuf;

const KNOWN: &[&str] = &[
    "data_dir",
    "min_idle_ttl_seconds",
    "default_idle_ttl_seconds",
    "max_database_bytes",
    "idle_close_seconds",
    "last_used_flush_seconds",
    "sweep_interval_seconds",
    "snapshot_interval_seconds",
    "snapshot_retention_seconds",
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
    /// The largest database file.
    pub max_database_bytes: u64,
    /// How long a burstable database stays open without a request.
    pub idle_close_seconds: u64,
    /// How often last use is written to PostgreSQL.
    pub last_used_flush_seconds: u64,
    /// How often the sweeper expires burstable databases.
    pub sweep_interval_seconds: u64,
    /// How often persistent databases are snapshotted.
    pub snapshot_interval_seconds: u64,
    /// How long snapshots are kept.
    pub snapshot_retention_seconds: u64,
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
        let mut number = |key, default, range| integer(section, key, default, range, &mut issues);
        let min_idle_ttl_seconds = number(
            "min_idle_ttl_seconds",
            MIN_IDLE_TTL_SECONDS,
            1..=MAX_LIFETIME_SECONDS,
        );
        let default_idle_ttl_seconds = number(
            "default_idle_ttl_seconds",
            DEFAULT_IDLE_TTL_SECONDS,
            min_idle_ttl_seconds..=MAX_LIFETIME_SECONDS,
        );
        let max_database_bytes = number(
            "max_database_bytes",
            MAX_DATABASE_BYTES,
            65_536..=MAX_DATABASE_BYTES,
        );
        let idle_close_seconds = number("idle_close_seconds", 300, 1..=86_400);
        let last_used_flush_seconds = number("last_used_flush_seconds", 60, 1..=3_600);
        let sweep_interval_seconds = number("sweep_interval_seconds", 60, 1..=3_600);
        let snapshot_interval_seconds = number(
            "snapshot_interval_seconds",
            6 * 3_600,
            1..=MAX_LIFETIME_SECONDS,
        );
        let snapshot_retention_seconds = number(
            "snapshot_retention_seconds",
            MAX_LIFETIME_SECONDS,
            1..=365 * 86_400,
        );
        match data_dir {
            Some(data_dir) if issues.is_empty() => Ok(Self {
                data_dir,
                min_idle_ttl_seconds,
                default_idle_ttl_seconds,
                max_database_bytes,
                idle_close_seconds,
                last_used_flush_seconds,
                sweep_interval_seconds,
                snapshot_interval_seconds,
                snapshot_retention_seconds,
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

/// An optional whole number in `range`, `default` when absent.
fn integer(
    section: &ModuleSection,
    key: &'static str,
    default: u64,
    range: std::ops::RangeInclusive<u64>,
    issues: &mut Vec<ConfigIssue>,
) -> u64 {
    let Some(value) = section.table.get(key) else {
        return default;
    };
    match value.as_integer().map(u64::try_from) {
        Some(Ok(n)) if range.contains(&n) => n,
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
