//! `memory-cursors.json`: how far `memory log --unseen` has read, per
//! server, database and scope filter, on this machine.
//!
//! The file lives beside the credentials and is written the same way:
//! directory 0700, file 0600, atomic replace; a file others can read is
//! refused. A cursor remembers the store it belongs to, so a database
//! created again under the same name starts over.
use crate::config::{Env, create_private_dir, io_error, shared_mode, write_private};
use crate::error::{Error, Result, exit};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const FILE: &str = "memory-cursors.json";
/// Held while a run reads, merges and rewrites the cursors file.
const LOCK: &str = "memory-cursors.lock";
const VERSION: u32 = 1;

/// Where one filter has read up to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub store_id: String,
    pub seq: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorFile {
    version: u32,
    cursors: BTreeMap<String, Cursor>,
}

/// The key of a cursor: the server URL, the database and, when the read
/// was filtered, its sorted scopes. A filtered read keeps its own cursor,
/// so it never hides other scopes' events from an unfiltered one.
pub fn key(server: &str, db: &str, scopes: &[String]) -> String {
    let mut key = format!("{server} {db}");
    if !scopes.is_empty() {
        let mut scopes = scopes.to_vec();
        scopes.sort();
        scopes.dedup();
        key.push(' ');
        key.push_str(&scopes.join(","));
    }
    key
}

/// The cursors file of this machine.
pub struct Cursors {
    path: PathBuf,
}

impl Cursors {
    pub fn new(env: &Env) -> Result<Self> {
        Ok(Self {
            path: env.dir()?.join(FILE),
        })
    }

    /// For a cursors file at `path`.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&self) -> Result<CursorFile> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CursorFile {
                    version: VERSION,
                    cursors: BTreeMap::new(),
                });
            }
            Err(e) => return Err(io_error(&self.path, &e)),
        };
        if let Some(mode) = shared_mode(&self.path)? {
            return Err(Error::local(
                "insecure_cursors",
                exit::FAILURE,
                format!(
                    "{} is readable by others (mode {mode:o}); run `chmod 600` on it",
                    self.path.display()
                ),
            ));
        }
        let invalid = || {
            Error::local(
                "invalid_cursors",
                exit::FAILURE,
                format!(
                    "{} is not a cursors file this CLI reads; fix or remove it",
                    self.path.display()
                ),
            )
        };
        let file: CursorFile = serde_json::from_str(&text).map_err(|_| invalid())?;
        if file.version != VERSION {
            return Err(invalid());
        }
        Ok(file)
    }

    /// The exclusive lock on the cursors file, beside it in the same
    /// private directory.
    fn lock(&self) -> Result<std::fs::File> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        create_private_dir(dir).map_err(|e| io_error(dir, &e))?;
        let path = dir.join(LOCK);
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(|e| io_error(&path, &e))?;
        file.lock().map_err(|e| io_error(&path, &e))?;
        Ok(file)
    }

    /// The cursor `key`, if this machine has one.
    pub fn saved(&self, key: &str) -> Result<Option<Cursor>> {
        Ok(self.load()?.cursors.remove(key))
    }

    /// Moves cursor `key`, never backwards within one store. The file is
    /// read again first, so the other cursors keep what concurrent runs
    /// wrote in the meantime, and a run that read less than another one
    /// already recorded leaves the further cursor.
    pub fn advance(&self, key: &str, cursor: Cursor) -> Result<()> {
        // Agents on one machine share the file: without the lock two runs
        // could both load it before either renames, and the later rename
        // would drop the other's cursor. Released when dropped.
        let _lock = self.lock()?;
        let mut file = self.load()?;
        let cursor = match file.cursors.get(key) {
            Some(saved) if saved.store_id == cursor.store_id && saved.seq > cursor.seq => {
                saved.clone()
            }
            _ => cursor,
        };
        file.cursors.insert(key.to_owned(), cursor);
        write_private(
            &self.path,
            &serde_json::to_string_pretty(&file).expect("cursors serialize"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_sort_the_scope_filter() {
        assert_eq!(
            key("https://m.example", "memory", &[]),
            "https://m.example memory"
        );
        let scopes = ["run:2".to_owned(), "global".to_owned(), "run:2".to_owned()];
        assert_eq!(
            key("https://m.example", "memory", &scopes),
            "https://m.example memory global,run:2"
        );
    }

    #[test]
    fn concurrent_advances_lose_nothing_and_never_go_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memoire").join(FILE);
        const RUNS: usize = 8;
        const STEPS: i64 = 15;
        std::thread::scope(|scope| {
            for run in 0..RUNS {
                let path = path.clone();
                scope.spawn(move || {
                    let cursors = Cursors::at(path);
                    for seq in 1..=STEPS {
                        let at = |seq| Cursor {
                            store_id: "mst_a".into(),
                            seq,
                        };
                        cursors.advance(&format!("run-{run}"), at(seq)).unwrap();
                        // Every run moves the shared cursor to its own,
                        // interleaved, values: it only ever grows.
                        let shared = seq * RUNS as i64 + run as i64;
                        cursors.advance("shared", at(shared)).unwrap();
                        let seen = cursors.saved("shared").unwrap().unwrap().seq;
                        assert!(seen >= shared, "{seen} < {shared}");
                    }
                });
            }
        });
        let cursors = Cursors::at(&path);
        for run in 0..RUNS {
            let saved = cursors.saved(&format!("run-{run}")).unwrap();
            assert_eq!(saved.map(|c| c.seq), Some(STEPS), "run-{run}");
        }
        let top = STEPS * RUNS as i64 + RUNS as i64 - 1;
        assert_eq!(cursors.saved("shared").unwrap().map(|c| c.seq), Some(top));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let lock = path.parent().unwrap().join(LOCK);
            let mode = std::fs::metadata(&lock).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn cursors_are_private_and_follow_their_store() {
        let dir = tempfile::tempdir().unwrap();
        let cursors = Cursors::at(dir.path().join("memoire").join(FILE));
        assert_eq!(cursors.saved("k").unwrap(), None);
        cursors
            .advance(
                "k",
                Cursor {
                    store_id: "mst_a".into(),
                    seq: 7,
                },
            )
            .unwrap();
        cursors
            .advance(
                "other",
                Cursor {
                    store_id: "mst_a".into(),
                    seq: 2,
                },
            )
            .unwrap();
        let seq = |key: &str| cursors.saved(key).unwrap().map(|c| c.seq);
        assert_eq!(seq("k"), Some(7));
        // A run that read less never moves the cursor back in its store...
        cursors
            .advance(
                "k",
                Cursor {
                    store_id: "mst_a".into(),
                    seq: 3,
                },
            )
            .unwrap();
        assert_eq!(seq("k"), Some(7));
        // ...but a new store starts over.
        cursors
            .advance(
                "other",
                Cursor {
                    store_id: "mst_b".into(),
                    seq: 1,
                },
            )
            .unwrap();
        assert_eq!(seq("other"), Some(1));
        cursors
            .advance(
                "other",
                Cursor {
                    store_id: "mst_a".into(),
                    seq: 2,
                },
            )
            .unwrap();
        assert_eq!(seq("other"), Some(2));
        assert_eq!(seq("missing"), None);
        let text = std::fs::read_to_string(cursors.path()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(
            json["cursors"]["k"],
            serde_json::json!({"store_id": "mst_a", "seq": 7})
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(cursors.path()), 0o600);
            assert_eq!(mode(cursors.path().parent().unwrap()), 0o700);
            std::fs::set_permissions(cursors.path(), std::fs::Permissions::from_mode(0o644))
                .unwrap();
            assert_eq!(cursors.saved("k").unwrap_err().code(), "insecure_cursors");
            std::fs::set_permissions(cursors.path(), std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        for bad in [
            "{",
            r#"{"version": 2, "cursors": {}}"#,
            r#"{"cursors": {}}"#,
        ] {
            std::fs::write(cursors.path(), bad).unwrap();
            let error = cursors.saved("k").unwrap_err();
            assert_eq!(error.code(), "invalid_cursors", "{bad}");
            assert_eq!(error.exit_code(), exit::FAILURE);
        }
    }
}
