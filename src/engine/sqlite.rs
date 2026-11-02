use super::{DatabaseEngine, EngineInfo, SaveOutcome};
use crate::storage::{cas, compress, Location, SnapshotRecord, SourceState, Storage};
use crate::util;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use rusqlite::backup::Backup;
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

/// Minimum free space required before any snapshot operation (design §4-T8).
const MIN_FREE_BYTES: u64 = 100 * 1024 * 1024;
const BACKUP_PAGES_PER_STEP: i32 = 64;

pub struct SqliteEngine {
    path: PathBuf,
}

impl SqliteEngine {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn open_live(&self) -> Result<Connection> {
        let conn = Connection::open(&self.path)
            .with_context(|| format!("opening database {}", self.path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        Ok(conn)
    }

    /// (PRAGMA data_version, file size, mtime millis) — advisory fingerprint
    /// of the source state for the unchanged fast path.
    fn source_state(&self) -> Option<SourceState> {
        let conn = self.open_live().ok()?;
        let data_version: i64 = conn
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .ok()?;
        let meta = std::fs::metadata(&self.path).ok()?;
        let mtime_ms = meta
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_millis() as i64;
        Some(SourceState {
            data_version,
            size_bytes: meta.len(),
            mtime_ms,
        })
    }
}

impl DatabaseEngine for SqliteEngine {
    fn probe(&self) -> Result<EngineInfo> {
        let conn = self.open_live()?;
        let version: String = conn.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
        let journal: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        let mut size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        let wal = self.path.with_extension("db-wal");
        if let Ok(m) = std::fs::metadata(&wal) {
            size += m.len();
        }
        Ok(EngineInfo {
            engine: "sqlite".to_string(),
            detail: format!("SQLite {version}, journal: {journal}"),
            size_bytes: Some(size),
        })
    }

    fn save(
        &self,
        store: &Storage,
        tag: &str,
        previous: Option<&SnapshotRecord>,
        taken_ids: &HashSet<String>,
    ) -> Result<SaveOutcome> {
        // Fast path: the source is provably untouched since the previous
        // snapshot (data_version + size + mtime all equal) — link it.
        let current_state = self.source_state();
        if let (Some(prev), Some(state)) = (previous, current_state.as_ref()) {
            if let Some(prev_state) = &prev.source_state {
                if prev_state.data_version == state.data_version
                    && prev_state.size_bytes == state.size_bytes
                    && prev_state.mtime_ms == state.mtime_ms
                {
                    let mut record = prev.clone();
                    record.id = util::new_id(taken_ids);
                    record.tag = tag.to_string();
                    record.created_at = Utc::now();
                    return Ok(SaveOutcome {
                        record,
                        linked: true,
                    });
                }
            }
        }

        let free = store.free_bytes()?;
        if free < MIN_FREE_BYTES {
            bail!(
                "only {} free on this volume — refusing to snapshot (safety threshold 100 MB)",
                util::format_size(free)
            );
        }

        let id = util::new_id(taken_ids);
        let staged = store.tmp_dir().join(format!("save-{id}.db"));

        // Hot backup through the Online Backup API: page-consistent copy
        // while the dev server keeps running (design §2.1).
        {
            let src = self.open_live()?;
            let mut dst = Connection::open(&staged)?;
            let backup = Backup::new(&src, &mut dst)?;
            backup.run_to_completion(BACKUP_PAGES_PER_STEP, Duration::from_millis(0), None)?;
        }

        // Delta snapshot: hash 64 KB page-aligned chunks, write only the
        // chunks missing from the store, and record a manifest (§3.5).
        // Identical content to the previous delta snapshot writes nothing.
        let prev_content_id = previous.filter(|p| p.is_delta()).map(|p| p.blake3.clone());
        match cas::save_delta(store, &staged, &id, prev_content_id.as_deref())? {
            cas::DeltaOutcome::Identical { .. } => {
                std::fs::remove_file(&staged).ok();
                let Some(prev) = previous else {
                    bail!("identical content reported without a previous snapshot");
                };
                let mut record = prev.clone();
                record.id = id;
                record.tag = tag.to_string();
                record.created_at = Utc::now();
                Ok(SaveOutcome {
                    record,
                    linked: true,
                })
            }
            cas::DeltaOutcome::Written {
                manifest_rel,
                manifest_size,
                written_bytes,
                content_id,
                original_size,
            } => {
                std::fs::remove_file(&staged).ok();
                Ok(SaveOutcome {
                    record: SnapshotRecord {
                        id,
                        tag: tag.to_string(),
                        created_at: Utc::now(),
                        engine: "sqlite".to_string(),
                        location: Location::Manifest { path: manifest_rel },
                        size_bytes: manifest_size + written_bytes,
                        original_bytes: original_size,
                        blake3: content_id,
                        method: "sqlite-delta".to_string(),
                        source_state: current_state,
                    },
                    linked: false,
                })
            }
        }
    }

    fn stage_restore(&self, store: &Storage, record: &SnapshotRecord) -> Result<PathBuf> {
        match &record.location {
            Location::File { path } => {
                let src = store.root.join(path);
                if !src.exists() {
                    bail!(
                        "snapshot file {} is missing on disk — the snapshot catalog may be out of sync",
                        src.display()
                    );
                }
                let staged = store.tmp_dir().join(format!("restore-{}.db", record.id));
                compress::decompress_verified(&src, &staged, &record.blake3)?;
                Ok(staged)
            }
            Location::Manifest { path } => {
                // Reassembles from the chunk store, verifying every chunk
                // (per-chunk integrity, design §4-T3).
                let (staged, _) = cas::stage_from_manifest(store, path, &record.id)?;
                Ok(staged)
            }
        }
    }

    fn apply_restore(&self, staged: &Path) -> Result<()> {
        {
            let src = Connection::open(staged)
                .with_context(|| format!("opening staged snapshot {}", staged.display()))?;
            let mut dst = self.open_live()?;
            let backup = Backup::new(&src, &mut dst)?;
            backup.run_to_completion(BACKUP_PAGES_PER_STEP, Duration::from_millis(0), None)?;
        }
        // Flush the WAL so the running dev server reads the restored pages
        // immediately (design §2.1).
        let live = self.open_live()?;
        let _ = live.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        Ok(())
    }
}
