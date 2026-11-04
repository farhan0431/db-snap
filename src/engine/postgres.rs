use super::{DatabaseEngine, EngineInfo, RestoreOutcome, SaveMethod, SaveOpts, SaveOutcome};
use crate::config::ServerTarget;
use crate::storage::{compress, Location, SnapshotRecord, Storage, TemporaryFile};
use crate::util;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use postgres::NoTls;
use std::collections::HashSet;
use std::io::Write;
use std::process::Command;

/// Prefix reserved for dbsnap-managed databases (design §2.2 rule 1) —
/// this engine NEVER drops or renames databases outside it.
pub const SNAP_PREFIX: &str = "__dbsnap_";

/// A dbsnap-managed server-side clone of the target database.
pub struct PostgresEngine {
    pg: ServerTarget,
}

impl PostgresEngine {
    fn restore_failpoint(name: &str) {
        if std::env::var("DBSNAP_TEST_FAILPOINT").as_deref() == Ok(name) {
            std::process::exit(86);
        }
    }
    fn journal_path(store: &Storage) -> std::path::PathBuf {
        store.root.join("postgres-restore.json")
    }

    fn write_restore_journal(&self, store: &Storage, previous: &str) -> Result<()> {
        let path = Self::journal_path(store);
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)
            .context("a PostgreSQL restore journal already exists or cannot be created; resolve it before another restore")?;
        let data = serde_json::json!({"version":1,"target":self.pg.display(),"previous":previous});
        file.write_all(serde_json::to_string(&data)?.as_bytes())?;
        file.sync_all()?;
        #[cfg(unix)]
        std::fs::File::open(&store.root)?.sync_all()?;
        Ok(())
    }

    /// Recover only an unambiguous interrupted rename. Never drop a database
    /// or overwrite an existing target while interpreting the journal.
    pub fn recover_interrupted(&self, store: &Storage) -> Result<()> {
        let path = Self::journal_path(store);
        if !path.exists() {
            return Ok(());
        }
        let journal: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)
            .context("invalid PostgreSQL restore journal; manual recovery required")?;
        let previous = journal["previous"].as_str().unwrap_or("");
        let suffix = previous.strip_prefix("__dbsnap_previous_").unwrap_or("");
        if journal["version"] != 1
            || journal["target"].as_str() != Some(self.pg.display().as_str())
            || suffix.len() != 6
            || !suffix.bytes().all(|b| b.is_ascii_hexdigit())
        {
            bail!("invalid or mismatched PostgreSQL restore journal; manual recovery required");
        }
        let mut admin = self.maintenance()?;
        let target_exists = Self::db_exists(&mut admin, &self.pg.database)?;
        let previous_exists = Self::db_exists(&mut admin, previous)?;
        match (target_exists, previous_exists) {
            (false, true) => {
                admin.execute(&format!("ALTER DATABASE {} RENAME TO {}", Self::quote_ident(previous), Self::quote_ident(&self.pg.database)), &[])?;
                Self::unfence(&mut admin, &self.pg.database)?;
            }
            (true, false) => Self::unfence(&mut admin, &self.pg.database)?,
            (true, true) => bail!("interrupted PostgreSQL restore: both target and preserved database {previous} exist; inspect them before manual recovery"),
            (false, false) => bail!("interrupted PostgreSQL restore: both target and preserved database are missing; manual recovery required"),
        }
        std::fs::remove_file(path).context("removing PostgreSQL restore journal")?;
        Ok(())
    }
    fn client_major(version: &str) -> Option<u32> {
        version.split_whitespace().find_map(|word| {
            let first = word.split('.').next()?;
            first.parse::<u32>().ok()
        })
    }

    fn require_matching_client(admin: &mut postgres::Client, bin: &str) -> Result<()> {
        let output = Command::new(bin)
            .arg("--version")
            .output()
            .with_context(|| {
                format!("{bin} not found in PATH — install matching PostgreSQL client tools")
            })?;
        if !output.status.success() {
            bail!("could not determine {bin} version");
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let client = Self::client_major(&text)
            .ok_or_else(|| anyhow::anyhow!("could not parse {bin} version: {text}"))?;
        let number: String = admin.query_one("SHOW server_version_num", &[])?.get(0);
        let numeric: u32 = number
            .parse()
            .context("invalid PostgreSQL server_version_num")?;
        let server = numeric / 10000;
        if client != server {
            bail!("{bin} major version {client} does not match PostgreSQL server major version {server}; install matching client tools and retry before restoring or saving a dump");
        }
        Ok(())
    }
    pub fn new(pg: ServerTarget) -> Self {
        Self { pg }
    }

    pub fn verify_clone_exists(&self, name: &str) -> Result<()> {
        let mut admin = self.maintenance()?;
        if !Self::db_exists(&mut admin, name)? {
            bail!("PostgreSQL snapshot clone is missing");
        }
        Ok(())
    }

    pub fn managed_exists(&self, name: &str) -> Result<bool> {
        if !name.starts_with(SNAP_PREFIX) {
            bail!("not a managed database");
        }
        Self::db_exists(&mut self.maintenance()?, name)
    }

    pub(crate) fn create_sandbox(
        &self,
        store: &Storage,
        record: &SnapshotRecord,
        ids: &HashSet<String>,
    ) -> Result<String> {
        let name = format!("{SNAP_PREFIX}sandbox_{}", util::new_id(ids));
        if record.engine == "postgres" && record.method == "pg-dump" {
            let archive = store.snapshot_path(record)?;
            if archive.metadata()?.len() != record.size_bytes
                || compress::hash_file(&archive)? != record.blake3
            {
                bail!("PostgreSQL archive integrity check failed");
            }
            let mut admin = self.maintenance()?;
            Self::require_matching_client(&mut admin, "pg_restore")?;
            admin.batch_execute(&format!(
                "CREATE DATABASE {} TEMPLATE template0",
                Self::quote_ident(&name)
            ))?;
            if let Err(error) = self.replay_archive(&archive, &name, true) {
                if let Err(cleanup) = drop_managed_db(&self.pg, &name) {
                    bail!("{error:#}; cleanup failed for {name}: {cleanup:#}");
                }
                return Err(error);
            }
            if std::env::var("DBSNAP_FAILPOINT").as_deref() == Ok("pg_sandbox_after_replay") {
                eprintln!(
                    "{}",
                    util::mask(&format!(
                        "interrupted sandbox suffix: {}",
                        &name["__dbsnap_sandbox_".len()..]
                    ))
                );
                std::process::exit(86);
            }
            return Ok(name);
        }
        let source = Self::clone_name(record)?;
        self.maintenance()?.batch_execute(&format!(
            "CREATE DATABASE {} TEMPLATE {}",
            Self::quote_ident(&name),
            Self::quote_ident(source)
        ))?;
        Ok(name)
    }

    pub(crate) fn clone_name(record: &SnapshotRecord) -> Result<&str> {
        let Location::PgDatabase { name: source } = &record.location else {
            bail!("expected a PostgreSQL clone snapshot");
        };
        if ![
            format!("{SNAP_PREFIX}{}", record.id),
            format!("{SNAP_PREFIX}auto_{}", record.id),
        ]
        .contains(source)
            || record.engine != "postgres"
        {
            bail!("invalid source clone");
        }
        Ok(source)
    }

    fn replay_archive(
        &self,
        archive: &std::path::Path,
        database: &str,
        sandbox: bool,
    ) -> Result<()> {
        let mut target = self.pg.clone();
        target.database = database.into();
        let engine = Self::new(target);
        let mut command = Command::new("pg_restore");
        engine.configure_dump_tls(&mut command);
        command.args([
            "-d",
            &engine.client_connstring(),
            "--no-owner",
            "--exit-on-error",
        ]);
        if sandbox {
            command.args(["--no-privileges", "--no-tablespaces"]);
        }
        let output = command
            .arg(archive)
            .env("PGPASSWORD", self.pg.password.as_deref().unwrap_or(""))
            .output()?;
        if !output.status.success() {
            bail!(
                "pg_restore into staging failed; target was not changed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        engine
            .connect(database)
            .context("staged PostgreSQL database verification failed")?;
        Ok(())
    }

    pub(crate) fn dump_clone_to(
        &self,
        record: &SnapshotRecord,
        output: &std::path::Path,
    ) -> Result<()> {
        let mut target = self.pg.clone();
        target.database = Self::clone_name(record)?.into();
        Self::require_matching_client(&mut self.maintenance()?, "pg_dump")?;
        Self::new(target).dump_to(output)
    }

    fn dump_to(&self, path: &std::path::Path) -> Result<()> {
        let mut command = Command::new("pg_dump");
        self.configure_dump_tls(&mut command);
        let output = command
            .args(["-Fc", "-d", &self.client_connstring(), "-f"])
            .arg(path)
            .env("PGPASSWORD", self.pg.password.as_deref().unwrap_or(""))
            .output()?;
        if !output.status.success() {
            bail!(
                "pg_dump failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    /// Compare catalogued server-side clones using read-only transactions.
    /// Dump archives must not be replayed into a live server for inspection.
    pub fn diff(
        &self,
        store: &Storage,
        left: &SnapshotRecord,
        right: &SnapshotRecord,
    ) -> Result<String> {
        if left.method == "pg-dump" && right.method == "pg-dump" {
            return super::postgres_archive_diff::compare(store, left, right);
        }
        use std::collections::BTreeMap;
        type Objects = BTreeMap<(String, String), (String, Option<i64>)>;
        fn inspect(engine: &PostgresEngine, record: &SnapshotRecord) -> Result<Objects> {
            let name = match &record.location {
                Location::PgDatabase { name } if name == &format!("{SNAP_PREFIX}{}", record.id) && record.method == "pg-clone" => name,
                _ => bail!("PostgreSQL diff requires two clone snapshots or two pg-dump archives; mixed snapshot types are unsupported"),
            };
            let mut client = engine.connect(name)?;
            let mut tx = client.build_transaction().read_only(true).start()?;
            let rows = tx.query("SELECT n.nspname, c.relname, c.relkind::text, COALESCE(pg_get_viewdef(c.oid, true), '') FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg_toast%' AND c.relkind IN ('r','p','v','m') ORDER BY 1,2", &[])?;
            let mut objects = BTreeMap::new();
            for row in rows {
                let schema: String = row.get(0);
                let name: String = row.get(1);
                let relkind: String = row.get(2);
                let view: String = row.get(3);
                let kind = if relkind == "r" || relkind == "p" {
                    "table"
                } else {
                    "view"
                };
                let qualified = format!("{schema}.{name}");
                let schema_rows = tx.query("SELECT a.attname, pg_catalog.format_type(a.atttypid,a.atttypmod), a.attnotnull, COALESCE(pg_get_expr(d.adbin,d.adrelid),'') FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid JOIN pg_namespace n ON n.oid=c.relnamespace LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum WHERE n.nspname=$1 AND c.relname=$2 AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum", &[&schema, &name])?;
                let mut definition = view;
                for col in schema_rows {
                    let column: String = col.get(0);
                    let datatype: String = col.get(1);
                    let not_null: bool = col.get(2);
                    let default: String = col.get(3);
                    definition
                        .push_str(&format!("{column:?}:{datatype:?}:{not_null}:{default:?};"));
                }
                let count = if kind == "table" {
                    let sql = format!(
                        "SELECT COUNT(*) FROM {}.{}",
                        PostgresEngine::quote_ident(&schema),
                        PostgresEngine::quote_ident(&name)
                    );
                    Some(tx.query_one(&sql, &[])?.get::<_, i64>(0))
                } else {
                    None
                };
                objects.insert((kind.to_string(), qualified), (definition, count));
            }
            tx.rollback()?;
            Ok(objects)
        }
        let a = inspect(self, left)?;
        let b = inspect(self, right)?;
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
            changes.push("no table/view schema or row-count differences".into());
        }
        Ok(format!(
            "{}\n(row contents, indexes, constraints, routines and triggers are not compared)",
            changes.join("\n")
        ))
    }

    fn use_verified_tls(&self) -> bool {
        let loopback = self.pg.host.eq_ignore_ascii_case("localhost")
            || self
                .pg
                .host
                .trim_matches(&['[', ']'][..])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        !loopback || std::env::var("DBSNAP_PG_TLS").as_deref() == Ok("verify-full")
    }

    pub(crate) fn connect(&self, dbname: &str) -> Result<postgres::Client> {
        let tls = self.use_verified_tls();
        let host = if !tls && self.pg.host.eq_ignore_ascii_case("localhost") {
            "127.0.0.1"
        } else {
            &self.pg.host
        };
        let mut config = postgres::Config::new();
        config
            .host(host)
            .port(self.pg.port)
            .user(&self.pg.user)
            .password(self.pg.password.as_deref().unwrap_or(""))
            .dbname(dbname);
        let result = if tls {
            let mut builder = native_tls::TlsConnector::builder();
            if let Some(path) = std::env::var_os("DBSNAP_PG_CA_CERT") {
                let pem = std::fs::read(path).context("reading PostgreSQL CA certificate")?;
                builder.add_root_certificate(
                    native_tls::Certificate::from_pem(&pem)
                        .context("parsing PostgreSQL CA certificate")?,
                );
            }
            let connector = postgres_native_tls::MakeTlsConnector::new(builder.build()?);
            config
                .ssl_mode(postgres::config::SslMode::Require)
                .connect(connector)
        } else {
            config.connect(NoTls)
        };
        result.with_context(|| format!("connecting to {} (as {})", self.pg.display(), self.pg.user))
    }

    fn configure_dump_tls(&self, cmd: &mut Command) {
        if self.use_verified_tls() {
            cmd.env("PGSSLMODE", "verify-full");
            if let Some(path) = std::env::var_os("DBSNAP_PG_CA_CERT") {
                cmd.env("PGSSLROOTCERT", path);
            }
        } else {
            cmd.env("PGSSLMODE", "disable");
        }
        cmd.env_remove("PGHOSTADDR");
    }

    fn client_connstring(&self) -> String {
        let mut target = self.pg.clone();
        if !self.use_verified_tls() && target.host.eq_ignore_ascii_case("localhost") {
            target.host = "127.0.0.1".to_string();
        }
        target.connstring()
    }

    /// Control-plane connection — deliberately never to the target database
    /// (we may have to drop it). Tries `postgres`, then `template1`.
    fn maintenance(&self) -> Result<postgres::Client> {
        let mut last_error = None;
        for db in ["postgres", "template1"] {
            match self.connect(db) {
                Ok(conn) => return Ok(conn),
                Err(error) => last_error = Some(error),
            }
        }
        bail!(
            "cannot connect to a maintenance database (tried postgres, template1) on {}: {:#}",
            self.pg.display(),
            last_error.expect("both connection attempts were made")
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
        store: &Storage,
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

        let recovery_name = format!(
            "{SNAP_PREFIX}previous_{}",
            util::new_id(&store.load_metadata()?.ids())
        );
        if Self::db_exists(&mut admin, &recovery_name)? {
            bail!("refusing to overwrite existing recovery database {recovery_name}");
        }
        self.write_restore_journal(store, &recovery_name)?;
        Self::fence(&mut admin, target)?;
        Self::restore_failpoint("pg_save_after_fence");
        if let Err(e) = admin.execute(
            &format!(
                "CREATE DATABASE {} TEMPLATE {}",
                Self::quote_ident(&snap_db),
                Self::quote_ident(target)
            ),
            &[],
        ) {
            // Guarantee the fence never outlives the operation (§4-T6).
            if Self::unfence(&mut admin, target).is_ok() {
                std::fs::remove_file(Self::journal_path(store)).ok();
            }
            bail!("creating snapshot clone {snap_db} failed: {e:#}");
        }
        Self::unfence(&mut admin, target)?;
        std::fs::remove_file(Self::journal_path(store))?;

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
        let id = util::new_id(taken_ids);
        let out_path = store.snapshots_dir().join(format!("{id}.dump"));
        let staged = store.tmp_dir().join(format!("save-{id}.dump"));
        let mut admin = self.maintenance()?;
        Self::require_matching_client(&mut admin, "pg_dump")?;
        let size = Self::db_size(&mut admin, &self.pg.database)?;
        let _staged_guard = TemporaryFile::create(staged.clone())?;

        self.dump_to(&staged)?;

        let file_size = std::fs::metadata(&staged)?.len();
        let hash = compress::hash_file(&staged)?;
        // Never overwrite an existing artifact, including an orphan left by an
        // interrupted operation. Both guards clean up on a normal copy error.
        let output_guard = TemporaryFile::create(out_path.clone())?;
        {
            let mut input = std::fs::File::open(&staged)?;
            let mut output = std::fs::OpenOptions::new().write(true).open(&out_path)?;
            std::io::copy(&mut input, &mut output)?;
            output.sync_all()?;
        }
        output_guard.persist();
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

        // Never rename the catalogued snapshot itself: restore from a separate
        // managed clone so the same restore point remains available.
        let working_name = format!(
            "{SNAP_PREFIX}restore_{}",
            util::new_id(&store.load_metadata()?.ids())
        );
        admin.execute(
            &format!(
                "CREATE DATABASE {} TEMPLATE {}",
                Self::quote_ident(&working_name),
                Self::quote_ident(snap_db)
            ),
            &[],
        )?;

        let previous_name = format!(
            "{SNAP_PREFIX}previous_{}",
            util::new_id(&store.load_metadata()?.ids())
        );
        if Self::db_exists(&mut admin, &previous_name)? {
            bail!("refusing to overwrite existing recovery database {previous_name}");
        }
        self.write_restore_journal(store, &previous_name)?;

        let mut safety: Option<SnapshotRecord> = None;
        Self::fence(&mut admin, target)?;
        Self::restore_failpoint("pg_clone_after_fence");
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
            Self::restore_failpoint("pg_clone_after_safety_snapshot");
            admin.execute(
                &format!(
                    "ALTER DATABASE {} RENAME TO {}",
                    Self::quote_ident(target),
                    Self::quote_ident(&previous_name)
                ),
                &[],
            )?;
            Self::restore_failpoint("pg_clone_after_previous_rename");
            admin.execute(
                &format!(
                    "ALTER DATABASE {} RENAME TO {}",
                    Self::quote_ident(&working_name),
                    Self::quote_ident(target)
                ),
                &[],
            )?;
            Self::restore_failpoint("pg_clone_after_cutover");
            Ok(())
        })();

        if let Err(e) = outcome {
            drop(admin);
            let recovery = self.recover_interrupted(store);
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
                "restore failed: {e:#}{hint}\nrecovery: {}",
                match recovery {
                    Ok(()) => "original target recovered".to_string(),
                    Err(err) => format!("manual action required: {err:#}"),
                }
            );
        }
        Self::unfence(&mut admin, target)?;
        self.connect(target)
            .context("restore applied but PostgreSQL verification failed")?;
        admin.execute(
            &format!("DROP DATABASE {}", Self::quote_ident(&previous_name)),
            &[],
        )?;
        std::fs::remove_file(Self::journal_path(store))?;
        Ok(RestoreOutcome { safety })
    }

    /// Replay a verified dump into staging, then use the journaled clone
    /// cutover. A replay failure cannot alter the target database.
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
        let target = &self.pg.database;
        let mut admin = self.maintenance()?;
        Self::require_matching_client(&mut admin, "pg_restore")?;
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

        // Restore the archive into a managed staging database first. A replay
        // error leaves the live target untouched.
        let staging = format!(
            "{SNAP_PREFIX}restore_{}",
            util::new_id(&store.load_metadata()?.ids())
        );
        admin.execute(
            &format!(
                "CREATE DATABASE {} TEMPLATE template0 ENCODING {} LC_COLLATE {} LC_CTYPE {} OWNER {}",
                Self::quote_ident(&staging), Self::quote_lit(&encoding),
                Self::quote_lit(&collate), Self::quote_lit(&ctype), Self::quote_ident(&owner)
            ), &[],
        )?;
        let result = (|| -> Result<RestoreOutcome> {
            self.replay_archive(&dump_path, &staging, false)?;
            self.restore_clone(store, record, &staging, safety_tag)
        })();
        if let Err(error) = admin.execute(
            &format!("DROP DATABASE {}", Self::quote_ident(&staging)),
            &[],
        ) {
            eprintln!(
                "warning: {}",
                crate::util::mask(&format!(
                    "could not remove staging database {staging}: {error}"
                ))
            );
        }
        result
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
