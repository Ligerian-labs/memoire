//! `memory-cursors.json`: how far `memory log --unseen` has read, per
//! server, database, scope filter and reader, on this machine.
//!
//! The file lives beside the credentials and is written the same way:
//! directory 0700, file 0600, atomic replace; a file others can read is
//! refused. A cursor remembers the store it belongs to, so a database
//! created again under the same name gets a first look again.
use crate::config::{Env, create_private_dir, io_error, shared_mode, write_private};
use crate::error::{Error, Result, exit};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const FILE: &str = "memory-cursors.json";
/// Held while a run reads, merges and rewrites the cursors file.
const LOCK: &str = "memory-cursors.lock";
const VERSION: u32 = 2;

/// A reader's first look at a store starts at the events of this many
/// seconds before the store's now, not at the beginning: a new session
/// sees what is recent, not the whole history.
pub const FIRST_LOOK_SECONDS: u64 = 86_400;

/// A `session:` cursor not used for this many days is dropped at the next
/// write of the file. Named and machine cursors are kept.
pub const SESSION_CURSOR_DAYS: u64 = 7;

/// Who reads, as the cursors file and `--json` name it: `name:NAME` for a
/// role (`--reader`, `MEMOIRE_READER`), else `session:ID` when the session
/// is set, else `machine`.
pub fn reader(name: Option<&str>, session: &str) -> String {
    match name {
        Some(name) => format!("name:{name}"),
        None if !session.is_empty() => format!("session:{session}"),
        None => "machine".into(),
    }
}

/// Where one reader has read up to in one store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub store_id: String,
    pub seq: i64,
}

/// What a cursor is for: the server URL, the database, the scope filter
/// (sorted, without repeats) and the reader. A filtered read keeps its own
/// cursor, so it never hides other scopes' events from an unfiltered one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub server: String,
    pub db: String,
    pub scopes: Vec<String>,
    pub reader: String,
}

impl Key {
    pub fn new(server: &str, db: &str, scopes: &[String], reader: String) -> Self {
        let mut scopes = scopes.to_vec();
        scopes.sort();
        scopes.dedup();
        Self {
            server: server.to_owned(),
            db: db.to_owned(),
            scopes,
            reader,
        }
    }
}

/// One cursor as the file holds it. A session id may hold spaces, so the
/// key is kept as fields rather than joined into one string.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    server: String,
    db: String,
    scopes: Vec<String>,
    reader: String,
    store_id: String,
    seq: i64,
    /// Unix seconds, on this machine's clock, of the last `log --unseen`
    /// that used this cursor.
    seen_at: u64,
}

impl Entry {
    fn is(&self, key: &Key) -> bool {
        self.server == key.server
            && self.db == key.db
            && self.scopes == key.scopes
            && self.reader == key.reader
    }

    /// A session's cursor left unused past [`SESSION_CURSOR_DAYS`].
    fn stale(&self, now: u64) -> bool {
        self.reader.starts_with("session:")
            && now.saturating_sub(self.seen_at) > SESSION_CURSOR_DAYS * 86_400
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorFile {
    version: u32,
    cursors: Vec<Entry>,
}

/// Version 1: one cursor per machine, keyed by
/// `"<server> <db>[ <scopes joined by ,>]"`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileV1 {
    /// Always 1: read before this struct is.
    #[serde(rename = "version")]
    _version: u32,
    cursors: BTreeMap<String, CursorV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorV1 {
    store_id: String,
    seq: i64,
}

/// A version 1 key as a machine cursor's key; `None` when it does not
/// parse. Server and database hold no space, scopes neither space nor
/// comma.
fn key_v1(key: &str) -> Option<Key> {
    let mut parts = key.splitn(3, ' ');
    let server = parts.next().filter(|s| !s.is_empty())?;
    let db = parts.next().filter(|s| !s.is_empty())?;
    let scopes: Vec<String> = match parts.next() {
        None => Vec::new(),
        Some(list) if list.contains(' ') => return None,
        Some(list) => list.split(',').map(str::to_owned).collect(),
    };
    if scopes.iter().any(String::is_empty) {
        return None;
    }
    Some(Key::new(server, db, &scopes, reader(None, "")))
}

/// Unix seconds on this machine's clock.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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

    /// The file as version 2; a version 1 file is migrated in memory, its
    /// cursors seen `now`.
    fn load(&self, now: u64) -> Result<CursorFile> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CursorFile {
                    version: VERSION,
                    cursors: Vec::new(),
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
        let value: serde_json::Value = serde_json::from_str(&text).map_err(|_| invalid())?;
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(2) => serde_json::from_value(value).map_err(|_| invalid()),
            Some(1) => {
                let old: FileV1 = serde_json::from_value(value).map_err(|_| invalid())?;
                let cursors = old
                    .cursors
                    .into_iter()
                    .filter_map(|(key, cursor)| {
                        let key = key_v1(&key)?;
                        Some(Entry {
                            server: key.server,
                            db: key.db,
                            scopes: key.scopes,
                            reader: key.reader,
                            store_id: cursor.store_id,
                            seq: cursor.seq,
                            seen_at: now,
                        })
                    })
                    .collect();
                Ok(CursorFile {
                    version: VERSION,
                    cursors,
                })
            }
            _ => Err(invalid()),
        }
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
    pub fn saved(&self, key: &Key) -> Result<Option<Cursor>> {
        Ok(self
            .load(unix_now())?
            .cursors
            .into_iter()
            .find(|e| e.is(key))
            .map(|e| Cursor {
                store_id: e.store_id,
                seq: e.seq,
            }))
    }

    /// Moves cursor `key`, never backwards within one store, and marks it
    /// seen now.
    pub fn advance(&self, key: &Key, cursor: Cursor) -> Result<()> {
        self.advance_at(key, cursor, unix_now())
    }

    /// [`Cursors::advance`] at `now`, in unix seconds. The file is read
    /// again first, so the other cursors keep what concurrent runs wrote in
    /// the meantime, and a run that read less than another one already
    /// recorded leaves the further cursor. Stale session cursors go.
    fn advance_at(&self, key: &Key, cursor: Cursor, now: u64) -> Result<()> {
        // Agents on one machine share the file: without the lock two runs
        // could both load it before either renames, and the later rename
        // would drop the other's cursor. Released when dropped.
        let _lock = self.lock()?;
        let mut file = self.load(now)?;
        let seq = match file.cursors.iter().find(|e| e.is(key)) {
            Some(saved) if saved.store_id == cursor.store_id && saved.seq > cursor.seq => saved.seq,
            _ => cursor.seq,
        };
        file.cursors.retain(|e| !e.is(key) && !e.stale(now));
        file.cursors.push(Entry {
            server: key.server.clone(),
            db: key.db.clone(),
            scopes: key.scopes.clone(),
            reader: key.reader.clone(),
            store_id: cursor.store_id,
            seq,
            seen_at: now,
        });
        write_private(
            &self.path,
            &serde_json::to_string_pretty(&file).expect("cursors serialize"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: u64 = 1_795_170_000;
    const DAY: u64 = 86_400;

    fn key(reader: &str) -> Key {
        Key::new("https://m.example", "memory", &[], reader.into())
    }

    fn at(store_id: &str, seq: i64) -> Cursor {
        Cursor {
            store_id: store_id.into(),
            seq,
        }
    }

    fn cursors() -> (tempfile::TempDir, Cursors) {
        let dir = tempfile::tempdir().unwrap();
        let cursors = Cursors::at(dir.path().join("memoire").join(FILE));
        (dir, cursors)
    }

    fn file(cursors: &Cursors) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(cursors.path()).unwrap()).unwrap()
    }

    #[test]
    fn readers_come_from_the_name_then_the_session() {
        assert_eq!(reader(Some("nightly"), "s-1"), "name:nightly");
        assert_eq!(reader(None, "s 1"), "session:s 1");
        assert_eq!(reader(None, ""), "machine");
        let scopes = ["run:2".to_owned(), "global".to_owned(), "run:2".to_owned()];
        let key = Key::new("https://m.example", "memory", &scopes, "machine".into());
        assert_eq!(key.scopes, ["global", "run:2"]);
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
                        cursors
                            .advance(&key(&format!("session:run-{run}")), at("mst_a", seq))
                            .unwrap();
                        // Every run moves the shared cursor to its own,
                        // interleaved, values: it only ever grows.
                        let shared = seq * RUNS as i64 + run as i64;
                        cursors
                            .advance(&key("name:shared"), at("mst_a", shared))
                            .unwrap();
                        let seen = cursors.saved(&key("name:shared")).unwrap().unwrap().seq;
                        assert!(seen >= shared, "{seen} < {shared}");
                    }
                });
            }
        });
        let cursors = Cursors::at(&path);
        for run in 0..RUNS {
            let saved = cursors.saved(&key(&format!("session:run-{run}"))).unwrap();
            assert_eq!(saved.map(|c| c.seq), Some(STEPS), "run-{run}");
        }
        let top = STEPS * RUNS as i64 + RUNS as i64 - 1;
        let shared = cursors.saved(&key("name:shared")).unwrap();
        assert_eq!(shared.map(|c| c.seq), Some(top));
        assert_eq!(
            file(&cursors)["cursors"].as_array().unwrap().len(),
            RUNS + 1
        );
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
        let (_dir, cursors) = cursors();
        let k = key("session:s 41");
        let other = key("machine");
        assert_eq!(cursors.saved(&k).unwrap(), None);
        cursors.advance_at(&k, at("mst_a", 7), NOW).unwrap();
        cursors.advance_at(&other, at("mst_a", 2), NOW).unwrap();
        let seq = |key: &Key| cursors.saved(key).unwrap().map(|c| c.seq);
        assert_eq!(seq(&k), Some(7));
        // A run that read less never moves the cursor back in its store...
        cursors.advance_at(&k, at("mst_a", 3), NOW).unwrap();
        assert_eq!(seq(&k), Some(7));
        // ...but a new store starts over.
        cursors.advance_at(&other, at("mst_b", 1), NOW).unwrap();
        assert_eq!(seq(&other), Some(1));
        cursors.advance_at(&other, at("mst_a", 2), NOW).unwrap();
        assert_eq!(seq(&other), Some(2));
        assert_eq!(seq(&key("session:missing")), None);
        // The same reader of another database or scope filter is apart.
        let filtered = Key::new(
            "https://m.example",
            "memory",
            &["global".into()],
            k.reader.clone(),
        );
        assert_eq!(seq(&filtered), None);
        let json = file(&cursors);
        assert_eq!(json["version"], 2);
        assert_eq!(
            json["cursors"][0],
            json!({"server": "https://m.example", "db": "memory", "scopes": [],
                   "reader": "session:s 41", "store_id": "mst_a", "seq": 7, "seen_at": NOW})
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(cursors.path()), 0o600);
            assert_eq!(mode(cursors.path().parent().unwrap()), 0o700);
            std::fs::set_permissions(cursors.path(), std::fs::Permissions::from_mode(0o644))
                .unwrap();
            assert_eq!(cursors.saved(&k).unwrap_err().code(), "insecure_cursors");
            std::fs::set_permissions(cursors.path(), std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        for bad in [
            "{",
            "[]",
            r#"{"version": 3, "cursors": []}"#,
            r#"{"version": 2, "cursors": {}}"#,
            r#"{"version": 1, "cursors": []}"#,
            r#"{"cursors": []}"#,
            r#"{"version": 2, "cursors": [{"server": "s", "db": "d", "scopes": [],
                "reader": "machine", "store_id": "mst_a", "seq": 1, "seen_at": 1, "extra": 1}]}"#,
            r#"{"version": 1, "cursors": {"s d": {"store_id": "mst_a", "seq": 1, "seen_at": 1}}}"#,
        ] {
            std::fs::write(cursors.path(), bad).unwrap();
            let error = cursors.saved(&k).unwrap_err();
            assert_eq!(error.code(), "invalid_cursors", "{bad}");
            assert_eq!(error.exit_code(), exit::FAILURE);
            let error = cursors.advance_at(&k, at("mst_a", 1), NOW).unwrap_err();
            assert_eq!(error.code(), "invalid_cursors", "{bad}");
        }
    }

    #[test]
    fn a_version_1_file_reads_as_machine_cursors() {
        let (_dir, cursors) = cursors();
        std::fs::create_dir_all(cursors.path().parent().unwrap()).unwrap();
        let v1 = json!({"version": 1, "cursors": {
            "https://m.example memory": {"store_id": "mst_a", "seq": 12},
            "https://m.example memory client:acme,global": {"store_id": "mst_a", "seq": 9},
            "https://m.example": {"store_id": "mst_a", "seq": 3},
            "https://m.example memory a b": {"store_id": "mst_a", "seq": 4},
            "https://m.example memory a,,b": {"store_id": "mst_a", "seq": 5},
        }});
        std::fs::write(cursors.path(), v1.to_string()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(cursors.path(), std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        let filtered = Key::new(
            "https://m.example",
            "memory",
            &["global".into(), "client:acme".into()],
            "machine".into(),
        );
        assert_eq!(
            cursors.saved(&key("machine")).unwrap(),
            Some(at("mst_a", 12))
        );
        assert_eq!(cursors.saved(&filtered).unwrap(), Some(at("mst_a", 9)));
        // A session never inherits the machine's cursor.
        assert_eq!(cursors.saved(&key("session:s-1")).unwrap(), None);
        // Reading writes nothing; the next advance writes version 2.
        assert_eq!(file(&cursors)["version"], 1);
        cursors
            .advance_at(&key("machine"), at("mst_a", 15), NOW)
            .unwrap();
        let json = file(&cursors);
        assert_eq!(json["version"], 2);
        let entries = json["cursors"].as_array().unwrap();
        assert_eq!(entries.len(), 2, "{json}");
        assert!(entries.iter().all(|e| e["reader"] == "machine"));
        assert!(
            entries.contains(&json!({"server": "https://m.example", "db": "memory",
            "scopes": ["client:acme", "global"], "reader": "machine",
            "store_id": "mst_a", "seq": 9, "seen_at": NOW}))
        );
        assert_eq!(
            cursors.saved(&key("machine")).unwrap(),
            Some(at("mst_a", 15))
        );
    }

    #[test]
    fn unused_session_cursors_are_pruned_at_the_next_write() {
        let (_dir, cursors) = cursors();
        let ago = |days: u64| NOW - days * DAY;
        for (reader, days) in [
            ("session:old", 8),
            ("session:recent", 6),
            ("name:nightly", 400),
            ("machine", 400),
        ] {
            cursors
                .advance_at(&key(reader), at("mst_a", 5), ago(days))
                .unwrap();
        }
        // A run that does not move the cursor still marks it seen.
        cursors
            .advance_at(&key("session:recent"), at("mst_a", 4), ago(1))
            .unwrap();
        cursors
            .advance_at(&key("session:now"), at("mst_a", 5), NOW)
            .unwrap();
        let entries = || file(&cursors)["cursors"].as_array().unwrap().clone();
        let readers: Vec<String> = entries()
            .iter()
            .map(|e| e["reader"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            readers,
            ["name:nightly", "machine", "session:recent", "session:now"]
        );
        assert_eq!(cursors.saved(&key("session:old")).unwrap(), None);
        let recent = entries()
            .into_iter()
            .find(|e| e["reader"] == "session:recent")
            .unwrap();
        assert_eq!(
            (recent["seq"].clone(), recent["seen_at"].clone()),
            (json!(5), json!(ago(1)))
        );
        // Seven days after that run it is still there; a day later it goes.
        let write = |now| {
            cursors
                .advance_at(&key("name:nightly"), at("mst_a", 6), now)
                .unwrap()
        };
        write(ago(1) + 7 * DAY);
        assert!(cursors.saved(&key("session:recent")).unwrap().is_some());
        write(ago(1) + 8 * DAY);
        assert_eq!(cursors.saved(&key("session:recent")).unwrap(), None);
        assert!(cursors.saved(&key("machine")).unwrap().is_some());
    }
}
