//! The module's other tables: the teams that ever held a database, so the
//! sweeper, the snapshots and the reconciler can visit every team, and the
//! snapshots' records.
use merezha_domain::Failure;
use merezha_platform::{Database, Migration};
use merezha_postgres::{Statement, classify};
use std::time::{Duration, Instant};

pub const MIGRATION: Migration = Migration::new(
    2,
    "teams_snapshots",
    "CREATE TABLE memoire.teams (
         team_id text PRIMARY KEY
     );
     CREATE TABLE memoire.snapshots (
         id text PRIMARY KEY,
         team_id text NOT NULL,
         database_id text NOT NULL,
         created_unix_ms bigint NOT NULL,
         size_bytes bigint NOT NULL,
         reason text NOT NULL CHECK (reason IN ('scheduled', 'pre_restore'))
     );
     CREATE INDEX snapshots_database ON memoire.snapshots (team_id, database_id, created_unix_ms DESC);
     CREATE INDEX snapshots_created ON memoire.snapshots (created_unix_ms);",
);

const TEAM_UPSERT: Statement = Statement::new(
    "memoire.teams.upsert",
    "INSERT memoire.teams",
    "INSERT INTO memoire.teams (team_id) VALUES ($1) ON CONFLICT DO NOTHING",
);
const TEAMS: Statement = Statement::new(
    "memoire.teams.all",
    "SELECT memoire.teams",
    "SELECT team_id FROM memoire.teams ORDER BY team_id",
);
const SNAPSHOT_INSERT: Statement = Statement::new(
    "memoire.snapshots.insert",
    "INSERT memoire.snapshots",
    "INSERT INTO memoire.snapshots (id, team_id, database_id, created_unix_ms, size_bytes, reason)
     VALUES ($1, $2, $3, $4, $5, $6)",
);
const SNAPSHOT_LIST: Statement = Statement::new(
    "memoire.snapshots.list",
    "SELECT memoire.snapshots",
    "SELECT id, created_unix_ms, size_bytes, reason FROM memoire.snapshots
     WHERE team_id = $1 AND database_id = $2 ORDER BY created_unix_ms DESC, id DESC",
);
const SNAPSHOT_DELETE: Statement = Statement::new(
    "memoire.snapshots.delete",
    "DELETE memoire.snapshots",
    "DELETE FROM memoire.snapshots WHERE id = $1",
);
const SNAPSHOT_DELETE_DATABASE: Statement = Statement::new(
    "memoire.snapshots.delete_database",
    "DELETE memoire.snapshots",
    "DELETE FROM memoire.snapshots WHERE team_id = $1 AND database_id = $2",
);
const SNAPSHOT_EXPIRED: Statement = Statement::new(
    "memoire.snapshots.expired",
    "SELECT memoire.snapshots",
    "SELECT id, team_id, database_id FROM memoire.snapshots WHERE created_unix_ms < $1 LIMIT 1000",
);

/// Every statement of this file.
pub const STATEMENTS: &[Statement] = &[
    TEAM_UPSERT,
    TEAMS,
    SNAPSHOT_INSERT,
    SNAPSHOT_LIST,
    SNAPSHOT_DELETE,
    SNAPSHOT_DELETE_DATABASE,
    SNAPSHOT_EXPIRED,
];

const BUDGET: Duration = Duration::from_secs(5);

/// One snapshot's record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotRow {
    pub id: String,
    pub created_unix_ms: u64,
    pub size_bytes: u64,
    pub reason: String,
}

/// Runs one statement in its own transaction.
macro_rules! run {
    ($db:expr, |$tx:ident, $store:ident| $body:expr) => {{
        let $store = $db.clone();
        $db.transaction(Instant::now() + BUDGET, move |$tx| {
            Box::pin(async move { $body })
        })
        .await
    }};
}

/// Records that `team` holds databases.
pub async fn add_team(db: &Database, team: &str) -> Result<(), Failure> {
    let team = team.to_owned();
    run!(db, |tx, store| {
        store
            .execute(tx, &TEAM_UPSERT, &[&team])
            .await
            .map_err(|e| classify(&e))?;
        Ok(())
    })
}

/// Every team that ever held a database.
pub async fn teams(db: &Database) -> Result<Vec<String>, Failure> {
    run!(db, |tx, store| {
        let rows = store
            .query(tx, &TEAMS, &[])
            .await
            .map_err(|e| classify(&e))?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    })
}

pub async fn insert_snapshot(
    db: &Database,
    team: &str,
    database: &str,
    row: &SnapshotRow,
) -> Result<(), Failure> {
    let (team, database, row) = (team.to_owned(), database.to_owned(), row.clone());
    run!(db, |tx, store| {
        let created = i64::try_from(row.created_unix_ms).unwrap_or(i64::MAX);
        let size = i64::try_from(row.size_bytes).unwrap_or(i64::MAX);
        store
            .execute(
                tx,
                &SNAPSHOT_INSERT,
                &[&row.id, &team, &database, &created, &size, &row.reason],
            )
            .await
            .map_err(|e| classify(&e))?;
        Ok(())
    })
}

/// A database's snapshots, newest first.
pub async fn snapshots(
    db: &Database,
    team: &str,
    database: &str,
) -> Result<Vec<SnapshotRow>, Failure> {
    let (team, database) = (team.to_owned(), database.to_owned());
    run!(db, |tx, store| {
        let rows = store
            .query(tx, &SNAPSHOT_LIST, &[&team, &database])
            .await
            .map_err(|e| classify(&e))?;
        Ok(rows
            .iter()
            .map(|row| SnapshotRow {
                id: row.get(0),
                created_unix_ms: u64::try_from(row.get::<_, i64>(1)).unwrap_or(0),
                size_bytes: u64::try_from(row.get::<_, i64>(2)).unwrap_or(0),
                reason: row.get(3),
            })
            .collect())
    })
}

pub async fn delete_snapshot(db: &Database, id: &str) -> Result<(), Failure> {
    let id = id.to_owned();
    run!(db, |tx, store| {
        store
            .execute(tx, &SNAPSHOT_DELETE, &[&id])
            .await
            .map_err(|e| classify(&e))?;
        Ok(())
    })
}

pub async fn delete_snapshots_of(db: &Database, team: &str, database: &str) -> Result<(), Failure> {
    let (team, database) = (team.to_owned(), database.to_owned());
    run!(db, |tx, store| {
        store
            .execute(tx, &SNAPSHOT_DELETE_DATABASE, &[&team, &database])
            .await
            .map_err(|e| classify(&e))?;
        Ok(())
    })
}

/// Up to 1000 snapshots taken before `before_unix_ms`: `(id, team, database)`.
pub async fn expired_snapshots(
    db: &Database,
    before_unix_ms: u64,
) -> Result<Vec<(String, String, String)>, Failure> {
    let before = i64::try_from(before_unix_ms).unwrap_or(i64::MAX);
    run!(db, |tx, store| {
        let rows = store
            .query(tx, &SNAPSHOT_EXPIRED, &[&before])
            .await
            .map_err(|e| classify(&e))?;
        Ok(rows
            .iter()
            .map(|row| (row.get(0), row.get(1), row.get(2)))
            .collect())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_statements_are_registered_once() {
        merezha_postgres::check_registry(&[
            merezha_postgres::KERNEL_STATEMENTS,
            crate::last_used::STATEMENTS,
            super::STATEMENTS,
        ])
        .unwrap();
    }
}
