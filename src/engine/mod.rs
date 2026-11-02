pub mod sqlite;

use crate::config::{Engine, ResolvedDb};
use crate::storage::{SnapshotRecord, Storage};
use anyhow::Result;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct EngineInfo {
    pub engine: String,
    pub detail: String,
    pub size_bytes: Option<u64>,
}

pub struct SaveOutcome {
    pub record: SnapshotRecord,
    /// True when the snapshot content already existed (deduplicated by hash
    /// or unchanged fast path) and no new file was written.
    pub linked: bool,
}

/// Engine seam (design §3.1) — v0.1 implements SQLite; PostgreSQL and
/// MySQL/MariaDB join through this same trait in v0.2/v0.3.
pub trait DatabaseEngine: Send + Sync {
    fn probe(&self) -> Result<EngineInfo>;

    /// Create a snapshot. `previous` enables dedup/unchanged fast paths;
    /// `taken_ids` is used to mint a collision-free id.
    fn save(
        &self,
        store: &Storage,
        tag: &str,
        previous: Option<&SnapshotRecord>,
        taken_ids: &HashSet<String>,
    ) -> Result<SaveOutcome>;

    /// Verify (T3) and decompress the snapshot into a staging file without
    /// touching the live database.
    fn stage_restore(&self, store: &Storage, record: &SnapshotRecord) -> Result<PathBuf>;

    /// Atomically apply the staged snapshot to the live database.
    fn apply_restore(&self, staged: &Path) -> Result<()>;
}

/// Engine registry keyed by resolved database type.
pub fn make(db: &ResolvedDb) -> Result<Box<dyn DatabaseEngine>> {
    match db.engine {
        Engine::Sqlite => Ok(Box::new(sqlite::SqliteEngine::new(db.path.clone()))),
    }
}
