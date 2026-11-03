use super::{DatabaseEngine, EngineInfo, RestoreOutcome, SaveMethod, SaveOpts, SaveOutcome};
use crate::config::ServerTarget;
use crate::storage::{compress, Location, SnapshotRecord, Storage};
use crate::util;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use postgres::NoTls;
use std::collections::HashSet;
use std::process::Command;

/// Prefix reserved for dbsnap-managed databases (design §2.2 rule 1) —
/// this engine NEVER drops or renames databases outside it.
pub const SNAP_PREFIX: &str = "__dbsnap_";

/// A dbsnap-managed server-side clone of the target database.
pub struct PostgresEngine {
    pg: ServerTarget,
}

impl PostgresEngine {
    pub fn new(pg: ServerTarget) -> Self {
        Self { pg }
    }

    fn connect(&self, dbname: &str) -> Result<postgres::Client> {
        let host = if self.pg.host.eq_ignore_ascii_case("localhost") {
            "127.0.0.1"
        } else {
            &self.pg.host
        };
        postgres::Config::new()
            .host(host)
            .port(self.pg.port)
            .user(&self.pg.user)
            .password(self.pg.password.as_deref().unwrap_or(""))
            .dbname(dbname)
            .connect(NoTls)
            .with_context(|| format!("connecting to {} (as {})", self.pg.display(), self.pg.user))
    }

    /// Control-plane connection — deliberately never to the target database
    /// (we may have to drop it). Tries `postgres`, then `template1`.
    fn maintenance(&self) -> Result<postgres::Client> {
        for db in ["postgres", "template1"] {
            if let Ok(conn) = self.connect(db) {
                return Ok(conn);
            }
        }
        bail!(
            "cannot connect to a maintenance database (tried postgres, template1) on {}",
            self.pg.display()
        )
    }

    fn quote_ident(s: &str) -> String {
        format!("\"{}\"", s.replace('"', "\"\""))
    }

    fn quote_lit(s: &str) -> String {
        format!("'{}'", s.replace('\'', "''"))
    }

    /// Block NEW connections and terminate existing ones (§2.2). Our own
    /// control-plane session sits in a different database and is unaffected.
    fn fence(admin: &mut postgres::Client, db: &str) -> Result<()> {
        admin.execute(
            &format!(
                "ALTER DATABASE {} WITH ALLOW_CONNECTIONS false",
                Self::quote_ident(db)
            ),
            &[],
        )?;
        if let Err(e) = admin.execute(
            &format!(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = {} AND pid <> pg_backend_pid()",
                Self::quote_lit(db)
            ),
            &[],
        ) {
            return match Self::unfence(admin, db) {
                Ok(()) => Err(e).context("terminating database connections; fence was removed"),
                Err(cleanup) => bail!("terminating database connections failed: {e}; removing the connection fence also failed: {cleanup}"),
            };
        }
        Ok(())
    }

    fn unfence(admin: &mut postgres::Client, db: &str) -> Result<()> {
        admin.execute(
            &format!(
                "ALTER DATABASE {} WITH ALLOW_CONNECTIONS true",
                Self::quote_ident(db)
            ),
            &[],
        )?;
        Ok(())
    }

    /// Idempotent healing: re-enable connections in case a previous process
    /// died mid-fence (§2.2 rule 2 / §4-T6).
    fn heal(admin: &mut postgres::Client, db: &str) {
        if let Err(e) = Self::unfence(admin, db) {
            eprintln!(
                "warning: {}",
                crate::util::mask(&format!("could not heal connection fence on {db}: {e:#}"))
            );
        }
    }

    fn db_exists(admin: &mut postgres::Client, db: &str) -> Result<bool> {
        let row = admin.query_opt(
            &format!(
                "SELECT 1 FROM pg_database WHERE datname = {}",
                Self::quote_lit(db)
            ),
            &[],
        )?;
        Ok(row.is_some())
    }

    fn db_size(admin: &mut postgres::Client, db: &str) -> Result<u64> {
        let row = admin.query_one(
            &format!("SELECT pg_database_size({})", Self::quote_lit(db)),
            &[],
        )?;
        let v: i64 = row.get(0);
        Ok(v.max(0) as u64)
    }

    fn server_version(conn: &mut postgres::Client) -> Result<String> {
        let row = conn.query_one("SELECT version()", &[])?;
        let v: String = row.get(0);
        Ok(v.split_whitespace().nth(1).unwrap_or("?").to_string())
    }

    /// Server-side block copy of the whole database (design §2.2): fence →
    /// CREATE DATABASE __dbsnap_<id> TEMPLATE target → unfence. ~1 s
    /// connection blackout; dev servers reconnect automatically.
    fn save_clone(
        &self,
        _store: &Storage,
        tag: &str,
        taken_ids: &HashSet<String>,
    ) -> Result<SaveOutcome> {
        let id = util::new_id(taken_ids);
        let snap_db = format!("{SNAP_PREFIX}{id}");
        let target = &self.pg.database;
        let mut admin = self.maintenance()?;
        Self::heal(&mut admin, target);

        if !Self::db_exists(&mut admin, target)? {
            bail!(
                "database {target:?} does not exist on {} — check the bound target (dbsnap status)",
                self.pg.display()
            );
        }
        let size = Self::db_size(&mut admin, target)?;
        let version = Self::server_version(&mut admin)?;

        Self::fence(&mut admin, target)?;
        if let Err(e) = admin.execute(
            &format!(
                "CREATE DATABASE {} TEMPLATE {}",
                Self::quote_ident(&snap_db),
                Self::quote_ident(target)
            ),
            &[],
        ) {
            // Guarantee the fence never outlives the operation (§4-T6).
            Self::unfence(&mut admin, target).ok();
            bail!("creating snapshot clone {snap_db} failed: {e:#}");
        }
        Self::unfence(&mut admin, target)?;

        Ok(SaveOutcome {
            record: SnapshotRecord {
                id,
                tag: tag.to_string(),
                created_at: Utc::now(),
                engine: "postgres".to_string(),
                location: Location::PgDatabase { name: snap_db },
                size_bytes: size,
                original_bytes: size,
                blake3: blake3::hash(
                    format!("pg-clone|{version}|{}|{size}", self.pg.display()).as_bytes(),
                )
                .to_hex()
                .to_string(),
                method: "pg-clone".to_string(),
                pinned: false,
                source_state: None,
            },
            linked: false,
        })
    }

    /// Portable artifact via `pg_dump -Fc` — no fencing, works while the dev
    /// server runs; survives container/volume rebuilds (§2.2).
    fn save_dump(
        &self,
        store: &Storage,
        tag: &str,
        taken_ids: &HashSet<String>,
    ) -> Result<SaveOutcome> {
        let check = Command::new("pg_dump").arg("--version").output();
        if check.is_err() {
            bail!(
                "pg_dump not found in PATH — install the PostgreSQL client tools, or use --method clone (default)"
            );
        }

        let id = util::new_id(taken_ids);
        let out_path = store.snapshots_dir().join(format!("{id}.dump"));
        let mut admin = self.maintenance()?;
        let size = Self::db_size(&mut admin, &self.pg.database)?;

        let output = Command::new("pg_dump")
            .args(["-Fc", "-d", &self.pg.connstring(), "-f"])
            .arg(&out_path)
            // password travels via env, never argv (§4-T10)
            .env("PGPASSWORD", self.pg.password.as_deref().unwrap_or(""))
            .env_remove("PGHOSTADDR")
            .output()?;
        if !output.status.success() {
            bail!(
                "pg_dump failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        let file_size = std::fs::metadata(&out_path)?.len();
        let hash = compress::hash_file(&out_path)?;
        let rel = format!("snapshots/{id}.dump");
        Ok(SaveOutcome {
            record: SnapshotRecord {
                id,
                tag: tag.to_string(),
                created_at: Utc::now(),
                engine: "postgres".to_string(),
                location: Location::File { path: rel },
                size_bytes: file_size,
                original_bytes: size,
                blake3: hash,
                method: "pg-dump".to_string(),
                pinned: false,
                source_state: None,
            },
            linked: false,
        })
    }

    /// Restore a server-side clone: fence once, clone the current state as
    /// the safety snapshot, DROP the target, RENAME the snapshot into place,
    /// unfence (§2.2).
    fn restore_clone(
        &self,
        store: &Storage,
        _record: &SnapshotRecord,
        snap_db: &str,
        safety_tag: Option<&str>,
    ) -> Result<RestoreOutcome> {
        if !snap_db.starts_with(SNAP_PREFIX) {
            bail!(
                "refusing to restore: {snap_db:?} is not a dbsnap-managed database (must start with {SNAP_PREFIX:?})"
            );
        }
        let target = &self.pg.database;
        let mut admin = self.maintenance()?;
        Self::heal(&mut admin, target);

        if !Self::db_exists(&mut admin, snap_db)? {
            bail!(
                "snapshot database {snap_db} no longer exists on {} — server-side snapshots do not survive container/volume rebuilds; save again or use --method dump",
                self.pg.display()
            );
        }

        let mut safety: Option<SnapshotRecord> = None;
        Self::fence(&mut admin, target)?;
        let outcome = (|| -> Result<()> {
            if let Some(tag) = safety_tag {
                let mut meta = store.load_metadata()?;
                let taken = meta.ids();
                let auto_id = util::new_id(&taken);
                let auto_name = format!("{SNAP_PREFIX}auto_{auto_id}");
                let size = Self::db_size(&mut admin, target)?;
                let version = Self::server_version(&mut admin)?;
                admin.execute(
                    &format!(
                        "CREATE DATABASE {} TEMPLATE {}",
                        Self::quote_ident(&auto_name),
                        Self::quote_ident(target)
                    ),
                    &[],
                )?;
                let safety_record = SnapshotRecord {
                    id: auto_id,
                    tag: tag.to_string(),
                    created_at: Utc::now(),
                    engine: "postgres".to_string(),
                    location: Location::PgDatabase { name: auto_name },
                    size_bytes: size,
                    original_bytes: size,
                    blake3: blake3::hash(
                        format!("pg-clone|{version}|{}|{size}", self.pg.display()).as_bytes(),
                    )
                    .to_hex()
                    .to_string(),
                    method: "pg-clone".to_string(),
                    pinned: false,
                    source_state: None,
                };
                meta.snapshots.push(safety_record.clone());
                store.write_metadata(&meta)?;
                safety = Some(safety_record);
            }
            admin.execute(&format!("DROP DATABASE {}", Self::quote_ident(target)), &[])?;
            admin.execute(
                &format!(
                    "ALTER DATABASE {} RENAME TO {}",
                    Self::quote_ident(snap_db),
                    Self::quote_ident(target)
                ),
                &[],
            )?;
            Ok(())
        })();

        if let Err(e) = outcome {
            Self::unfence(&mut admin, target).ok();
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
        Self::unfence(&mut admin, target)?;
        self.connect(target)
            .context("restore applied but PostgreSQL verification failed")?;
        Ok(RestoreOutcome { safety })
    }

    /// Restore a `pg_dump` artifact: verify the file (§4-T3), capture the
    /// target's encoding/locale/owner, safety-clone, drop + recreate the
    /// database with those attributes, then `pg_restore` into it.
    fn restore_dump(
        &self,
        store: &Storage,
        record: &SnapshotRecord,
        safety_tag: Option<&str>,
    ) -> Result<RestoreOutcome> {
        let dump_path = store.snapshot_path(record)?;
        if !dump_path.exists() {
            bail!("dump file {} is missing on disk", dump_path.display());
        }
        let actual = compress::hash_file(&dump_path)?;
        if actual != record.blake3 {
            bail!(
                "dump integrity check FAILED (hash mismatch)\n  expected blake3: {}\n  actual   blake3: {actual}\nRefusing to restore from a corrupt dump.",
                record.blake3
            );
        }
        if Command::new("pg_restore")
            .arg("--version")
            .output()
            .is_err()
        {
            bail!("pg_restore not found in PATH — install the PostgreSQL client tools");
        }

        let target = &self.pg.database;
        let mut admin = self.maintenance()?;
        Self::heal(&mut admin, target);

        let attrs = admin.query_opt(
            &format!(
                "SELECT pg_encoding_to_char(encoding), datcollate, datctype, rolname \
                 FROM pg_database JOIN pg_roles ON pg_database.datdba = pg_roles.oid \
                 WHERE datname = {}",
                Self::quote_lit(target)
            ),
            &[],
        )?;
        let Some(attrs) = attrs else {
            bail!(
                "database {target:?} does not exist on {} — nothing to replace",
                self.pg.display()
            );
        };
        let encoding: String = attrs.get(0);
        let collate: String = attrs.get(1);
        let ctype: String = attrs.get(2);
        let owner: String = attrs.get(3);

        let mut safety: Option<SnapshotRecord> = None;
        Self::fence(&mut admin, target)?;
        let outcome = (|| -> Result<()> {
            if let Some(tag) = safety_tag {
                let mut meta = store.load_metadata()?;
                let taken = meta.ids();
                let auto_id = util::new_id(&taken);
                let auto_name = format!("{SNAP_PREFIX}auto_{auto_id}");
                let size = Self::db_size(&mut admin, target)?;
                admin.execute(
                    &format!(
                        "CREATE DATABASE {} TEMPLATE {}",
                        Self::quote_ident(&auto_name),
                        Self::quote_ident(target)
                    ),
                    &[],
                )?;
                let safety_record = SnapshotRecord {
                    id: auto_id,
                    tag: tag.to_string(),
                    created_at: Utc::now(),
                    engine: "postgres".to_string(),
                    location: Location::PgDatabase { name: auto_name },
                    size_bytes: size,
                    original_bytes: size,
                    blake3: blake3::hash(
                        format!("pg-clone|{}|{size}", self.pg.display()).as_bytes(),
                    )
                    .to_hex()
                    .to_string(),
                    method: "pg-clone".to_string(),
                    pinned: false,
                    source_state: None,
                };
                meta.snapshots.push(safety_record.clone());
                store.write_metadata(&meta)?;
                safety = Some(safety_record);
            }
            admin.execute(&format!("DROP DATABASE {}", Self::quote_ident(target)), &[])?;
            admin.execute(
                &format!(
                    "CREATE DATABASE {} TEMPLATE template0 ENCODING {} LC_COLLATE {} LC_CTYPE {} OWNER {}",
                    Self::quote_ident(target),
                    Self::quote_lit(&encoding),
                    Self::quote_lit(&collate),
                    Self::quote_lit(&ctype),
                    Self::quote_ident(&owner)
                ),
                &[],
            )?;
            Ok(())
        })();
        if let Err(e) = outcome {
            Self::unfence(&mut admin, target).ok();
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
        Self::unfence(&mut admin, target)?;

        let output = Command::new("pg_restore")
            .args(["-d", &self.pg.connstring(), "--no-owner"])
            .arg(&dump_path)
            .env("PGPASSWORD", self.pg.password.as_deref().unwrap_or(""))
            .env_remove("PGHOSTADDR")
            .output()?;
        if !output.status.success() {
            let hint = safety
                .as_ref()
                .map(|r| {
                    format!(
                        "\nthe pre-restore state was saved as snapshot {} — recover with: dbsnap restore {}",
                        r.id, r.id
                    )
                })
                .unwrap_or_default();
            bail!(
                "pg_restore failed: {}{hint}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        self.connect(target)
            .context("restore applied but PostgreSQL verification failed")?;
        Ok(RestoreOutcome { safety })
    }
}

impl DatabaseEngine for PostgresEngine {
    fn probe(&self) -> Result<EngineInfo> {
        let mut conn = self.connect(&self.pg.database)?;
        let version = Self::server_version(&mut conn)?;
        let size = Self::db_size(&mut conn, &self.pg.database)?;
        Ok(EngineInfo {
            engine: "postgres".to_string(),
            detail: format!("PostgreSQL {version} @ {}", self.pg.display()),
            size_bytes: Some(size),
        })
    }

    fn save(
        &self,
        store: &Storage,
        tag: &str,
        opts: &SaveOpts,
        _previous: Option<&SnapshotRecord>,
        taken_ids: &HashSet<String>,
    ) -> Result<SaveOutcome> {
        match opts.method {
            SaveMethod::Clone => self.save_clone(store, tag, taken_ids),
            SaveMethod::Dump => self.save_dump(store, tag, taken_ids),
        }
    }

    fn restore(
        &self,
        store: &Storage,
        record: &SnapshotRecord,
        safety_tag: Option<&str>,
    ) -> Result<RestoreOutcome> {
        match &record.location {
            Location::PgDatabase { name } => self.restore_clone(store, record, name, safety_tag),
            Location::File { .. } if record.method == "pg-dump" => {
                self.restore_dump(store, record, safety_tag)
            }
            Location::File { .. } | Location::Manifest { .. } => bail!(
                "snapshot {} is a {} snapshot — it cannot be restored into PostgreSQL (bound engine: postgres)",
                record.id,
                record.engine
            ),
        }
    }
}

/// Drop a dbsnap-managed clone database (GC path for prune/retention).
/// Refuses anything outside the `__dbsnap_` prefix (§2.2 rule 1).
pub fn drop_managed_db(pg: &ServerTarget, name: &str) -> Result<()> {
    if !name.starts_with(SNAP_PREFIX) {
        bail!("refusing to drop {name:?} — not a dbsnap-managed database");
    }
    let engine = PostgresEngine::new(pg.clone());
    let mut admin = engine.maintenance()?;
    if !PostgresEngine::db_exists(&mut admin, name)? {
        return Ok(()); // already gone
    }
    admin.execute(
        &format!(
            "DROP DATABASE IF EXISTS {}",
            PostgresEngine::quote_ident(name)
        ),
        &[],
    )?;
    Ok(())
}
