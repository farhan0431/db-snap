//! Explicit, trusted migration commands against retained sandbox databases.
use super::*;
use anyhow::Context;
use serde_json::{json, Value};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct RehearseOptions {
    pub command: Vec<String>,
    pub timeout_seconds: u64,
    pub fail_on_drop_table: bool,
}

impl RehearseOptions {
    pub fn validate(&self) -> Result<()> {
        if self.command.is_empty()
            || self.command.len() > 128
            || self.command[0].is_empty()
            || self
                .command
                .iter()
                .any(|arg| arg.len() > 8192 || arg.contains('\0'))
            || self.command.iter().map(String::len).sum::<usize>() > 8192
            || !(1..=600).contains(&self.timeout_seconds)
        {
            bail!("rehearse requires a command (at most 128 arguments, 8192 bytes total) and a timeout between 1 and 600 seconds");
        }
        Ok(())
    }
}

pub fn rehearse_at(
    root: &Path,
    explicit: Option<&str>,
    selector: &str,
    options: &RehearseOptions,
) -> Result<String> {
    redact((|| {
        rehearse_with(
            &context_at(root, explicit, false, false)?,
            root,
            selector,
            options,
        )
    })())
}

pub fn rehearse_with(
    ctx: &Ctx,
    cwd: &Path,
    selector: &str,
    options: &RehearseOptions,
) -> Result<String> {
    options.validate()?;
    if matches!(ctx.db.target, DbTarget::Mysql(_)) {
        bail!("rehearse supports SQLite and PostgreSQL; MySQL/MariaDB needs a separate Docker migrator connection design");
    }
    let (source, workspace) = {
        let _lock = ctx.store.lock()?;
        health::check_binding(ctx)?;
        ensure_no_restore_journal(ctx)?;
        let meta = ctx.store.load_metadata()?;
        let source = meta.find(Some(selector))?;
        let parent = ctx.store.root.join("rehearsals");
        fs::create_dir_all(&parent)?;
        if !fs::symlink_metadata(&parent)?.file_type().is_dir() {
            bail!("rehearsal directory must be a real directory");
        }
        // Atomic directory creation refuses collisions and pre-existing entries.
        let workspace = parent.join(format!(
            "{}-{}",
            std::process::id(),
            util::new_id(&meta.ids())
        ));
        fs::create_dir(&workspace)?;
        (source, workspace)
    };
    let report_path = workspace.join("report.json");
    let mut report = json!({
        "version": 1, "operation": "rehearse", "status": "incomplete",
        "source_snapshot_id": source.id, "engine": ctx.db.engine.to_string(),
        "sandbox_retained": false, "sandbox": Value::Null,
        "report_path": report_path,
        "command_exit_code": Value::Null, "timed_out": false,
        "policy": { "fail_on_drop_table": options.fail_on_drop_table, "violated": Value::Null },
        "cleanup": "Retained for inspection. Close sandbox clients before manual cleanup.",
        "execution": "Trusted command with a sandbox connection in its environment. No OS isolation."
    });
    persist_report(&report_path, &report)?;
    let result = run_rehearsal(ctx, cwd, &source, &workspace, options, &mut report);
    if let Err(error) = result {
        report["status"] = json!("infrastructure_failed");
        report["error"] = json!(util::mask(&format!("{error:#}"))
            .chars()
            .take(2048)
            .collect::<String>());
    }
    persist_report(&report_path, &report)?;
    Ok(serde_json::to_string_pretty(&report)?)
}

fn persist_report(path: &Path, report: &Value) -> Result<()> {
    let bytes = util::mask(&serde_json::to_string_pretty(report)?);
    crate::storage::write_atomic(path, bytes.as_bytes())
}

fn run_rehearsal(
    ctx: &Ctx,
    cwd: &Path,
    source: &SnapshotRecord,
    workspace: &Path,
    options: &RehearseOptions,
    report: &mut Value,
) -> Result<()> {
    let project = ctx
        .store
        .root
        .parent()
        .context("project directory missing")?;
    let sqlite_file = workspace.join("sandbox.db");
    // sandbox_at owns source locking, integrity verification and registration.
    // Read the exact owned resource from the registry, never from masked output.
    let sandbox_db = match &ctx.db.target {
        DbTarget::Sqlite(_) => {
            report["sandbox"] = json!(sqlite_file);
            persist_report(&workspace.join("report.json"), report)?;
            let explicit = match &ctx.db.target {
                DbTarget::Sqlite(path) => path.to_str().context("database path is not UTF-8")?,
                _ => unreachable!(),
            };
            health::sandbox_at(
                project,
                Some(explicit),
                &source.id,
                Some(&sqlite_file),
                None,
            )?;
            config::parse_db_string(
                sqlite_file.to_str().context("sandbox path is not UTF-8")?,
                false,
            )?
        }
        DbTarget::Postgres(target) => {
            let _lock = ctx.store.lock()?;
            health::check_binding(ctx)?;
            ensure_no_restore_journal(ctx)?;
            let meta = ctx.store.load_metadata()?;
            let record = meta.find(Some(&source.id))?;
            let engine = engine::postgres::PostgresEngine::new(target.clone());
            let name = engine.create_sandbox(&ctx.store, &record, &meta.ids())?;
            let mut sandbox = target.clone();
            sandbox.database = name;
            report["sandbox"] = json!(sandbox.display());
            report["sandbox_retained"] = json!(true);
            // Persist a resource hint before attempting any further server work.
            persist_report(&workspace.join("report.json"), report)?;
            let mut entries = health::sandbox_registry(&ctx.store)?;
            entries.push(json!({"engine": "postgres", "name": sandbox.database, "snapshot_id": source.id, "target": target.display(), "rehearsal": workspace}));
            crate::storage::write_atomic(
                &ctx.store.root.join("sandboxes.json"),
                &serde_json::to_vec_pretty(&entries)?,
            )?;
            ResolvedDb {
                engine: config::Engine::Postgres,
                target: DbTarget::Postgres(sandbox),
            }
        }
        DbTarget::Mysql(_) => unreachable!(),
    };
    report["sandbox"] = json!(sandbox_db.display());
    report["sandbox_retained"] = json!(true);
    let sandbox = Ctx {
        store: Storage::create(workspace, &sandbox_db.fingerprint())?,
        db: sandbox_db,
    };
    // Retain before/after artifacts inside the rehearsal catalog.
    save_with(&sandbox, Some("before".into()), engine::SaveMethod::Clone)?;
    let before = sandbox.store.load_metadata()?.find(Some("before"))?;
    report["before_snapshot_id"] = json!(before.id);
    persist_report(&workspace.join("report.json"), report)?;
    let mut command = Command::new(&options.command[0]);
    command
        .args(&options.command[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_connection(&mut command, &sandbox.db)?;
    // Commands receive no shell expansion. A script can read these variables.
    let (exit_code, timed_out, success) = run_command(command, options.timeout_seconds)?;
    report["command_exit_code"] = json!(exit_code);
    report["timed_out"] = json!(timed_out);
    report["status"] = json!(if timed_out {
        "timed_out"
    } else if success {
        "incomplete"
    } else {
        "command_failed"
    });
    persist_report(&workspace.join("report.json"), report)?;
    save_with(&sandbox, Some("after".into()), engine::SaveMethod::Clone)?;
    let after = sandbox.store.load_metadata()?.find(Some("after"))?;
    report["after_snapshot_id"] = json!(after.id);
    let _lock = sandbox.store.lock()?;
    let diff = schema_diff_records(&sandbox, &before, &after)?;
    let dropped = diff.removed_tables.len();
    let violated = options.fail_on_drop_table && dropped > 0;
    report["policy"]["violated"] = json!(violated);
    report["removed_table_count"] = json!(dropped);
    report["removed_tables"] = json!(diff
        .removed_tables
        .iter()
        .take(50)
        .map(|s| s.chars().take(256).collect::<String>())
        .collect::<Vec<_>>());
    report["removed_tables_truncated"] = json!(
        dropped > 50
            || diff
                .removed_tables
                .iter()
                .any(|name| name.chars().count() > 256)
    );
    report["summary"] = json!(diff.text.chars().take(8192).collect::<String>());
    report["summary_truncated"] = json!(diff.text.chars().count() > 8192);
    if success && !timed_out {
        report["status"] = json!(if violated { "policy_failed" } else { "passed" });
    }
    Ok(())
}

fn configure_connection(command: &mut Command, db: &ResolvedDb) -> Result<()> {
    // Override common libpq source selectors in inherited environments.
    for name in [
        "PGSERVICE",
        "PGSERVICEFILE",
        "PGHOSTADDR",
        "PGOPTIONS",
        "DATABASE_URL",
        "DIRECT_URL",
        "SHADOW_DATABASE_URL",
    ] {
        command.env_remove(name);
    }
    let connection = match &db.target {
        DbTarget::Sqlite(path) => {
            command.env("DBSNAP_SANDBOX_DB", path);
            format!(
                "file:{}",
                path.to_str()
                    .context("sandbox path is not UTF-8")?
                    .replace('\\', "/")
            )
        }
        DbTarget::Postgres(target) => {
            command
                .env("PGHOST", &target.host)
                .env("PGPORT", target.port.to_string())
                .env("PGUSER", &target.user)
                .env("PGDATABASE", &target.database)
                .env("PGPASSWORD", target.password.as_deref().unwrap_or(""));
            let mut url = url::Url::parse("postgresql://localhost/")?;
            url.set_host(Some(&target.host))
                .map_err(|_| anyhow::anyhow!("invalid sandbox host"))?;
            url.set_port(Some(target.port))
                .map_err(|_| anyhow::anyhow!("invalid sandbox port"))?;
            url.set_username(&target.user)
                .map_err(|_| anyhow::anyhow!("invalid sandbox user"))?;
            url.set_password(target.password.as_deref())
                .map_err(|_| anyhow::anyhow!("invalid sandbox password"))?;
            url.set_path(&target.database);
            command.env("DBSNAP_SANDBOX_DB", &target.database);
            url.to_string()
        }
        DbTarget::Mysql(_) => unreachable!(),
    };
    command
        .env("DATABASE_URL", &connection)
        .env("DIRECT_URL", &connection);
    Ok(())
}

fn run_command(mut command: Command, timeout: u64) -> Result<(Option<i32>, bool, bool)> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .context("starting trusted migration command")?;
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status.code(), false, status.success()));
        }
        if Instant::now() >= deadline {
            #[cfg(unix)]
            let _ = Command::new("kill")
                .args(["-KILL", "--", &format!("-{}", child.id())])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            #[cfg(windows)]
            let _ = Command::new("taskkill")
                .args(["/PID", &child.id().to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if let Err(error) = child.kill() {
                if child.try_wait()?.is_none() {
                    return Err(error).context(
                        "could not terminate timed-out command; inspect retained sandbox",
                    );
                }
            }
            let status = child.wait()?;
            return Ok((status.code(), true, false));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub fn rehearse_exit_code(report: &Value) -> i32 {
    match report["status"].as_str() {
        Some("passed") => 0,
        Some("policy_failed") => 3,
        Some("command_failed") => 4,
        Some("timed_out") => 5,
        _ => 1,
    }
}
