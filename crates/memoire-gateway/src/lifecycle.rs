//! What happens to databases without a request: the sweeper expires
//! burstable databases past their idle TTL or lifetime, persistent ones are
//! snapshotted and their old snapshots pruned, and the reconciler repairs
//! drift between the files and the catalog.
//!
//! The sweep and the snapshots are Merezha jobs on the `memoire` queue that
//! carry themselves forward: each run first enqueues the next slot of its
//! interval (deduplicated by the slot, so several instances or retries make
//! one job per slot), then does its work. The module's start seeds both.
use crate::{
    catalog::{Catalog, System, random_hex},
    config::Settings,
    data::DataPlane,
    last_used::LastUse,
    routes::Entry,
    store::{self, SnapshotRow},
};
use memoire_api::{DatabaseKind, DatabaseState};
use merezha_domain::{Failure, Identity};
use merezha_platform::{
    BoxFuture, Database,
    ports::{DedupeKey, JobContext, JobFailure, JobHandler, JobsHandle, MessageContext, NewJob},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// The queue the lifecycle jobs run on.
pub const QUEUE: &str = "memoire";
/// A file the catalog does not hold is removed once it has been left alone
/// this long, so a create whose file is being written is never mistaken for
/// an orphan.
pub const ORPHAN_AGE: Duration = Duration::from_secs(300);

/// One of the recurring tasks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task {
    Sweep,
    Snapshot,
}

impl Task {
    pub fn kind(self) -> &'static str {
        match self {
            Self::Sweep => "memoire.sweep",
            Self::Snapshot => "memoire.snapshot",
        }
    }
}

/// The lifecycle's dependencies.
pub struct Lifecycle {
    pub catalog: Catalog,
    pub data: Arc<DataPlane>,
    pub last_used: Arc<LastUse>,
    pub database: Database,
    pub settings: Arc<Settings>,
    sweeping: tokio::sync::Mutex<()>,
    snapshotting: tokio::sync::Mutex<()>,
}

impl Lifecycle {
    pub fn new(
        catalog: Catalog,
        data: Arc<DataPlane>,
        last_used: Arc<LastUse>,
        database: Database,
        settings: Arc<Settings>,
    ) -> Self {
        Self {
            catalog,
            data,
            last_used,
            database,
            settings,
            sweeping: tokio::sync::Mutex::new(()),
            snapshotting: tokio::sync::Mutex::new(()),
        }
    }

    fn interval(&self, task: Task) -> Duration {
        Duration::from_secs(match task {
            Task::Sweep => self.settings.sweep_interval_seconds,
            Task::Snapshot => self.settings.snapshot_interval_seconds,
        })
    }

    /// Every team that may hold a database: the recorded ones and the ones
    /// with a directory.
    async fn teams(&self) -> Result<BTreeSet<String>, Failure> {
        let mut teams: BTreeSet<String> = store::teams(&self.database).await?.into_iter().collect();
        teams.extend(self.data.team_dirs());
        Ok(teams)
    }

    /// The team's catalog entries, read as the gateway itself; with
    /// `after`, once the projection reached that command.
    async fn entries(
        &self,
        team: &str,
        after: Option<&merezha_domain::Receipt>,
    ) -> Result<Vec<Entry>, Failure> {
        let value = self
            .catalog
            .query(System::new(team)?, "ListDatabases", json!({}), after)
            .await?;
        serde_json::from_value(value["databases"].clone())
            .map_err(|_| Failure::transient("internal"))
    }

    /// Expires every burstable database past its deadline, then removes
    /// the files the catalog no longer holds.
    pub async fn sweep(&self) -> Result<(), Failure> {
        let _running = self.sweeping.lock().await;
        for team in self.teams().await? {
            if let Err(failure) = self.sweep_team(&team).await {
                merezha_telemetry::event!(
                    WARN,
                    "memoire.sweep_failed",
                    code = failure.code.as_str()
                );
            }
        }
        Ok(())
    }

    async fn sweep_team(&self, team: &str) -> Result<(), Failure> {
        let uses = self.last_used.team(&self.database, team).await?;
        let now = now_unix_ms();
        let mut last = None;
        for entry in self.entries(team, None).await? {
            if entry.state != DatabaseState::Active {
                continue;
            }
            // The freshest use: a query records it before it runs.
            let used = uses
                .get(&entry.id)
                .copied()
                .max(self.last_used.seen(team, &entry.id));
            let Some(deadline) = entry.expires_unix_ms(used) else {
                continue;
            };
            if now < deadline {
                continue;
            }
            let mut payload = json!({"name": entry.name, "id": entry.id, "now_unix_ms": now});
            if let Some(used) = used {
                payload["last_used_unix_ms"] = json!(used);
            }
            match self
                .catalog
                .command(System::new(team)?, "ExpireDatabase", payload)
                .await
            {
                Ok(receipt) => {
                    last = Some(receipt);
                    merezha_telemetry::event!(INFO, "memoire.database_expired");
                    self.data.remove(team, &entry.id).await?;
                    let _ = self.last_used.forget(&self.database, team, &entry.id).await;
                }
                // A use the sweep had not seen yet: the guest decides.
                Err(failure) if failure.code == "not_expired" => {}
                Err(failure) => return Err(failure),
            }
        }
        // Orphans only: a file that looks missing under a lagging read may
        // be one being deleted.
        self.reconcile_team(team, ORPHAN_AGE, false, last.as_ref())
            .await
    }

    /// The startup reconciler: removes the files and snapshots of
    /// databases the catalog does not hold as active (once left alone for
    /// `min_age`), and gives an active database whose file is missing its
    /// latest snapshot, or an empty file.
    pub async fn reconcile(&self, min_age: Duration) -> Result<(), Failure> {
        let mut failed = None;
        for team in self.teams().await? {
            if let Err(failure) = self.reconcile_team(&team, min_age, true, None).await {
                failed = Some(failure);
            }
        }
        // The caller retries the pass while any team failed.
        failed.map_or(Ok(()), Err)
    }

    async fn reconcile_team(
        &self,
        team: &str,
        min_age: Duration,
        repair_missing: bool,
        after: Option<&merezha_domain::Receipt>,
    ) -> Result<(), Failure> {
        let entries = self.entries(team, after).await?;
        let active: BTreeSet<&str> = entries
            .iter()
            .filter(|e| e.state == DatabaseState::Active)
            .map(|e| e.id.as_str())
            .collect();
        let held: BTreeSet<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        for (id, age) in self.data.team_files(team) {
            if !active.contains(id.as_str()) && age >= min_age {
                merezha_telemetry::event!(INFO, "memoire.reconcile_removed");
                self.data.remove(team, &id).await?;
                let _ = self.last_used.forget(&self.database, team, &id).await;
            }
        }
        for id in self.data.snapshot_dirs(team) {
            if !held.contains(id.as_str()) {
                self.data.remove_snapshots(team, &id).await?;
                store::delete_snapshots_of(&self.database, team, &id).await?;
            }
        }
        if !repair_missing {
            return Ok(());
        }
        let mut failed = None;
        for entry in entries.iter().filter(|e| e.state == DatabaseState::Active) {
            if self.data.size(team, &entry.id).is_some() {
                continue;
            }
            if let Err(failure) = self.repair(team, &entry.id).await {
                failed = Some(failure);
            }
        }
        failed.map_or(Ok(()), Err)
    }

    /// Gives a database whose file is missing its newest snapshot that is
    /// still on disk, or an empty file.
    async fn repair(&self, team: &str, id: &str) -> Result<(), Failure> {
        for snapshot in store::snapshots(&self.database, team, id).await? {
            let source = self.data.snapshot_path(team, id, &snapshot.id)?;
            match std::fs::metadata(&source) {
                Ok(meta) if meta.is_file() => {
                    self.data.recreate_from(team, id, source).await?;
                    merezha_telemetry::event!(WARN, "memoire.reconcile_restored");
                    return Ok(());
                }
                // Gone: the next older snapshot, then an empty file.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                // Unreadable is not absent: no empty file over it.
                _ => return Err(Failure::transient("data_dir_unavailable")),
            }
        }
        self.data.create(team, id).await?;
        merezha_telemetry::event!(WARN, "memoire.reconcile_created");
        Ok(())
    }

    /// Snapshots every active persistent database, then prunes the
    /// snapshots past the retention.
    pub async fn snapshot_all(&self) -> Result<(), Failure> {
        let _running = self.snapshotting.lock().await;
        for team in self.teams().await? {
            let entries = match self.entries(&team, None).await {
                Ok(entries) => entries,
                Err(failure) => {
                    merezha_telemetry::event!(
                        WARN,
                        "memoire.snapshot_failed",
                        code = failure.code.as_str()
                    );
                    continue;
                }
            };
            for entry in entries {
                if entry.kind != DatabaseKind::Persistent || entry.state != DatabaseState::Active {
                    continue;
                }
                if let Err(failure) = self.snapshot(&team, &entry, "scheduled").await {
                    merezha_telemetry::event!(
                        WARN,
                        "memoire.snapshot_failed",
                        code = failure.code.as_str()
                    );
                }
            }
        }
        self.prune().await
    }

    /// Takes one snapshot of `entry`.
    pub async fn snapshot(
        &self,
        team: &str,
        entry: &Entry,
        reason: &str,
    ) -> Result<SnapshotRow, Failure> {
        let id = format!("snap_{}", random_hex()?);
        let path = self.data.snapshot_path(team, &entry.id, &id)?;
        let created_unix_ms = now_unix_ms();
        let size_bytes = self
            .data
            .copy_to(&entry.target(team), path.clone())
            .await
            .map_err(crate::data::refusal_failure)?;
        let row = SnapshotRow {
            id,
            created_unix_ms,
            size_bytes,
            reason: reason.to_owned(),
        };
        if let Err(failure) = store::insert_snapshot(&self.database, team, &entry.id, &row).await {
            let _ = std::fs::remove_file(&path);
            return Err(failure);
        }
        Ok(row)
    }

    /// Removes snapshots older than the retention, rows first so a listed
    /// snapshot always has its file.
    async fn prune(&self) -> Result<(), Failure> {
        let retention = self
            .settings
            .snapshot_retention_seconds
            .saturating_mul(1000);
        let cutoff = now_unix_ms().saturating_sub(retention);
        loop {
            let expired = store::expired_snapshots(&self.database, cutoff).await?;
            if expired.is_empty() {
                return Ok(());
            }
            let mut stuck = false;
            for (id, team, database) in expired {
                // The file first: a row stays until its file is gone, so a
                // failed unlink is retried by the next run.
                let removed = match self.data.snapshot_path(&team, &database, &id) {
                    Ok(path) => match std::fs::remove_file(path) {
                        Ok(()) => true,
                        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
                    },
                    Err(_) => true,
                };
                if removed {
                    store::delete_snapshot(&self.database, &id).await?;
                } else {
                    stuck = true;
                }
            }
            if stuck {
                return Ok(());
            }
        }
    }
}

/// The job handler of one recurring task.
pub struct Tick {
    pub lifecycle: Arc<Lifecycle>,
    pub jobs: JobsHandle,
    pub task: Task,
}

impl JobHandler for Tick {
    fn handle<'a>(
        &'a self,
        _: JobContext,
        payload: Value,
    ) -> BoxFuture<'a, Result<(), JobFailure>> {
        Box::pin(async move {
            let slot = payload["slot"].as_u64().unwrap_or(0);
            // The next run first, so a failing one never ends the chain; if
            // it cannot be enqueued, this job fails after its work and the
            // runtime retries it, enqueueing again.
            let next = schedule(&self.lifecycle, &self.jobs, self.task, Some(slot)).await;
            let done = match self.task {
                Task::Sweep => self.lifecycle.sweep().await,
                Task::Snapshot => self.lifecycle.snapshot_all().await,
            };
            // An enqueue failure wins: it is what keeps the chain going.
            next.and(done).map_err(JobFailure::from)
        })
    }
}

/// Enqueues the task's next slot: the one after `after`, or after now.
pub async fn schedule(
    lifecycle: &Lifecycle,
    jobs: &JobsHandle,
    task: Task,
    after: Option<u64>,
) -> Result<(), Failure> {
    let every = u64::try_from(lifecycle.interval(task).as_millis())
        .unwrap_or(u64::MAX)
        .max(1);
    let now_slot = now_unix_ms() / every;
    let slot = now_slot.max(after.unwrap_or(0)) + 1;
    let run_at = i64::try_from(slot.saturating_mul(every))
        .ok()
        .and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok());
    jobs.enqueue(NewJob {
        queue: Identity::new(QUEUE)?,
        kind: Identity::new(task.kind())?,
        idempotency_key: DedupeKey::new(format!("{}:{every}:{slot}", task.kind()))?,
        payload: json!({"slot": slot}),
        contains_secret: false,
        run_at,
        priority: 0,
        context: MessageContext {
            correlation_id: Identity::new(random_hex()?)?,
            causation_id: None,
            tenant: None,
        },
    })
    .await
    .map(|_| ())
}

pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
