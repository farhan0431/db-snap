pub mod mysql;
mod mysql_diff;
pub(crate) mod mysql_sandbox;
pub mod postgres;
mod postgres_archive_diff;
pub mod sqlite;

use crate::config::{DbTarget, ResolvedDb};
use crate::storage::{SnapshotRecord, Storage};
use anyhow::Result;
use std::collections::HashSet;

/// How a snapshot is taken. Clone = physical copy (server-side template
/// cloning on PostgreSQL, delta store on SQLite); Dump = portable logical
/// dump (`pg_dump -Fc`).
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
#[value(rename_all = "kebab")]
pub enum SaveMethod {
    Clone,
    Dump,
}

#[derive(Clone, Copy, Debug)]
pub struct SaveOpts {
    pub method: SaveMethod,
}

#[derive(Debug, Clone)]
pub struct EngineInfo {
    pub engine: String,
    pub detail: String,
    pub size_bytes: Option<u64>,
}

pub struct SaveOutcome {
    pub record: SnapshotRecord,
    /// True when the snapshot content already existed (deduplicated by hash
    /// or unchanged fast path) and no new artifact was written.
    pub linked: bool,
}

pub struct RestoreOutcome {
    /// The pre-restore safety snapshot, when one was made. The engine
    /// persists this record into metadata itself the moment it exists
    /// (crash-safety: it must be recoverable even if restore dies midway) —
    /// ops only reports it.
    pub safety: Option<SnapshotRecord>,
}

/// Engine seam (design §3.1) — SQLite, PostgreSQL, MySQL/MariaDB.
pub trait DatabaseEngine: Send + Sync {
    fn probe(&self) -> Result<EngineInfo>;

    /// Create a snapshot. `previous` enables dedup/unchanged fast paths;
    /// `taken_ids` is used to mint a collision-free id.
    fn save(
        &self,
        store: &Storage,
        tag: &str,
        opts: &SaveOpts,
        previous: Option<&SnapshotRecord>,
        taken_ids: &HashSet<String>,
    ) -> Result<SaveOutcome>;

    /// Restore `record`. When `safety_tag` is Some, the current state is
    /// first preserved as a snapshot with that tag and returned in the
    /// outcome (already persisted to metadata by the engine — see
    /// RestoreOutcome).
    fn restore(
        &self,
        store: &Storage,
        record: &SnapshotRecord,
        safety_tag: Option<&str>,
    ) -> Result<RestoreOutcome>;
}

/// Engine registry keyed by resolved database type.
pub fn make(db: &ResolvedDb) -> Result<Box<dyn DatabaseEngine>> {
    match &db.target {
        DbTarget::Sqlite(path) => Ok(Box::new(sqlite::SqliteEngine::new(path.clone()))),
        DbTarget::Postgres(pg) => Ok(Box::new(postgres::PostgresEngine::new(pg.clone()))),
        DbTarget::Mysql(m) => Ok(Box::new(mysql::MysqlEngine::new(m.clone()))),
    }
}
