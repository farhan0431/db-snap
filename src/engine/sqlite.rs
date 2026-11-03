use super::{DatabaseEngine, EngineInfo, RestoreOutcome, SaveMethod, SaveOpts, SaveOutcome};
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

    /// Compare verified SQLite snapshots without opening or changing the live database.
    pub fn diff(
        &self,
        store: &Storage,
        left: &SnapshotRecord,
        right: &SnapshotRecord,
    ) -> Result<String> {
        use std::collections::BTreeMap;
        type SchemaObjects = BTreeMap<(String, String), (String, Option<i64>)>;
        fn inspect(path: &Path) -> Result<SchemaObjects> {
            let conn =
                Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let mut statement = conn.prepare("SELECT type, name, COALESCE(sql, '') FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' AND type IN ('table','index','view','trigger') ORDER BY type, name")?;
            let entries = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            let mut result = BTreeMap::new();
            for entry in entries {
                let (kind, name, sql) = entry?;
                let count = if kind == "table" {
                    let identifier = format!("\"{}\"", name.replace('"', "\"\""));
                    Some(conn.query_row(
                        &format!("SELECT COUNT(*) FROM {identifier}"),
                        [],
                        |row| row.get(0),
                    )?)
                } else {
                    None
                };
                result.insert((kind, name), (sql, count));
            }
            Ok(result)
        }
        let left_path = self.stage(store, left)?;
        let right_path = match self.stage(store, right) {
            Ok(path) => path,
            Err(e) => {
                std::fs::remove_file(&left_path).ok();
                return Err(e);
            }
        };
        let result = (|| {
            let a = inspect(&left_path)?;
            let b = inspect(&right_path)?;
            let mut changes = Vec::new();
            for (key, old) in &a {
                match b.get(key) {
                    None => changes.push(format!("removed {} {}", key.0, key.1)),
                    Some(new) => {
                        if old.0 != new.0 {
                            changes.push(format!("changed schema {} {}", key.0, key.1));
                        }
                        if old.1 != new.1 {
                            changes.push(format!(
                                "table {} rows: {} -> {}",
                                key.1,
                                old.1.unwrap_or(0),
                                new.1.unwrap_or(0)
                            ));
                        }
                    }
                }
            }
            for key in b.keys() {
                if !a.contains_key(key) {
                    changes.push(format!("added {} {}", key.0, key.1));
                }
            }
            if changes.is_empty() {
                Ok(
                    "no schema or table row-count differences (row contents are not compared)"
                        .to_string(),
                )
            } else {
                Ok(format!(
                    "{}\n(row contents are not compared)",
                    changes.join("\n")
                ))
            }
        })();
        std::fs::remove_file(&left_path).ok();
        std::fs::remove_file(&right_path).ok();
        result
    }

    fn open_live(&self) -> Result<Connection> {
        let conn =
            Connection::open_with_flags(&self.path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
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

    /// Verify (§4-T3) and materialize the snapshot into a staging file
    /// without touching the live database.
    fn stage(&self, store: &Storage, record: &SnapshotRecord) -> Result<PathBuf> {
        match &record.location {
            Location::File { path } => {
                let src = store.checked_artifact_path(path, "snapshots")?;
                if !src.exists() {
                    bail!(
                        "snapshot file {} is missing on disk — the snapshot catalog may be out of sync",
                        src.display()
                    );
                }
                let staged = store.tmp_dir().join(format!("restore-{}.db", record.id));
                compress::decompress_verified(&src, &staged, &record.blake3, record.original_bytes)?;
                Ok(staged)
            }
            Location::Manifest { path } => {
                // Reassembles from the chunk store, verifying every chunk
                // (per-chunk integrity, design §4-T3).
                let (staged, _) = cas::stage_from_manifest(store, path, &record.id, &record.blake3, record.original_bytes)?;
                Ok(staged)
            }
            Location::PgDatabase { .. } => bail!(
                "snapshot {} is a PostgreSQL server-side clone — restore it with the PostgreSQL engine bound to that server",
                record.id
            ),
        }
    }

    /// Backup-API the staged pages into the live database, then flush the
    /// WAL so the running dev server reads the new state immediately.
    fn apply(&self, staged: &Path) -> Result<()> {
        {
            let src = Connection::open(staged)
                .with_context(|| format!("opening staged snapshot {}", staged.display()))?;
            let mut dst = self.open_live()?;
            let backup = Backup::new(&src, &mut dst)?;
            backup.run_to_completion(BACKUP_PAGES_PER_STEP, Duration::from_millis(0), None)?;
        }
        let live = self.open_live()?;
        let _ = live.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        Ok(())
    }
}

impl DatabaseEngine for SqliteEngine {
    fn probe(&self) -> Result<EngineInfo> {
        let conn = self.open_live()?;
        let version: String = conn.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
        let journal: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        let mut size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        let mut wal = self.path.as_os_str().to_os_string();
        wal.push("-wal");
        let wal = PathBuf::from(wal);
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
        opts: &SaveOpts,
        previous: Option<&SnapshotRecord>,
        taken_ids: &HashSet<String>,
    ) -> Result<SaveOutcome> {
        if opts.method == SaveMethod::Dump {
            bail!(
                "--method dump is PostgreSQL-only — SQLite snapshots are already physical and delta-compressed"
            );
        }

        // Always read a consistent backup: a new connection's data_version
        // and the main file's mtime cannot prove the WAL has not changed.
        let current_state = self.source_state();

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
                let Location::Manifest { path } = &prev.location else {
                    bail!("cannot reuse a non-manifest snapshot");
                };
                let manifest = store.read_manifest_file(path)?;
                cas::verify_manifest(&manifest, &prev.blake3, prev.original_bytes)?;
                let mut record = prev.clone();
                record.id = id;
                record.tag = tag.to_string();
                record.pinned = false;
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
                        pinned: false,
                        source_state: current_state,
                    },
                    linked: false,
                })
            }
        }
    }

    fn restore(
        &self,
        store: &Storage,
        record: &SnapshotRecord,
        safety_tag: Option<&str>,
    ) -> Result<RestoreOutcome> {
        // Verify integrity BEFORE anything touches the live database (§4-T3).
        let staged = self.stage(store, record)?;

        // Safety snapshot of the current state first (§4-T7) — persisted to
        // metadata immediately so a crash mid-restore still leaves a way back.
        let mut safety = None;
        if let Some(tag) = safety_tag {
            let meta = store.load_metadata()?;
            let taken = meta.ids();
            match self.save(
                store,
                tag,
                &SaveOpts {
                    method: SaveMethod::Clone,
                },
                meta.latest().as_ref(),
                &taken,
            ) {
                Ok(outcome) => {
                    let mut meta = store.load_metadata()?;
                    meta.snapshots.push(outcome.record.clone());
                    store.write_metadata(&meta)?;
                    safety = Some(outcome.record);
                }
                Err(e) => {
                    std::fs::remove_file(&staged).ok();
                    bail!("pre-restore safety snapshot failed; restore aborted: {e:#}");
                }
            }
        }

        if let Err(e) = self.apply(&staged) {
            std::fs::remove_file(&staged).ok();
            let hint = safety
                .as_ref()
                .map(|r| {
                    format!(
                        "\nthe pre-restore state was saved as snapshot {} — recover with: dbsnap restore {}",
                        r.id, r.id
                    )
                })
                .unwrap_or_default();
            bail!("restore failed: {e:#}{hint}");
        }
        std::fs::remove_file(&staged).ok();
        let conn = self
            .open_live()
            .context("restore applied but verification could not open the database")?;
        let check: String = conn
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
            .context("restore applied but SQLite verification failed")?;
        if check != "ok" {
            bail!("restore applied but SQLite quick_check failed: {check}");
        }
        Ok(RestoreOutcome { safety })
    }
}
