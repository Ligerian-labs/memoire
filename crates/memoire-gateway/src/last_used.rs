//! When each database was last used: updated in memory on every query and
//! import, flushed periodically to the module's own schema
//! (`memoire.last_used`). It is not an event per query.
use merezha_domain::Failure;
use merezha_platform::{Database, Migration};
use merezha_postgres::{Statement, classify};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

/// The module's first migration: the last-use table.
pub const MIGRATION: Migration = Migration::new(
    1,
    "last_used",
    "CREATE SCHEMA IF NOT EXISTS memoire;
     CREATE TABLE memoire.last_used (
         team_id text NOT NULL,
         database_id text NOT NULL,
         last_used_unix_ms bigint NOT NULL,
         PRIMARY KEY (team_id, database_id)
     );",
);

const UPSERT: Statement = Statement::new(
    "memoire.last_used.upsert",
    "INSERT memoire.last_used",
    "INSERT INTO memoire.last_used (team_id, database_id, last_used_unix_ms) VALUES ($1, $2, $3)
     ON CONFLICT (team_id, database_id)
     DO UPDATE SET last_used_unix_ms = GREATEST(memoire.last_used.last_used_unix_ms, EXCLUDED.last_used_unix_ms)",
);
const TEAM: Statement = Statement::new(
    "memoire.last_used.team",
    "SELECT memoire.last_used",
    "SELECT database_id, last_used_unix_ms FROM memoire.last_used WHERE team_id = $1",
);
const DELETE: Statement = Statement::new(
    "memoire.last_used.delete",
    "DELETE memoire.last_used",
    "DELETE FROM memoire.last_used WHERE team_id = $1 AND database_id = $2",
);

/// Every statement this module runs.
pub const STATEMENTS: &[Statement] = &[UPSERT, TEAM, DELETE];

/// Time one flush or read may take.
const BUDGET: Duration = Duration::from_secs(5);

type Key = (String, String);

/// The last uses this process saw, and the ones not flushed yet.
#[derive(Default)]
pub struct LastUse {
    seen: Mutex<HashMap<Key, u64>>,
    pending: Mutex<HashMap<Key, u64>>,
}

impl LastUse {
    /// Records a use of `database` of `team` at `unix_ms`.
    pub fn touch(&self, team: &str, database: &str, unix_ms: u64) {
        let key = (team.to_owned(), database.to_owned());
        for map in [&self.seen, &self.pending] {
            let mut map = map.lock().unwrap_or_else(|p| p.into_inner());
            let at = map.entry(key.clone()).or_insert(unix_ms);
            *at = (*at).max(unix_ms);
        }
    }

    /// Forgets a removed database, here and in the store.
    pub async fn forget(&self, db: &Database, team: &str, database: &str) -> Result<(), Failure> {
        let key = (team.to_owned(), database.to_owned());
        for map in [&self.seen, &self.pending] {
            map.lock().unwrap_or_else(|p| p.into_inner()).remove(&key);
        }
        let (team, database) = key;
        let store = db.clone();
        db.transaction(Instant::now() + BUDGET, move |tx| {
            Box::pin(async move {
                store
                    .execute(tx, &DELETE, &[&team, &database])
                    .await
                    .map_err(|e| classify(&e))?;
                Ok(())
            })
        })
        .await
    }

    /// The team's last uses: the store's, overlaid with this process's.
    pub async fn team(&self, db: &Database, team: &str) -> Result<HashMap<String, u64>, Failure> {
        let owned = team.to_owned();
        let store = db.clone();
        let mut uses: HashMap<String, u64> = db
            .transaction(Instant::now() + BUDGET, move |tx| {
                Box::pin(async move {
                    let rows = store
                        .query(tx, &TEAM, &[&owned])
                        .await
                        .map_err(|e| classify(&e))?;
                    Ok(rows
                        .iter()
                        .map(|row| {
                            let at: i64 = row.get(1);
                            (row.get::<_, String>(0), u64::try_from(at).unwrap_or(0))
                        })
                        .collect())
                })
            })
            .await?;
        let seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        for ((t, database), at) in seen.iter() {
            if t == team {
                let entry = uses.entry(database.clone()).or_insert(*at);
                *entry = (*entry).max(*at);
            }
        }
        Ok(uses)
    }

    /// Writes the pending uses to the store. They stay pending until the
    /// write committed, so a failed or interrupted flush loses none.
    pub async fn flush(&self, db: &Database) -> Result<(), Failure> {
        let pending: Vec<(Key, u64)> = {
            let map = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            map.iter().map(|(k, v)| (k.clone(), *v)).collect()
        };
        if pending.is_empty() {
            return Ok(());
        }
        let store = db.clone();
        let batch = pending.clone();
        let written = db
            .transaction(Instant::now() + BUDGET, move |tx| {
                Box::pin(async move {
                    for ((team, database), at) in &batch {
                        let at = i64::try_from(*at).unwrap_or(i64::MAX);
                        store
                            .execute(tx, &UPSERT, &[team, database, &at])
                            .await
                            .map_err(|e| classify(&e))?;
                    }
                    Ok(())
                })
            })
            .await;
        if written.is_ok() {
            // Keep what arrived meanwhile.
            let mut map = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            for (key, at) in pending {
                if map.get(&key) == Some(&at) {
                    map.remove(&key);
                }
            }
        }
        written
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_statements_are_registered_once() {
        merezha_postgres::check_registry(&[merezha_postgres::KERNEL_STATEMENTS, super::STATEMENTS])
            .unwrap();
    }
}
