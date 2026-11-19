//! The data plane: the SQLite files, one writer and a few readers per open
//! database, and the requests that run on them.
//!
//! Files live at `<data_dir>/teams/<team>/<db>.sqlite` in WAL mode. The
//! catalog decides which exist; the routes create a file after an accepted
//! create and remove it after a delete. A burstable database's connections
//! close once it has been idle for `idle_close`, and reopen on the next
//! request.
pub mod browse;
pub mod connection;
pub mod exec;
pub mod request;

use connection::{BUSY_TIMEOUT, Opened};
use exec::Refused;
use memoire_api::{Rows, Schema, StatementResult};
use merezha_domain::Failure;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

/// Reader connections per open database.
const READERS: usize = 4;
/// The suffixes SQLite puts beside a database file.
const SIDE_FILES: &[&str] = &["-wal", "-shm", "-journal"];

/// Which file a request is for.
#[derive(Clone, Debug)]
pub struct Target {
    pub team: String,
    pub id: String,
    pub burstable: bool,
}

/// One open database.
struct Handle {
    path: PathBuf,
    burstable: bool,
    writer: Arc<tokio::sync::Mutex<Option<Opened>>>,
    readers: Mutex<Vec<Opened>>,
    slots: Arc<Semaphore>,
    /// Milliseconds since the plane's epoch of the last request.
    used: AtomicU64,
}

/// The data plane.
pub struct DataPlane {
    root: PathBuf,
    teams: PathBuf,
    max_bytes: u64,
    idle_close: Duration,
    epoch: Instant,
    open: Mutex<HashMap<PathBuf, Arc<Handle>>>,
}

impl DataPlane {
    /// The plane over `<root>/teams`, with snapshots under
    /// `<root>/snapshots` and scratch files under `<root>/tmp`.
    pub fn new(root: PathBuf, max_bytes: u64, idle_close: Duration) -> Self {
        Self {
            teams: root.join("teams"),
            root,
            max_bytes,
            idle_close,
            epoch: Instant::now(),
            open: Mutex::new(HashMap::new()),
        }
    }

    /// `<teams>/<team>/<db>.sqlite`, for ids of the expected shapes only.
    fn path(&self, team: &str, id: &str) -> Result<PathBuf, Failure> {
        let team_ok = !team.is_empty()
            && team.len() <= 64
            && team.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        let id_ok = id
            .strip_prefix("db_")
            .is_some_and(|hex| hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
        if team_ok && id_ok {
            Ok(self.teams.join(team).join(format!("{id}.sqlite")))
        } else {
            Err(Failure::permanent("invalid_database_path"))
        }
    }

    /// Creates the empty file of a database the catalog accepted; an
    /// existing file is kept, so a retry is harmless.
    pub async fn create(&self, team: &str, id: &str) -> Result<(), Failure> {
        let path = self.path(team, id)?;
        blocking(move || {
            if let Some(dir) = path.parent() {
                private_dir(dir).map_err(|_| Failure::transient("data_dir_unavailable"))?;
            }
            // Side files without their main file are stale: SQLite would
            // replay them into the new one.
            if !path.exists() {
                remove_side_files(&path).map_err(|_| Failure::transient("data_dir_unavailable"))?;
            }
            connection::create(&path).map_err(|_| Failure::transient("data_dir_unavailable"))
        })
        .await
    }

    /// Closes the database and removes its files. Requests already running
    /// on it finish first (within twice the busy timeout); then its
    /// connections close, so none recreates a side file after the unlink.
    pub async fn remove(&self, team: &str, id: &str) -> Result<(), Failure> {
        let path = self.path(team, id)?;
        let handle = self.lock().remove(&path);
        if let Some(handle) = handle {
            let drained = tokio::time::timeout(BUSY_TIMEOUT * 2, drain(&handle)).await;
            let Ok((mut writer, _readers)) = drained else {
                // Still in use: the files stay, and the startup reconciler
                // removes them, as it does any file the catalog no longer
                // holds.
                merezha_telemetry::event!(WARN, "memoire.remove_deferred");
                return Ok(());
            };
            *writer = None;
            handle
                .readers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
        }
        blocking(move || {
            for suffix in std::iter::once("").chain(SIDE_FILES.iter().copied()) {
                let mut file = path.clone().into_os_string();
                file.push(suffix);
                match std::fs::remove_file(&file) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(Failure::transient("data_dir_unavailable")),
                }
            }
            Ok(())
        })
        .await
    }

    /// The file's size with its write-ahead log, whose pages are the
    /// database's until a checkpoint moves them; `None` when the file is
    /// not on disk.
    pub fn size(&self, team: &str, id: &str) -> Option<u64> {
        let path = self.path(team, id).ok()?;
        let main = std::fs::metadata(&path).ok()?.len();
        let mut wal = path.into_os_string();
        wal.push("-wal");
        Some(main + std::fs::metadata(wal).map_or(0, |m| m.len()))
    }

    /// Whether the database's connections are open, for tests of the idle
    /// close.
    pub fn is_open(&self, team: &str, id: &str) -> bool {
        self.path(team, id)
            .is_ok_and(|path| self.lock().contains_key(&path))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, Arc<Handle>>> {
        self.open
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn handle(&self, target: &Target) -> Result<Arc<Handle>, Refused> {
        let path = self
            .path(&target.team, &target.id)
            .map_err(|_| Refused::Code("database_unavailable"))?;
        if !path.is_file() {
            return Err(Refused::Code("database_unavailable"));
        }
        let now = self.now();
        let mut open = self.lock();
        let handle = open
            .entry(path.clone())
            .or_insert_with(|| {
                Arc::new(Handle {
                    path,
                    burstable: target.burstable,
                    writer: Arc::new(tokio::sync::Mutex::new(None)),
                    readers: Mutex::new(Vec::new()),
                    slots: Arc::new(Semaphore::new(READERS)),
                    used: AtomicU64::new(now),
                })
            })
            .clone();
        handle.used.store(now, Ordering::Relaxed);
        Ok(handle)
    }

    fn now(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Runs `work` on the database's writer, opened on first use.
    async fn on_writer<T: Send + 'static>(
        &self,
        target: &Target,
        work: impl FnOnce(&mut Opened) -> Result<T, Refused> + Send + 'static,
    ) -> Result<T, Refused> {
        let handle = self.handle(target)?;
        let mut writer = tokio::time::timeout(BUSY_TIMEOUT, handle.writer.clone().lock_owned())
            .await
            .map_err(|_| Refused::Code("sqlite_busy"))?;
        let (path, max_bytes) = (handle.path.clone(), self.max_bytes);
        let result = tokio::task::spawn_blocking(move || {
            if writer.is_none() {
                *writer = Some(
                    connection::open(&path, true, max_bytes)
                        .map_err(|_| Refused::Code("database_unavailable"))?,
                );
            }
            let opened = writer
                .as_mut()
                .ok_or(Refused::Code("database_unavailable"))?;
            work(opened)
        })
        .await
        .map_err(|_| Refused::Code("internal"))?;
        handle.used.store(self.now(), Ordering::Relaxed);
        result
    }

    /// Runs `work` on a read-only reader.
    async fn on_reader<T: Send + 'static>(
        &self,
        target: &Target,
        work: impl FnOnce(&Opened) -> Result<T, Refused> + Send + 'static,
    ) -> Result<T, Refused> {
        let handle = self.handle(target)?;
        let permit = tokio::time::timeout(BUSY_TIMEOUT, handle.slots.clone().acquire_owned())
            .await
            .map_err(|_| Refused::Code("sqlite_busy"))?
            .map_err(|_| Refused::Code("internal"))?;
        let (inner, max_bytes) = (handle.clone(), self.max_bytes);
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let pooled = inner
                .readers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .pop();
            let opened = match pooled {
                Some(opened) => opened,
                None => connection::open(&inner.path, false, max_bytes)
                    .map_err(|_| Refused::Code("database_unavailable"))?,
            };
            let result = work(&opened);
            inner
                .readers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(opened);
            result
        })
        .await
        .map_err(|_| Refused::Code("internal"))?;
        handle.used.store(self.now(), Ordering::Relaxed);
        result
    }

    /// Runs a batch: `write` mode on the writer, `read` on a reader.
    pub async fn query(
        &self,
        target: &Target,
        batch: request::Batch,
    ) -> Result<Vec<StatementResult>, Refused> {
        let write = batch.mode == memoire_api::QueryMode::Write;
        if write {
            self.on_writer(target, move |opened| {
                exec::run(&opened.connection, &opened.guard, &batch)
            })
            .await
        } else {
            self.on_reader(target, move |opened| {
                exec::run(&opened.connection, &opened.guard, &batch)
            })
            .await
        }
    }

    /// The schema.
    pub async fn schema(&self, target: &Target) -> Result<Schema, Refused> {
        self.on_reader(target, |opened| {
            let connection = &opened.connection;
            browse::bounded(connection, || {
                browse::schema(connection).map_err(browse::Refused::from)
            })
            .map_err(browse_refusal)
        })
        .await
    }

    /// A page of rows.
    pub async fn rows(
        &self,
        target: &Target,
        object: String,
        limit: usize,
        after: Option<String>,
    ) -> Result<Rows, Refused> {
        let database = target.id.clone();
        self.on_reader(target, move |opened| {
            let connection = &opened.connection;
            browse::bounded(connection, || {
                browse::rows(connection, &database, &object, limit, after.as_deref())
            })
            .map_err(browse_refusal)
        })
        .await
    }

    /// A fresh path under `<root>/tmp` for a file being built.
    pub fn scratch(&self) -> Result<PathBuf, Failure> {
        let dir = self.root.join("tmp");
        private_dir(&dir).map_err(|_| Failure::transient("data_dir_unavailable"))?;
        Ok(dir.join(format!("{}.sqlite", crate::catalog::random_hex()?)))
    }

    /// `<root>/snapshots/<team>/<db>/<snapshot>.sqlite`.
    pub fn snapshot_path(&self, team: &str, id: &str, snapshot: &str) -> Result<PathBuf, Failure> {
        let database = self.path(team, id)?;
        let ok = snapshot
            .strip_prefix("snap_")
            .is_some_and(|hex| hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
        if !ok {
            return Err(Failure::permanent("invalid_database_path"));
        }
        let _ = database;
        Ok(self
            .root
            .join("snapshots")
            .join(team)
            .join(id)
            .join(format!("{snapshot}.sqlite")))
    }

    /// Removes every snapshot file of a database.
    pub async fn remove_snapshots(&self, team: &str, id: &str) -> Result<(), Failure> {
        self.path(team, id)?;
        let dir = self.root.join("snapshots").join(team).join(id);
        blocking(move || match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(Failure::transient("data_dir_unavailable")),
        })
        .await
    }

    /// A consistent copy of the database at `destination` (SQLite's online
    /// backup in one pass, from a reader); its size.
    pub async fn copy_to(&self, target: &Target, destination: PathBuf) -> Result<u64, Refused> {
        self.on_reader(target, move |opened| {
            let failed = |_| Refused::Code("data_dir_unavailable");
            if let Some(dir) = destination.parent() {
                private_dir(dir).map_err(failed)?;
            }
            let mut copy = rusqlite::Connection::open(&destination)
                .map_err(|_| Refused::Code("data_dir_unavailable"))?;
            let done = rusqlite::backup::Backup::new(&opened.connection, &mut copy)
                .and_then(|backup| backup.step(-1))
                .map(|step| step == rusqlite::backup::StepResult::Done);
            drop(copy);
            if !matches!(done, Ok(true)) {
                let _ = std::fs::remove_file(&destination);
                return Err(Refused::Code("sqlite_busy"));
            }
            std::fs::metadata(&destination)
                .map(|m| m.len())
                .map_err(|_| Refused::Code("data_dir_unavailable"))
        })
        .await
    }

    /// Whether the database holds no user object.
    pub async fn is_empty(&self, target: &Target) -> Result<bool, Refused> {
        self.on_reader(target, |opened| {
            is_empty(&opened.connection).map_err(|_| Refused::Code("internal"))
        })
        .await
    }

    /// Replaces the database with the file at `source` in one step (a
    /// rename), once every request on it finished; with `require_empty`
    /// only while it holds no user object (`database_not_empty`). With
    /// `keep`, a consistent copy of the current contents goes there first,
    /// in the same quiet moment, so no write falls between the copy and the
    /// swap; its size is returned.
    pub async fn replace_with(
        &self,
        target: &Target,
        source: PathBuf,
        require_empty: bool,
        keep: Option<PathBuf>,
    ) -> Result<Option<u64>, Refused> {
        let handle = self.handle(target)?;
        let (mut writer, readers) = tokio::time::timeout(BUSY_TIMEOUT * 2, drain(&handle))
            .await
            .map_err(|_| Refused::Code("sqlite_busy"))?;
        let readers = readers.map_err(|_| Refused::Code("internal"))?;
        let (inner, max_bytes) = (handle.clone(), self.max_bytes);
        // The guards move into the blocking work: a cancelled request cannot
        // release them while the swap runs.
        let result = tokio::task::spawn_blocking(move || {
            let _readers = readers;
            if require_empty {
                let opened = connection::open(&inner.path, false, max_bytes)
                    .map_err(|_| Refused::Code("database_unavailable"))?;
                if !is_empty(&opened.connection).map_err(|_| Refused::Code("internal"))? {
                    return Err(Refused::Code("database_not_empty"));
                }
            }
            *writer = None;
            inner
                .readers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
            let kept = match keep {
                Some(destination) => Some(copy_file(&inner.path, &destination)?),
                None => None,
            };
            remove_side_files(&inner.path).map_err(|_| Refused::Code("data_dir_unavailable"))?;
            remove_side_files(&source).map_err(|_| Refused::Code("data_dir_unavailable"))?;
            std::fs::rename(&source, &inner.path)
                .map_err(|_| Refused::Code("data_dir_unavailable"))?;
            connection::wal(&inner.path).map_err(|_| Refused::Code("internal"))?;
            Ok(kept)
        })
        .await
        .map_err(|_| Refused::Code("internal"))?;
        handle.used.store(self.now(), Ordering::Relaxed);
        result
    }

    /// The databases of `team` with a snapshot directory.
    pub fn snapshot_dirs(&self, team: &str) -> Vec<String> {
        std::fs::read_dir(self.root.join("snapshots").join(team))
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .collect()
    }

    /// Gives a database whose file is missing a copy of `source`.
    pub async fn recreate_from(
        &self,
        team: &str,
        id: &str,
        source: PathBuf,
    ) -> Result<(), Failure> {
        let path = self.path(team, id)?;
        let scratch = self.scratch()?;
        blocking(move || {
            let failed = |_| Failure::transient("data_dir_unavailable");
            if let Some(dir) = path.parent() {
                private_dir(dir).map_err(failed)?;
            }
            std::fs::copy(&source, &scratch).map_err(failed)?;
            if path.exists() {
                let _ = std::fs::remove_file(&scratch);
                return Ok(());
            }
            remove_side_files(&path).map_err(failed)?;
            std::fs::rename(&scratch, &path).map_err(failed)?;
            connection::wal(&path).map_err(|_| Failure::transient("internal"))
        })
        .await
    }

    /// The teams with a directory under `<root>/teams`.
    pub fn team_dirs(&self) -> Vec<String> {
        std::fs::read_dir(&self.teams)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .collect()
    }

    /// The database files of `team`: each id and how long ago the file
    /// last changed.
    pub fn team_files(&self, team: &str) -> Vec<(String, Duration)> {
        std::fs::read_dir(self.teams.join(team))
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let name = entry.file_name().into_string().ok()?;
                let id = name.strip_suffix(".sqlite")?.to_owned();
                let age = entry
                    .metadata()
                    .ok()?
                    .modified()
                    .ok()?
                    .elapsed()
                    .unwrap_or_default();
                Some((id, age))
            })
            .collect()
    }

    /// Closes the connections of burstable databases idle for
    /// `idle_close` that no request holds.
    pub fn close_idle(&self) {
        let now = self.now();
        let limit = u64::try_from(self.idle_close.as_millis()).unwrap_or(u64::MAX);
        self.lock().retain(|_, handle| {
            let idle = now.saturating_sub(handle.used.load(Ordering::Relaxed)) >= limit;
            !(handle.burstable && idle && Arc::strong_count(handle) == 1)
        });
    }

    /// How often [`DataPlane::close_idle`] should run.
    pub fn idle_check(&self) -> Duration {
        (self.idle_close / 4).clamp(Duration::from_millis(100), Duration::from_secs(30))
    }
}

/// Waits for every request on `handle` to finish: its writer and every
/// reader slot, held until the guards drop.
async fn drain(
    handle: &Handle,
) -> (
    tokio::sync::OwnedMutexGuard<Option<Opened>>,
    Result<tokio::sync::OwnedSemaphorePermit, tokio::sync::AcquireError>,
) {
    let writer = handle.writer.clone().lock_owned().await;
    let readers = handle
        .slots
        .clone()
        .acquire_many_owned(READERS as u32)
        .await;
    (writer, readers)
}

/// A consistent copy of the closed database at `source` (its WAL
/// included) to `destination`, through SQLite's backup in one pass; its
/// size.
fn copy_file(source: &Path, destination: &Path) -> Result<u64, Refused> {
    let failed = |_: std::io::Error| Refused::Code("data_dir_unavailable");
    if let Some(dir) = destination.parent() {
        private_dir(dir).map_err(failed)?;
    }
    let from = rusqlite::Connection::open_with_flags(
        source,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| Refused::Code("database_unavailable"))?;
    let mut to = rusqlite::Connection::open(destination)
        .map_err(|_| Refused::Code("data_dir_unavailable"))?;
    let done = rusqlite::backup::Backup::new(&from, &mut to)
        .and_then(|backup| backup.step(-1))
        .map(|step| step == rusqlite::backup::StepResult::Done);
    drop(to);
    if !matches!(done, Ok(true)) {
        let _ = std::fs::remove_file(destination);
        return Err(Refused::Code("sqlite_busy"));
    }
    std::fs::metadata(destination)
        .map(|m| m.len())
        .map_err(failed)
}

/// Whether a database holds no user table, view, index or trigger.
fn is_empty(connection: &rusqlite::Connection) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT count(*) = 0 FROM sqlite_schema WHERE name NOT LIKE 'sqlite\\_%' ESCAPE '\\'",
        [],
        |row| row.get(0),
    )
}

/// Removes a database file's side files.
fn remove_side_files(path: &Path) -> std::io::Result<()> {
    for suffix in SIDE_FILES {
        let mut file = path.to_path_buf().into_os_string();
        file.push(suffix);
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// A refusal as a classified failure, for callers outside a route.
pub fn refusal_failure(refused: Refused) -> Failure {
    match refused {
        Refused::Code(
            code @ ("sqlite_busy" | "database_unavailable" | "internal" | "data_dir_unavailable"),
        ) => Failure::transient(code),
        Refused::Code(code) => Failure::permanent(code),
        Refused::Invalid { .. } => Failure::permanent("validation_failed"),
        Refused::Sql { .. } => Failure::permanent("sql_error"),
    }
}

fn browse_refusal(refused: browse::Refused) -> Refused {
    Refused::Code(match refused {
        browse::Refused::TableNotFound => "table_not_found",
        browse::Refused::InvalidPage => "invalid_page",
        browse::Refused::TooLarge => "result_too_large",
        browse::Refused::Timeout => "statement_timeout",
        browse::Refused::Sqlite => "internal",
    })
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| Failure::transient("internal"))?
}

/// Creates `dir` and its parents, the leaf mode 0700 on Unix.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}
