use super::{DatabaseEngine, EngineInfo, RestoreOutcome, SaveOpts, SaveOutcome};
use crate::config::ServerTarget;
use crate::storage::{compress, Location, SnapshotRecord, Storage};
use crate::util;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::collections::HashSet;
use std::process::{Command, Stdio};

/// MySQL has no physical hot-copy API — snapshots are consistent logical
/// dumps (`mysqldump --single-transaction`, design §2.3). Restore is disabled
/// until it can satisfy the project's target and recovery safety rules.

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
                common(&["--single-transaction", "--hex-blob"]),
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

        let mut last_err = String::new();
        for flags in self.dump_flag_sets(flavor) {
            let output = Command::new(&dump_bin)
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
                    let (original_bytes, size_bytes, hash) =
                        compress::compress_to(&raw_tmp, &zst_path)?;
                    std::fs::remove_file(&raw_tmp).ok();
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
            std::fs::remove_file(&raw_tmp).ok();
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
        _store: &Storage,
        _record: &SnapshotRecord,
        _safety_tag: Option<&str>,
    ) -> Result<RestoreOutcome> {
        bail!("MySQL/MariaDB restore is temporarily disabled: the previous implementation could replace the target before replay completed. Existing snapshots remain available; restore will return after a safe recovery design is implemented.")
    }

    fn quote_lit(s: &str) -> String {
        format!("'{}'", s.replace('\'', "''"))
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
}
