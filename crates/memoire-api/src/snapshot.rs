//! Snapshots of persistent databases.
use serde::{Deserialize, Serialize};

/// One snapshot, as `GET …/{name}/snapshots` lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    /// `snap_…`, opaque.
    pub id: String,
    pub created_at: String,
    pub size_bytes: u64,
    /// `scheduled` (every 6 hours) or `pre_restore` (taken just before a
    /// restore replaced the database).
    pub reason: SnapshotReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotReason {
    Scheduled,
    PreRestore,
}
