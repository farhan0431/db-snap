use super::{DatabaseEngine, EngineInfo, RestoreOutcome, SaveOpts, SaveOutcome};
use crate::config::ServerTarget;
use crate::storage::{compress, Location, SnapshotRecord, Storage, TemporaryFile};
use crate::util;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

/// MySQL has no physical hot-copy API — snapshots are consistent logical
/// dumps (`mysqldump --single-transaction`, design §2.3).

#[derive(Debug, Clone, Copy, PartialEq)]
enum Flavor {
    Mysql,
    Maria,
}

impl Flavor {
    fn name(self) -> &'static str {
        match self {
            Flavor::Mysql => "MySQL",
            Flavor::Maria => "MariaDB",
        }
    }
}

pub struct MysqlEngine {
    target: ServerTarget,
}

impl MysqlEngine {
    pub fn new(target: ServerTarget) -> Self {
        Self { target }
    }

    /// Client binaries talk TCP; the literal host `localhost` means a unix
    /// socket and fails in typical Docker/Windows setups (design §2.3).
    fn tcp_host(&self) -> String {
        if self.target.host.eq_ignore_ascii_case("localhost") {
            "127.0.0.1".to_string()
        } else {
            self.target.host.clone()
        }
    }

    /// Common CLI args for mysql-family clients. The password travels via
    /// the MYSQL_PWD env — never argv (§4-T10).
    fn base_args(&self) -> Vec<String> {
        vec![
            "--protocol=TCP".into(),
            "--default-character-set=utf8mb4".into(),
            "-h".into(),
            self.tcp_host(),
            "-P".into(),
            self.target.port.to_string(),
            "-u".into(),
            self.target.user.clone(),
        ]
    }

    fn run_client(&self, bin: &str, args: &[String]) -> Result<std::process::Output> {
        Command::new(bin)
            .arg("--no-defaults")
            .args(args)
            .env("MYSQL_PWD", self.target.password.as_deref().unwrap_or(""))
            .output()
            .with_context(|| format!("spawning {bin}"))
    }

    /// Resolve a client binary, honoring the detected flavor with a
    /// cross-flavor fallback (mariadb-dump ↔ mysqldump, §2.3).
    fn pick_bin(&self, flavor: Flavor, dump: bool) -> Result<String> {
        let (primary, fallback) = match (flavor, dump) {
            (Flavor::Maria, true) => ("mariadb-dump", "mysqldump"),
            (Flavor::Maria, false) => ("mariadb", "mysql"),
            (Flavor::Mysql, true) => ("mysqldump", "mariadb-dump"),
            (Flavor::Mysql, false) => ("mysql", "mariadb"),
        };
        for bin in [primary, fallback] {
            let probe = self.run_client(bin, &["--version".to_string()]);
            if probe.map(|o| o.status.success()).unwrap_or(false) {
                return Ok(bin.to_string());
            }
        }
        bail!(
            "neither {primary} nor {fallback} found in PATH — install the {} client tools",
            flavor.name()
        )
    }

    /// Run one SQL statement via the `mysql` client in batch mode; returns
    /// raw stdout (tab-separated, no headers).
    fn sql(&self, bin: &str, query: &str) -> Result<String> {
        let mut args = self.base_args();
        args.push("-N".into()); // no column names
        args.push("-B".into()); // batch (tab-separated)
        args.push("-e".into());
        args.push(query.to_string());
        let output = self.run_client(bin, &args)?;
        if !output.status.success() {
            bail!(
                "SQL failed ({}): {}",
                bin,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn detect_flavor(&self, exec_bin: &str) -> Result<(Flavor, String)> {
        let out = self.sql(exec_bin, "SELECT VERSION();")?;
        let version = out.lines().next().unwrap_or("").trim().to_string();
        if version.is_empty() {
            bail!("could not read server version");
        }
        let flavor = if version.to_ascii_lowercase().contains("mariadb") {
            Flavor::Maria
        } else {
            Flavor::Mysql
        };
        Ok((flavor, version))
    }

    /// Curated dump flag set per flavor, plus a fallback set for the known
    /// cross-version flag incompatibilities (§2.3).
    fn dump_flag_sets(&self, flavor: Flavor) -> Vec<Vec<String>> {
        let db = self.target.database.clone();
        let common = |flags: &[&str]| -> Vec<String> {
            let mut out: Vec<String> = flags.iter().map(|f| f.to_string()).collect();
            out.push("--databases".into());
            out.push(db.clone());
            out
        };
        match flavor {
            Flavor::Mysql => vec![
                common(&[
                    "--single-transaction",
                    "--routines",
                    "--triggers",
                    "--events",
                    "--hex-blob",
                    "--no-tablespaces",
                    "--set-gtid-purged=OFF",
                ]),
                // older servers: disable column statistics added by 8.x clients
                common(&[
                    "--single-transaction",
                    "--routines",
                    "--triggers",
                    "--events",
                    "--hex-blob",
                    "--no-tablespaces",
                    "--set-gtid-purged=OFF",
                    "--column-statistics=0",
                ]),
                // MariaDB's dump client does not support MySQL's GTID or
                // column-statistics flags even when connected to MySQL.
                common(&[
                    "--single-transaction",
                    "--routines",
                    "--triggers",
                    "--events",
                    "--hex-blob",
                    "--no-tablespaces",
                ]),
            ],
            Flavor::Maria => vec![
                common(&[
                    "--single-transaction",
                    "--routines",
                    "--triggers",
                    "--events",
                    "--hex-blob",
                    "--no-tablespaces",
                ]),
                // MySQL 8's mysqldump queries COLUMN_STATISTICS by default,
                // but that table does not exist on MariaDB servers.
                common(&[
                    "--single-transaction",
                    "--routines",
                    "--triggers",
                    "--events",
                    "--hex-blob",
                    "--no-tablespaces",
                    "--column-statistics=0",
                ]),
            ],
        }
    }

    fn save_dump(
        &self,
        store: &Storage,
        tag: &str,
        taken_ids: &HashSet<String>,
    ) -> Result<SaveOutcome> {
        let exec_bin = self.pick_bin(Flavor::Mysql, false)?;
        let (flavor, version) = self.detect_flavor(&exec_bin)?;
        let dump_bin = self.pick_bin(flavor, true)?;

        let id = util::new_id(taken_ids);
        let raw_tmp = store.tmp_dir().join(format!("save-{id}.sql"));
        let zst_path = store.snapshots_dir().join(format!("{id}.sql.zst"));
        let _raw_guard = TemporaryFile::create(raw_tmp.clone())?;

        let mut last_err = String::new();
        for flags in self.dump_flag_sets(flavor) {
            std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&raw_tmp)?;
            let output = Command::new(&dump_bin)
                .arg("--no-defaults")
                .args(self.base_args())
                .args(&flags)
                .env("MYSQL_PWD", self.target.password.as_deref().unwrap_or(""))
                .arg("--result-file")
                .arg(&raw_tmp)
                .stderr(Stdio::piped())
                .output()
                .with_context(|| format!("spawning {dump_bin}"));

            match output {
                Ok(o)
                    if o.status.success()
                        && std::fs::metadata(&raw_tmp)
                            .map(|m| m.len() > 0)
                            .unwrap_or(false) =>
                {
                    // wrap the raw SQL in zstd, hashing the compressed stream
                    let compressed_guard = TemporaryFile::create(zst_path.clone())?;
                    let (original_bytes, size_bytes, hash) =
                        compress::compress_to(&raw_tmp, &zst_path)?;
                    compressed_guard.persist();
                    return Ok(SaveOutcome {
                        record: SnapshotRecord {
                            id: id.clone(),
                            tag: tag.to_string(),
                            created_at: Utc::now(),
                            engine: "mysql".to_string(),
                            location: Location::File {
                                path: format!("snapshots/{id}.sql.zst"),
                            },
                            size_bytes,
                            original_bytes,
                            blake3: hash,
                            method: "mysql-dump".to_string(),
                            pinned: false,
                            source_state: None,
                        },
                        linked: false,
                    });
                }
                Ok(o) => {
                    last_err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                }
                Err(e) => {
                    last_err = format!("{e:#}");
                }
            }
        }
        bail!(
            "{}dump failed with every known flag set for {} {}:\n{last_err}",
            dump_bin,
            flavor.name(),
            version
        )
    }

    fn restore_dump(
        &self,
        store: &Storage,
        record: &SnapshotRecord,
        safety_tag: Option<&str>,
    ) -> Result<RestoreOutcome> {
        let mut meta = store.load_metadata()?;
        if store.free_bytes()?
            < record
                .original_bytes
                .saturating_mul(2)
                .max(100 * 1024 * 1024)
        {
            bail!("insufficient free space for MySQL restore staging and recovery");
        }
        if meta.project_fingerprint.target != self.target.display() {
            bail!("MySQL dumps can only be restored to their original target");
        }
        if !matches!(record.location, Location::File { .. }) || record.method != "mysql-dump" {
            bail!("MySQL restore requires a mysql-dump file snapshot");
        }
        let raw = store
            .tmp_dir()
            .join(format!("mysql-restore-{}.sql", util::new_id(&meta.ids())));
        let _guard = TemporaryFile::create(raw.clone())?;
        compress::decompress_verified(
            &store.snapshot_path(record)?,
            &raw,
            &record.blake3,
            record.original_bytes,
        )?;
        // Legacy dumps contain USE and CREATE DATABASE. Replay only on the
        // original database; never rewrite SQL with string substitution.
        Self::check_dump_database(&raw, &self.target.database)?;
        let journal = store.root.join("mysql-restore.json");
        let pending: Option<String> = if journal.exists() {
            Some(serde_json::from_slice(&std::fs::read(&journal)?)?)
        } else {
            None
        };
        if let Some(id) = &pending {
            if id != &record.id {
                bail!("interrupted MySQL restore: recover first with dbsnap restore {id}");
            }
        } else if safety_tag.is_none() {
            bail!("MySQL restore requires a safety snapshot; omit --no-safety-snapshot");
        }
        let bin = self.pick_bin(Flavor::Mysql, false)?;
        let cleanup = self.cleanup_sql(&bin)?;
        let safety = if pending.is_none() {
            let mut saved = self
                .save_dump(store, safety_tag.unwrap(), &meta.ids())?
                .record;
            saved.pinned = true; // Never prune the recovery copy after failure.
            meta.snapshots.push(saved.clone());
            store.write_metadata(&meta)?;
            // create_new prevents overwriting a concurrent/replaced journal.
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&journal)?;
            file.write_all(&serde_json::to_vec(&saved.id)?)?;
            file.sync_all()?;
            Some(saved)
        } else {
            None
        };
        let recovery = pending
            .as_deref()
            .or_else(|| safety.as_ref().map(|r| r.id.as_str()))
            .unwrap();
        let result = (|| -> Result<()> {
            self.sql(&bin, &cleanup)?;
            if std::env::var("DBSNAP_FAILPOINT").as_deref() == Ok("mysql_after_cleanup") {
                bail!("injected failure after MySQL cleanup");
            }
            if std::env::var("DBSNAP_FAILPOINT").as_deref() == Ok("mysql_exit_after_cleanup") {
                std::process::exit(86);
            }
            let output = Command::new(&bin)
                .arg("--no-defaults")
                .args(self.base_args())
                .args(["--binary-mode", "--database", &self.target.database])
                .env("MYSQL_PWD", self.target.password.as_deref().unwrap_or(""))
                .stdin(std::fs::File::open(&raw)?)
                .output()
                .context("replaying MySQL dump")?;
            if !output.status.success() {
                bail!(
                    "MySQL replay failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            Ok(())
        })();
        if let Err(error) = result {
            bail!("{error:#}\nTarget may be partially restored. Safety snapshot is pinned; recover with: dbsnap restore {recovery}");
        }
        std::fs::remove_file(journal)?;
        // Recovery snapshots stay pinned deliberately, including after success.
        Ok(RestoreOutcome { safety })
    }

    fn quote_ident(name: &str) -> String {
        format!("`{}`", name.replace('`', "``"))
    }

    fn check_dump_database(path: &std::path::Path, database: &str) -> Result<()> {
        let expected = format!("USE {};", Self::quote_ident(database));
        let mut found = false;
        for line in BufReader::new(std::fs::File::open(path)?).lines() {
            let line = line?;
            if line.starts_with("USE ") {
                if line != expected {
                    bail!("dump selects a different database; restore aborted");
                }
                found = true;
            }
            if line.to_ascii_uppercase().starts_with("DROP DATABASE ") {
                bail!("dump drops a database; restore aborted");
            }
        }
        if !found {
            bail!("dump has no matching database selection; restore aborted");
        }
        Ok(())
    }

    /// Enumerate only objects belonging to the bound target. The database
    /// itself (and its grants/default charset) is never dropped.
    fn cleanup_sql(&self, bin: &str) -> Result<String> {
        let db = Self::quote_lit(&self.target.database);
        let exists = self.sql(
            bin,
            &format!("SELECT COUNT(*) FROM information_schema.SCHEMATA WHERE SCHEMA_NAME={db}"),
        )?;
        if exists.trim() != "1" {
            bail!("target database does not exist");
        }
        let unsupported = self.sql(bin, &format!("SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA={db} AND TABLE_TYPE NOT IN ('BASE TABLE','VIEW')"))?;
        if unsupported.trim() != "0" {
            bail!("target contains unsupported objects (for example MariaDB sequences); restore aborted");
        }
        let external = self.sql(bin, &format!("SELECT COUNT(*) FROM information_schema.KEY_COLUMN_USAGE WHERE REFERENCED_TABLE_SCHEMA={db} AND TABLE_SCHEMA<>{db}"))?;
        if external.trim() != "0" {
            bail!("another database has foreign keys referencing the target; restore aborted");
        }
        let objects = self.sql(bin, &format!(
            "SELECT 'EVENT', HEX(EVENT_NAME) FROM information_schema.EVENTS WHERE EVENT_SCHEMA={db} UNION ALL SELECT 'VIEW', HEX(TABLE_NAME) FROM information_schema.VIEWS WHERE TABLE_SCHEMA={db} UNION ALL SELECT ROUTINE_TYPE, HEX(ROUTINE_NAME) FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA={db} UNION ALL SELECT 'TABLE', HEX(TABLE_NAME) FROM information_schema.TABLES WHERE TABLE_SCHEMA={db} AND TABLE_TYPE='BASE TABLE'"
        ))?;
        let mut sql = String::from("SET FOREIGN_KEY_CHECKS=0;\n");
        for row in objects.lines() {
            let (kind, hex) = row
                .split_once('\t')
                .context("invalid MySQL object listing")?;
            if !matches!(kind, "EVENT" | "VIEW" | "PROCEDURE" | "FUNCTION" | "TABLE") {
                bail!("unsupported MySQL object type");
            }
            if hex.len() % 2 != 0 || !hex.is_ascii() {
                bail!("invalid MySQL object name");
            }
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let name = String::from_utf8(bytes)?;
            sql.push_str(&format!(
                "DROP {kind} {}.{};\n",
                Self::quote_ident(&self.target.database),
                Self::quote_ident(&name)
            ));
        }
        Ok(sql)
    }

    fn quote_lit(s: &str) -> String {
        // Hex literals avoid quote/backslash interpretation differences across
        // MySQL SQL modes, including NO_BACKSLASH_ESCAPES.
        let hex: String = s.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
        format!("CONVERT(0x{hex} USING utf8mb4)")
    }
}

impl DatabaseEngine for MysqlEngine {
    fn probe(&self) -> Result<EngineInfo> {
        let exec_bin = self.pick_bin(Flavor::Mysql, false)?;
        let (flavor, version) = self.detect_flavor(&exec_bin)?;
        let db_lit = Self::quote_lit(&self.target.database);
        let out = self.sql(
            &exec_bin,
            &format!(
                "SELECT COALESCE(SUM(data_length + index_length), 0) FROM information_schema.TABLES WHERE table_schema = {db_lit};"
            ),
        )?;
        let size: u64 = out.trim().parse().unwrap_or(0);
        Ok(EngineInfo {
            engine: "mysql".to_string(),
            detail: format!("{} {version} @ {}", flavor.name(), self.target.display()),
            size_bytes: Some(size),
        })
    }

    fn save(
        &self,
        store: &Storage,
        tag: &str,
        _opts: &SaveOpts,
        _previous: Option<&SnapshotRecord>,
        taken_ids: &HashSet<String>,
    ) -> Result<SaveOutcome> {
        // MySQL has one safe method: consistent logical dump. Both --method
        // values map here so MCP's default works unchanged.
        self.save_dump(store, tag, taken_ids)
    }

    fn restore(
        &self,
        store: &Storage,
        record: &SnapshotRecord,
        safety_tag: Option<&str>,
    ) -> Result<RestoreOutcome> {
        if record.engine != "mysql" {
            bail!(
                "snapshot {} is a {} snapshot — it cannot be restored into MySQL/MariaDB (bound engine: mysql)",
                record.id,
                record.engine
            );
        }
        self.restore_dump(store, record, safety_tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_dump_selection_must_match_target() -> Result<()> {
        let path =
            std::env::temp_dir().join(format!("dbsnap-mysql-selection-{}.sql", std::process::id()));
        let _guard = TemporaryFile::create(path.clone())?;
        std::fs::write(&path, "USE `app``name`;\n")?;
        assert!(MysqlEngine::check_dump_database(&path, "app`name").is_ok());
        assert!(MysqlEngine::check_dump_database(&path, "other").is_err());
        std::fs::write(&path, "USE `app`;\nUSE `other`;\n")?;
        assert!(MysqlEngine::check_dump_database(&path, "app").is_err());
        std::fs::write(&path, "DROP DATABASE `app`;\nUSE `app`;\n")?;
        assert!(MysqlEngine::check_dump_database(&path, "app").is_err());
        std::fs::write(&path, "SELECT 1;\n")?;
        assert!(MysqlEngine::check_dump_database(&path, "app").is_err());
        Ok(())
    }

    #[test]
    fn mariadb_fallback_supports_mysql_dump_without_losing_objects() {
        let engine = MysqlEngine::new(ServerTarget {
            host: "localhost".into(),
            port: 3306,
            user: "test".into(),
            password: None,
            database: "app".into(),
        });
        let flags = engine.dump_flag_sets(Flavor::Maria);
        assert!(flags.iter().any(|set| {
            [
                "--column-statistics=0",
                "--single-transaction",
                "--routines",
                "--triggers",
                "--events",
                "--databases",
                "app",
            ]
            .iter()
            .all(|flag| set.iter().any(|value| value == flag))
        }));
    }

    #[test]
    fn mysql_fallback_supports_mariadb_dump_without_losing_objects() {
        let engine = MysqlEngine::new(ServerTarget {
            host: "localhost".into(),
            port: 3306,
            user: "test".into(),
            password: None,
            database: "app".into(),
        });
        assert!(engine.dump_flag_sets(Flavor::Mysql).iter().any(|set| {
            [
                "--single-transaction",
                "--routines",
                "--triggers",
                "--events",
                "--databases",
                "app",
            ]
            .iter()
            .all(|flag| set.iter().any(|value| value == flag))
                && !set.iter().any(|flag| {
                    flag.starts_with("--set-gtid-purged") || flag.starts_with("--column-statistics")
                })
        }));
    }

    #[test]
    fn every_dump_fallback_preserves_database_objects() {
        let engine = MysqlEngine::new(ServerTarget {
            host: "localhost".into(),
            port: 3306,
            user: "test".into(),
            password: None,
            database: "app".into(),
        });
        for flavor in [Flavor::Mysql, Flavor::Maria] {
            for flags in engine.dump_flag_sets(flavor) {
                for required in [
                    "--single-transaction",
                    "--routines",
                    "--triggers",
                    "--events",
                    "--hex-blob",
                    "--databases",
                ] {
                    assert!(
                        flags.iter().any(|flag| flag == required),
                        "{flavor:?} fallback missing {required}"
                    );
                }
            }
        }
    }

    #[test]
    fn database_name_is_encoded_without_sql_syntax() {
        assert_eq!(
            MysqlEngine::quote_lit("a\\'b"),
            "CONVERT(0x615c2762 USING utf8mb4)"
        );
    }
}
