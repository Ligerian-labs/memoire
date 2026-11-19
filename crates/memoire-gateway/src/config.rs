//! `[modules.memoire]`.
//!
//! ```toml
//! [modules.memoire]
//! data_dir = "/var/lib/memoire"   # required; relative paths resolve
//!                                 # against the configuration file
//! ```
use merezha_platform::{ConfigIssue, ModuleSection};
use std::path::PathBuf;

const KNOWN: &[&str] = &["data_dir"];

/// The module's validated settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Root of the SQLite files: `<data_dir>/teams/<team>/<db>.sqlite`.
    pub data_dir: PathBuf,
}

impl Settings {
    /// Reads the section, reporting every problem at once.
    ///
    /// # Errors
    ///
    /// `required` for a missing `data_dir`, `expected_string` when it is not
    /// a non-empty string, and `unknown_key` for any other key.
    pub fn parse(section: &ModuleSection) -> Result<Self, Vec<ConfigIssue>> {
        let mut issues = section.unknown_keys(KNOWN);
        let data_dir = match section.table.get("data_dir") {
            None => {
                issues.push(ConfigIssue::new(
                    section.key("data_dir"),
                    "required",
                    section.origin("data_dir"),
                ));
                None
            }
            Some(toml_value) => match toml_value.as_str().filter(|s| !s.is_empty()) {
                Some(path) => Some(section.base_dir().join(path)),
                None => {
                    issues.push(ConfigIssue::new(
                        section.key("data_dir"),
                        "expected_string",
                        section.origin("data_dir"),
                    ));
                    None
                }
            },
        };
        match data_dir {
            Some(data_dir) if issues.is_empty() => Ok(Self { data_dir }),
            _ => Err(issues),
        }
    }

    /// The directory holding every team's database files.
    pub fn teams_dir(&self) -> PathBuf {
        self.data_dir.join("teams")
    }
}
