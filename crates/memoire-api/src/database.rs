//! Databases: the catalog entries a team owns.
use serde::{Deserialize, Serialize};

/// How long a database lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseKind {
    /// Scratch space: expires after its idle TTL or its absolute lifetime,
    /// never backed up.
    Burstable,
    /// Lives until deleted; snapshotted every 6 hours, snapshots kept 7 days.
    Persistent,
}

/// Where a database is in its lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseState {
    Active,
    /// A burstable database past its idle TTL or lifetime. Its file is gone;
    /// the entry stays listed until it is deleted or its name is reused.
    Expired,
    /// Never returned by the API; present for the catalog's own records.
    Deleted,
}

/// One database, as every database route returns it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Database {
    /// `db_…`, opaque and never reused.
    pub id: String,
    /// Unique among the team's active databases; see [`valid_name`].
    pub name: String,
    pub kind: DatabaseKind,
    pub state: DatabaseState,
    pub created_at: String,
    /// Last query, import or extension; `null` until the first one.
    pub last_used_at: Option<String>,
    /// File size; `null` when the file is not on disk (expired).
    pub size_bytes: Option<u64>,
    /// Burstable only, `null` for persistent databases.
    pub idle_ttl_seconds: Option<u64>,
    /// Burstable only: `min(last use + idle TTL, expires_at_max)`, where the
    /// last use is `last_used_at`, or `created_at` before the first use.
    pub expires_at: Option<String>,
    /// Burstable only: `created_at` plus the 7-day lifetime. Never moves.
    pub expires_at_max: Option<String>,
}

/// `POST /v1/memoire/databases`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateDatabase {
    pub name: String,
    pub kind: DatabaseKind,
    /// Burstable only; defaults to 24 hours. Between 60 seconds and 7 days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_ttl_seconds: Option<u64>,
}

/// `POST /v1/memoire/databases/{name}/extend`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtendDatabase {
    /// The new idle TTL; omitted keeps the current one. Either way the idle
    /// clock restarts now, and `expires_at` never passes `expires_at_max`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_ttl_seconds: Option<u64>,
}

/// A page of a cursor-paginated list, Merezha's list shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Pass back as `?cursor=` for the next page; `null` on the last page.
    pub next_cursor: Option<String>,
}

/// Whether `name` is a valid database name: `^[a-z][a-z0-9-]{1,62}$`, so 2 to
/// 63 bytes, a lowercase letter first, then lowercase letters, digits and
/// hyphens.
pub fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (2..=63).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_follow_the_slug_rule() {
        for ok in [
            "ab",
            "scratch",
            "a1",
            "my-db-2",
            &format!("a{}", "b".repeat(62)),
        ] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            "a",
            "1db",
            "-db",
            "Db",
            "my_db",
            "my db",
            "émoi",
            &format!("a{}", "b".repeat(63)),
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn create_rejects_unknown_fields_and_kinds() {
        let ok: CreateDatabase =
            serde_json::from_value(json!({"name": "scratch", "kind": "burstable"})).unwrap();
        assert_eq!(ok.idle_ttl_seconds, None);
        assert!(
            serde_json::from_value::<CreateDatabase>(
                json!({"name": "x", "kind": "burstable", "ttl": 1})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<CreateDatabase>(json!({"name": "x", "kind": "forever"}))
                .is_err()
        );
    }

    #[test]
    fn database_serializes_nulls_for_persistent() {
        let db = Database {
            id: "db_0192f5c4a1b27c3d8e9f00112233aabb".into(),
            name: "notes".into(),
            kind: DatabaseKind::Persistent,
            state: DatabaseState::Active,
            created_at: "2026-11-19T10:00:00Z".into(),
            last_used_at: None,
            size_bytes: Some(4096),
            idle_ttl_seconds: None,
            expires_at: None,
            expires_at_max: None,
        };
        assert_eq!(
            serde_json::to_value(&db).unwrap(),
            json!({
                "id": "db_0192f5c4a1b27c3d8e9f00112233aabb", "name": "notes",
                "kind": "persistent", "state": "active", "created_at": "2026-11-19T10:00:00Z",
                "last_used_at": null, "size_bytes": 4096, "idle_ttl_seconds": null,
                "expires_at": null, "expires_at_max": null,
            })
        );
    }
}
