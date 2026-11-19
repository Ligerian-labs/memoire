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
    teams: PathBuf,
    max_bytes: u64,
    idle_close: Duration,
    epoch: Instant,
    open: Mutex<HashMap<PathBuf, Arc<Handle>>>,
}

impl DataPlane {
    pub fn new(teams: PathBuf, max_bytes: u64, idle_close: Duration) -> Self {
        Self {
            teams,
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
            connection::create(&path).map_err(|_| Failure::transient("data_dir_unavailable"))
        })
        .await
    }

    /// Closes the database and removes its files. Requests already running
    /// finish on the unlinked file first.
    pub async fn remove(&self, team: &str, id: &str) -> Result<(), Failure> {
        let path = self.path(team, id)?;
        self.lock().remove(&path);
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
        work: impl FnOnce(&Opened) -> Result<T, Refused> + Send + 'static,
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
                .as_ref()
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
        let work = move |opened: &Opened| exec::run(&opened.connection, &opened.guard, &batch);
        if write {
            self.on_writer(target, work).await
        } else {
            self.on_reader(target, work).await
        }
    }

    /// The schema.
    pub async fn schema(&self, target: &Target) -> Result<Schema, Refused> {
        self.on_reader(target, |opened| {
            browse::schema(&opened.connection).map_err(|_| Refused::Code("internal"))
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
            browse::rows(
                &opened.connection,
                &database,
                &object,
                limit,
                after.as_deref(),
            )
            .map_err(|refused| match refused {
                browse::Refused::TableNotFound => Refused::Code("table_not_found"),
                browse::Refused::InvalidPage => Refused::Code("invalid_page"),
                browse::Refused::Sqlite => Refused::Code("internal"),
            })
        })
        .await
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
