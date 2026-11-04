//! Versioned, single-payload archive with no embedded filesystem paths.
use super::*;
use crate::storage::{compress, Location, TemporaryFile};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};

const MAGIC: &[u8; 8] = b"DBSNAP\0\x01";
const MAX_HEADER: usize = 16 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    engine: String,
    tag: String,
    created_at: chrono::DateTime<chrono::Utc>,
    bytes: u64,
    original_bytes: u64,
    hash: String,
    mysql_database: Option<String>,
}

pub fn export_at(root: &Path, db: Option<&str>, selector: &str, output: &Path) -> Result<String> {
    redact((|| {
        let ctx = context_at(root, db, false, false)?;
        let _lock = ctx.store.lock()?;
        super::health::check_binding(&ctx)?;
        let meta = ctx.store.load_metadata()?;
        let record = meta.find(Some(selector))?;
        if record.engine != ctx.db.engine.to_string() {
            bail!("snapshot engine differs from binding");
        }
        let scratch = ctx
            .store
            .tmp_dir()
            .join(format!("export-{}.tmp", util::new_id(&meta.ids())));
        let _scratch = TemporaryFile::create(scratch.clone())?;
        let mut header = Header {
            engine: record.engine.clone(),
            tag: record.tag.clone(),
            created_at: record.created_at,
            bytes: record.size_bytes,
            original_bytes: record.original_bytes,
            hash: record.blake3.clone(),
            mysql_database: None,
        };
        let source = match (&ctx.db.target, &record.location) {
            (DbTarget::Sqlite(db), Location::Manifest { .. }) => {
                let mut staging_record = record.clone();
                staging_record.id = util::new_id(&meta.ids());
                let staged = engine::sqlite::SqliteEngine::new(db.clone())
                    .stage(&ctx.store, &staging_record)?;
                let compressed = compress::compress_to(&staged, &scratch);
                fs::remove_file(&staged).ok();
                (header.original_bytes, header.bytes, header.hash) = compressed?;
                scratch
            }
            (DbTarget::Postgres(pg), Location::PgDatabase { .. }) => {
                engine::postgres::PostgresEngine::new(pg.clone())
                    .dump_clone_to(&record, &scratch)?;
                header.bytes = scratch.metadata()?.len();
                header.hash = compress::hash_file(&scratch)?;
                scratch
            }
            (DbTarget::Postgres(_), Location::File { .. }) if record.method == "pg-dump" => {
                ctx.store.snapshot_path(&record)?
            }
            (DbTarget::Sqlite(_), Location::File { .. }) => ctx.store.snapshot_path(&record)?,
            (DbTarget::Mysql(target), Location::File { .. }) if record.method == "mysql-dump" => {
                header.mysql_database = Some(target.database.clone());
                ctx.store.snapshot_path(&record)?
            }
            _ => bail!("snapshot format cannot be exported"),
        };
        verify_payload(&source, &header)?;
        let encoded = serde_json::to_vec(&header)?;
        if encoded.len() > MAX_HEADER {
            bail!("portable archive metadata too large");
        }
        let guard = TemporaryFile::create(output.to_path_buf())?;
        let mut archive = OpenOptions::new().write(true).open(output)?;
        archive.write_all(MAGIC)?;
        archive.write_all(&(encoded.len() as u32).to_le_bytes())?;
        archive.write_all(&encoded)?;
        archive.write_all(blake3::hash(&encoded).as_bytes())?;
        if std::env::var("DBSNAP_FAILPOINT").as_deref() == Ok("export_before_payload") {
            std::process::exit(86);
        }
        io::copy(&mut File::open(source)?, &mut archive)?;
        if std::env::var("DBSNAP_FAILPOINT").as_deref() == Ok("export_disk_full_before_sync") {
            #[cfg(windows)]
            let code = 112;
            #[cfg(not(windows))]
            let code = 28;
            return Err(io::Error::from_raw_os_error(code))
                .context("syncing archive: simulated disk-full error");
        }
        archive.sync_all()?;
        guard.persist();
        Ok(format!(
            "exported snapshot {} to {}",
            record.id,
            output.display()
        ))
    })())
}

pub fn import_at(
    root: &Path,
    db: Option<&str>,
    archive: &Path,
    tag: Option<&str>,
) -> Result<String> {
    redact((|| {
        let mut input = File::open(archive).context("opening portable snapshot")?;
        let mut magic = [0; 8];
        input.read_exact(&mut magic)?;
        if &magic != MAGIC {
            bail!("unknown portable snapshot format or version");
        }
        let mut length = [0; 4];
        input.read_exact(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        if length == 0 || length > MAX_HEADER {
            bail!("invalid portable snapshot header length");
        }
        let mut bytes = vec![0; length];
        input.read_exact(&mut bytes)?;
        let mut header_hash = [0; 32];
        input.read_exact(&mut header_hash)?;
        if blake3::hash(&bytes).as_bytes() != &header_hash {
            bail!("portable snapshot header integrity check failed");
        }
        let header: Header =
            serde_json::from_slice(&bytes).context("reading portable snapshot header")?;
        let tag = tag.unwrap_or(&header.tag);
        util::validate_tag(tag)?;
        if header.bytes == 0
            || input.metadata()?.len().checked_sub(44 + length as u64) != Some(header.bytes)
        {
            bail!("portable snapshot payload length mismatch");
        }

        let ctx = context_at(root, db, false, false)?;
        let _lock = ctx.store.lock()?;
        super::health::check_binding(&ctx)?;
        if header.engine != ctx.db.engine.to_string() {
            bail!("archive engine differs from project binding");
        }
        let (method, extension) = match &ctx.db.target {
            DbTarget::Sqlite(_) => ("sqlite-backup", "zst"),
            DbTarget::Postgres(_) => ("pg-dump", "dump"),
            DbTarget::Mysql(target) => {
                if header.mysql_database.as_deref() != Some(&target.database) {
                    bail!("MySQL/MariaDB archive requires the original database name");
                }
                ("mysql-dump", "sql.zst")
            }
        };
        let mut meta = ctx.store.load_metadata()?;
        let id = util::new_id(&meta.ids());
        let path = format!("snapshots/{id}.{extension}");
        let output = ctx.store.checked_artifact_path(&path, "snapshots")?;
        let guard = TemporaryFile::create(output.clone())?;
        {
            let mut payload = OpenOptions::new().write(true).open(&output)?;
            if io::copy(&mut input.take(header.bytes), &mut payload)? != header.bytes {
                bail!("portable snapshot payload is truncated");
            }
            payload.sync_all()?;
        }
        verify_payload(&output, &header)?;
        meta.snapshots.push(SnapshotRecord {
            id: id.clone(),
            tag: tag.into(),
            created_at: header.created_at,
            engine: header.engine,
            location: Location::File { path },
            size_bytes: header.bytes,
            original_bytes: header.original_bytes,
            blake3: header.hash,
            method: method.into(),
            pinned: false,
            message: None,
            git_branch: None,
            source_state: None,
        });
        if let Err(error) = ctx.store.write_metadata(&meta) {
            // Directory sync can fail after metadata has already been renamed.
            // Preserve the payload if it may now be referenced by the catalog.
            if ctx
                .store
                .load_metadata()
                .map_or(true, |current| current.ids().contains(&id))
            {
                guard.persist();
            }
            return Err(error);
        }
        guard.persist();
        Ok(format!("imported snapshot {id} ({tag})"))
    })())
}

fn verify_payload(path: &Path, header: &Header) -> Result<()> {
    if path.metadata()?.len() != header.bytes || compress::hash_file(path)? != header.hash {
        bail!("portable snapshot payload integrity check failed");
    }
    let file = File::open(path)?;
    if header.engine == "postgres" {
        let mut signature = [0; 5];
        (&file).read_exact(&mut signature)?;
        if &signature != b"PGDMP" {
            bail!("portable PostgreSQL payload is not a custom dump");
        }
    } else {
        let decoder = zstd::stream::read::Decoder::new(file)?;
        let limit = header
            .original_bytes
            .checked_add(1)
            .context("invalid expanded size")?;
        if io::copy(&mut decoder.take(limit), &mut io::sink())? != header.original_bytes {
            bail!("portable snapshot expanded size mismatch");
        }
    }
    Ok(())
}
