use anyhow::{bail, Context, Result};
use fs4::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

pub mod cas;
pub mod compress;

pub const DBSNAP_DIR: &str = ".dbsnap";
const LOCK_WAIT_MS: u64 = 10_000;

/// Project binding persisted in config.json (design §3.2).
pub use crate::config::Fingerprint;

/// Where a snapshot's bytes live. `file` = single-file snapshot (v0.1
/// legacy or `pg_dump` output); `manifest` = delta snapshot over the
/// content-addressed chunk store (§3.5); `pg_database` = server-side
/// PostgreSQL clone (§2.2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Location {
    File { path: String },
    Manifest { path: String },
    PgDatabase { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceState {
    pub data_version: i64,
    pub size_bytes: u64,
    pub mtime_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRecord {
    pub id: String,
    pub tag: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub engine: String,
    pub location: Location,
    pub size_bytes: u64,
    pub original_bytes: u64,
    pub blake3: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_state: Option<SourceState>,
}

impl SnapshotRecord {
    pub fn file_path(&self) -> Option<&str> {
        match &self.location {
            Location::File { path } => Some(path),
            Location::Manifest { .. } | Location::PgDatabase { .. } => None,
        }
    }

    /// Relative path (inside .dbsnap/) of the record's primary artifact:
    /// the .zst/.dump file or the manifest. Server-side clones have none.
    pub fn artifact_rel(&self) -> Option<&str> {
        match &self.location {
            Location::File { path } | Location::Manifest { path } => Some(path),
            Location::PgDatabase { .. } => None,
        }
    }

    /// True when this record is a delta snapshot over the chunk store.
    pub fn is_delta(&self) -> bool {
        matches!(self.location, Location::Manifest { .. })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub version: u32,
    pub project_fingerprint: Fingerprint,
    pub snapshots: Vec<SnapshotRecord>,
}

impl Metadata {
    pub fn new(fingerprint: Fingerprint) -> Self {
        Self {
            version: 2,
            project_fingerprint: fingerprint,
            snapshots: Vec::new(),
        }
    }

    /// Resolve "latest" (None) or an id-prefix/tag to a snapshot record.
    /// Ambiguous tags are an error — ids are the precise selector.
    pub fn find(&self, id_or_tag: Option<&str>) -> Result<SnapshotRecord> {
        if self.snapshots.is_empty() {
            bail!("no snapshots yet — create one with: dbsnap save \"my-tag\"");
        }
        let latest = || {
            self.snapshots
                .iter()
                .max_by_key(|s| s.created_at)
                .expect("non-empty checked above")
                .clone()
        };
        let Some(query) = id_or_tag else {
            return Ok(latest());
        };

        let by_id: Vec<&SnapshotRecord> = self
            .snapshots
            .iter()
            .filter(|s| s.id.starts_with(query))
            .collect();
        let by_tag: Vec<&SnapshotRecord> =
            self.snapshots.iter().filter(|s| s.tag == query).collect();

        let mut matches: Vec<&SnapshotRecord> = by_id.clone();
        for t in &by_tag {
            if !matches.iter().any(|m| m.id == t.id) {
                matches.push(t);
            }
        }
        match matches.len() {
            0 => bail!(
                "no snapshot matches {:?} — available tags: {}",
                query,
                self.tags_summary()
            ),
            1 => Ok(matches[0].clone()),
            _ => bail!(
                "{:?} matches multiple snapshots ({}). Use a longer id prefix.",
                query,
                matches
                    .iter()
                    .map(|s| s.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    pub fn latest(&self) -> Option<SnapshotRecord> {
        self.snapshots.iter().max_by_key(|s| s.created_at).cloned()
    }

    pub fn ids(&self) -> HashSet<String> {
        self.snapshots.iter().map(|s| s.id.clone()).collect()
    }

    fn tags_summary(&self) -> String {
        if self.snapshots.is_empty() {
            return "(none)".to_string();
        }
        let mut tags: Vec<String> = self.snapshots.iter().map(|s| s.tag.clone()).collect();
        tags.sort();
        tags.join(", ")
    }
}

pub struct Storage {
    /// Absolute path of the `.dbsnap` directory.
    pub root: PathBuf,
}

impl Storage {
    /// Walk up from `start` (like git) looking for an existing `.dbsnap/`.
    pub fn find(start: &Path) -> Option<Storage> {
        let mut dir = Some(start.to_path_buf());
        while let Some(d) = dir {
            let candidate = d.join(DBSNAP_DIR);
            if candidate.is_dir() {
                return Some(Storage { root: candidate });
            }
            dir = d.parent().map(|p| p.to_path_buf());
        }
        None
    }

    /// Create `.dbsnap/` in `start`, (re)binding the project fingerprint.
    pub fn create(start: &Path, fingerprint: &Fingerprint) -> Result<Storage> {
        let root = start.join(DBSNAP_DIR);
        fs::create_dir_all(root.join("snapshots"))?;
        fs::create_dir_all(root.join("tmp"))?;
        fs::create_dir_all(root.join("objects"))?;
        fs::create_dir_all(root.join("manifests"))?;
        let config = serde_json::json!({
            "version": 1,
            "project_fingerprint": fingerprint,
        });
        write_atomic(
            &root.join("config.json"),
            serde_json::to_string_pretty(&config)?.as_bytes(),
        )?;
        update_gitignore(start)?;
        Ok(Storage { root })
    }

    pub fn load_config(&self) -> Result<Fingerprint> {
        let raw = fs::read_to_string(self.root.join("config.json"))
            .context("reading .dbsnap/config.json")?;
        let cfg: serde_json::Value = serde_json::from_str(&raw)?;
        serde_json::from_value(cfg["project_fingerprint"].clone())
            .context("parsing project_fingerprint from config.json")
    }

    pub fn load_metadata(&self) -> Result<Metadata> {
        let path = self.root.join("metadata.json");
        if !path.exists() {
            let cfg = self.load_config()?;
            return Ok(Metadata::new(cfg));
        }
        let raw = fs::read_to_string(&path).context("reading .dbsnap/metadata.json")?;
        serde_json::from_str(&raw).context("parsing .dbsnap/metadata.json")
    }

    /// Atomic metadata write: temp file then rename (design §4-T4).
    pub fn write_metadata(&self, metadata: &Metadata) -> Result<()> {
        let bytes = serde_json::to_string_pretty(metadata)?;
        write_atomic(&self.root.join("metadata.json"), bytes.as_bytes())
    }

    pub fn snapshots_dir(&self) -> PathBuf {
        self.root.join("snapshots")
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// Absolute path of a record's primary artifact (.zst/.dump file or
    /// manifest). Server-side clones have no local file.
    pub fn snapshot_path(&self, record: &SnapshotRecord) -> Result<PathBuf> {
        match record.artifact_rel() {
            Some(rel) => Ok(self.root.join(rel)),
            None => bail!(
                "snapshot {} lives inside the PostgreSQL server — no local file",
                record.id
            ),
        }
    }

    /// Exclusive cross-process lock held for the duration of any mutating
    /// operation (design §4-T5). Blocks up to 10s, then fails loudly.
    pub fn lock(&self) -> Result<LockGuard> {
        let path = self.root.join("lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(&path)
            .with_context(|| format!("opening lock file {}", path.display()))?;
        let attempts = LOCK_WAIT_MS / 100;
        for _ in 0..attempts {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(LockGuard { _file: file }),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(e).context("acquiring .dbsnap lock"),
            }
        }
        bail!("another dbsnap process holds the lock (waited {LOCK_WAIT_MS}ms)")
    }

    /// Free disk space on the volume holding `.dbsnap/` (design §4-T8).
    pub fn free_bytes(&self) -> Result<u64> {
        Ok(fs4::available_space(&self.root)?)
    }
}

pub struct LockGuard {
    _file: File,
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.flush()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

/// Register `.dbsnap/` in the project .gitignore (design §4-T14).
fn update_gitignore(project_root: &Path) -> Result<()> {
    let gitignore = project_root.join(".gitignore");
    let existing = fs::read_to_string(&gitignore).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == DBSNAP_DIR) {
        return Ok(());
    }
    let mut out = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&gitignore)?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        out.write_all(b"\n")?;
    }
    out.write_all(b"\n# dbsnap snapshots\n.dbsnap/\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_walks_upwards() {
        let base = std::env::temp_dir().join(format!("dbsnap-find-{}", std::process::id()));
        let nested = base.join("a/b/c");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(base.join(DBSNAP_DIR)).unwrap();
        let found = Storage::find(&nested).expect("should find .dbsnap upward");
        assert_eq!(found.root, base.join(DBSNAP_DIR));
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn metadata_find_by_tag_and_prefix() {
        let fp = Fingerprint {
            engine: "sqlite".into(),
            target: "x".into(),
        };
        let mut meta = Metadata::new(fp);
        meta.snapshots.push(SnapshotRecord {
            id: "a1b2c3".into(),
            tag: "before-migration".into(),
            created_at: chrono::Utc::now(),
            engine: "sqlite".into(),
            location: Location::File {
                path: "snapshots/a1b2c3.zst".into(),
            },
            size_bytes: 1,
            original_bytes: 1,
            blake3: "x".into(),
            method: "sqlite-backup".into(),
            source_state: None,
        });
        assert_eq!(meta.find(Some("a1")).unwrap().id, "a1b2c3");
        assert_eq!(meta.find(Some("before-migration")).unwrap().id, "a1b2c3");
        assert!(meta.find(Some("zzz")).is_err());
    }
}
