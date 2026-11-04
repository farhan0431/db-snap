//! Shared diagnostic/checkpoint operations used by CLI and MCP.
use super::*;
use crate::storage::{compress, Location, TemporaryFile};
use anyhow::Context;
use serde_json::{json, Value};

pub(super) fn check_binding(ctx: &Ctx) -> Result<()> {
    let fingerprint = ctx.db.fingerprint();
    if ctx.store.load_config()? != fingerprint {
        bail!("project binding changed");
    }
    ensure_catalog_binding(&ctx.store, &fingerprint)
}

pub fn verify_at(root: &Path, explicit: Option<&str>, selector: Option<&str>) -> Result<String> {
    if Storage::find(root).is_none() {
        bail!("no dbsnap project; run init first");
    }
    redact((|| {
        verify_with(&context_at(root, explicit, false, false)?, selector)
    })())
}

pub fn verify_with(ctx: &Ctx, selector: Option<&str>) -> Result<String> {
    let _lock = ctx.store.lock()?;
    check_binding(ctx)?;
    let meta = ctx.store.load_metadata()?;
    let records = match selector {
        Some(s) => vec![meta.find(Some(s))?],
        None => meta.snapshots,
    };
    let mut checks = Vec::new();
    let mut failed = false;
    for record in records {
        let check = (|| -> Result<&str> {
            if record.engine != ctx.db.engine.to_string() {
                bail!("snapshot engine differs from binding");
            }
            match &record.location {
                Location::Manifest { path } => {
                    let manifest = ctx.store.read_manifest_file(path)?;
                    crate::storage::cas::verify_manifest(
                        &manifest,
                        &record.blake3,
                        record.original_bytes,
                    )?;
                    let mut total = 0u64;
                    for hash in manifest.hexes() {
                        total = total
                            .checked_add(ctx.store.read_chunk_verified(&hash)?.len() as u64)
                            .context("snapshot size overflow")?;
                    }
                    if total != record.original_bytes {
                        bail!("verified chunks do not match snapshot size");
                    }
                    Ok("chunk_integrity")
                }
                Location::File { .. } => {
                    let source = ctx.store.snapshot_path(&record)?;
                    if compress::hash_file(&source)? != record.blake3 {
                        bail!("snapshot hash mismatch");
                    }
                    if record.method == "pg-dump" {
                        if fs::metadata(source)?.len() != record.size_bytes {
                            bail!("archive size mismatch");
                        }
                        Ok("archive_integrity")
                    } else {
                        let path = ctx.store.tmp_dir().join(format!(
                            "verify-{}.tmp",
                            util::new_id(&meta_ids(&ctx.store)?)
                        ));
                        let _guard = TemporaryFile::create(path.clone())?;
                        compress::decompress_verified(
                            &source,
                            &path,
                            &record.blake3,
                            record.original_bytes,
                        )?;
                        Ok("compressed_integrity")
                    }
                }
                Location::PgDatabase { name } => {
                    if record.engine != "postgres"
                        || ![
                            format!("__dbsnap_{}", record.id),
                            format!("__dbsnap_auto_{}", record.id),
                        ]
                        .contains(name)
                    {
                        bail!("invalid clone name");
                    }
                    let DbTarget::Postgres(target) = &ctx.db.target else {
                        bail!("clone requires PostgreSQL binding");
                    };
                    engine::postgres::PostgresEngine::new(target.clone())
                        .verify_clone_exists(name)?;
                    Ok("existence_only")
                }
            }
        })();
        match check {
            Ok(level) => checks.push(json!({"id":record.id,"tag":record.tag,"ok":true,"verification":level,"restore_tested":false})),
            Err(error) => { failed = true; checks.push(json!({"id":record.id,"tag":record.tag,"ok":false,"error":util::mask(&format!("{error:#}"))})); }
        }
    }
    let report = json!({"ok":!failed,"operation":"verify","snapshots":checks,"note":"Integrity/existence checks do not execute restore or prove SQL replay success."});
    let text = serde_json::to_string_pretty(&report)?;
    if failed {
        bail!("{text}");
    }
    Ok(text)
}

fn meta_ids(store: &Storage) -> Result<HashSet<String>> {
    Ok(store.load_metadata()?.ids())
}

pub fn sandbox_at(
    root: &Path,
    explicit: Option<&str>,
    selector: &str,
    output: Option<&Path>,
    docker_image: Option<&str>,
) -> Result<String> {
    redact((|| {
        let ctx = context_at(root, explicit, false, false)?;
        let _lock = ctx.store.lock()?;
        check_binding(&ctx)?;
        let record = ctx.store.load_metadata()?.find(Some(selector))?;
        if let DbTarget::Mysql(_) = &ctx.db.target {
            if output.is_some() {
                bail!("Docker sandbox does not use --output");
            }
            let image=docker_image.context("MySQL/MariaDB sandbox requires explicit --docker-image mysql:8.0 or mariadb:11; no replay is allowed on the source server")?;
            return engine::mysql::MysqlEngine::sandbox(&ctx.store, &record, image);
        }
        if docker_image.is_some() {
            bail!("--docker-image is only supported for MySQL/MariaDB snapshots");
        }
        if let DbTarget::Postgres(target) = &ctx.db.target {
            if output.is_some() {
                bail!("PostgreSQL sandbox is server-side; omit --output");
            }
            let registry = ctx.store.root.join("sandboxes.json");
            let mut entries: Vec<Value> = if registry.exists() {
                serde_json::from_slice(&fs::read(&registry)?)?
            } else {
                vec![]
            };
            let engine = engine::postgres::PostgresEngine::new(target.clone());
            let name = engine.create_sandbox(&ctx.store, &record, &meta_ids(&ctx.store)?)?;
            entries.push(json!({"engine":"postgres","name":name,"snapshot_id":record.id,"target":target.display()}));
            crate::storage::write_atomic(&registry, &serde_json::to_vec_pretty(&entries)?)
                .with_context(|| format!("sandbox {name} created but registry save failed; retain its name for manual cleanup"))?;
            return Ok(serde_json::to_string_pretty(
                &json!({"operation":"sandbox","engine":"postgres","database":name,"snapshot_id":record.id,"source_modified":false,"cleanup":"close sandbox clients then explicitly DROP this managed database; registry: sandboxes.json"}),
            )?);
        }
        if !matches!(ctx.db.target, DbTarget::Sqlite(_)) {
            bail!("MySQL sandbox requires a separate isolated server; schema-qualified SQL cannot safely be redirected on the live server");
        }
        if record.engine != "sqlite" {
            bail!("sandbox requires a SQLite snapshot");
        }
        let output = output.context("SQLite sandbox requires --output NEW_FILE")?;
        let destination = if output.is_absolute() {
            output.to_path_buf()
        } else {
            root.join(output)
        };
        let mut entries = sandbox_registry(&ctx.store)?;
        let guard = TemporaryFile::create(destination.clone())?;
        match &record.location {
            Location::Manifest { path } => {
                let manifest = ctx.store.read_manifest_file(path)?;
                crate::storage::cas::verify_manifest(
                    &manifest,
                    &record.blake3,
                    record.original_bytes,
                )?;
                use std::io::Write;
                let mut file = fs::OpenOptions::new().write(true).open(&destination)?;
                for hash in manifest.hexes() {
                    file.write_all(&ctx.store.read_chunk_verified(&hash)?)?;
                }
                file.sync_all()?;
            }
            Location::File { .. } => {
                compress::decompress_verified(
                    &ctx.store.snapshot_path(&record)?,
                    &destination,
                    &record.blake3,
                    record.original_bytes,
                )?;
            }
            _ => bail!("sandbox requires a SQLite file or manifest"),
        }
        let connection = rusqlite::Connection::open_with_flags(
            &destination,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let integrity: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            bail!("sandbox integrity check failed");
        }
        drop(connection);
        let registered_path = destination.canonicalize()?;
        guard.persist();
        entries.push(json!({"engine":"sqlite","path":registered_path,"snapshot_id":record.id}));
        crate::storage::write_atomic(
            &ctx.store.root.join("sandboxes.json"),
            &serde_json::to_vec_pretty(&entries)?,
        )
        .with_context(|| {
            format!(
                "sandbox created at {} but registry save failed",
                destination.display()
            )
        })?;
        Ok(serde_json::to_string_pretty(
            &json!({"operation":"sandbox","snapshot_id":record.id,"database":destination,"source_modified":false,"cleanup":"close sandbox connections, then explicitly delete this file"}),
        )?)
    })())
}

fn sandbox_registry(store: &Storage) -> Result<Vec<Value>> {
    match fs::read(store.root.join("sandboxes.json")) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).context("reading sandbox registry")?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

pub fn sandbox_list_at(root: &Path, explicit: Option<&str>) -> Result<String> {
    redact((|| {
        if Storage::find(root).is_none() {
            bail!("no dbsnap project; run init first");
        }
        let ctx = context_at(root, explicit, false, false)?;
        let _lock = ctx.store.lock()?;
        check_binding(&ctx)?;
        Ok(serde_json::to_string_pretty(&json!({
            "operation": "sandbox_list", "sandboxes": sandbox_inventory(&ctx)?
        }))?)
    })())
}

pub fn sandbox_remove_at(root: &Path, explicit: Option<&str>, id: &str) -> Result<String> {
    redact((|| {
        if Storage::find(root).is_none() {
            bail!("no dbsnap project; run init first");
        }
        let ctx = context_at(root, explicit, false, false)?;
        let _lock = ctx.store.lock()?;
        check_binding(&ctx)?;
        let mut entries = sandbox_registry(&ctx.store)?;
        let matches: Vec<_> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry["engine"] == "mysql" && entry["container_id"].as_str() == Some(id)
            })
            .map(|(index, _)| index)
            .collect();
        let [index] = matches.as_slice() else {
            bail!("expected one registered Docker sandbox with the exact full container ID");
        };
        engine::mysql_sandbox::remove_owned(&ctx.store, id)?;
        entries.remove(*index);
        crate::storage::write_atomic(
            &ctx.store.root.join("sandboxes.json"),
            &serde_json::to_vec_pretty(&entries)?,
        )
        .context("container removed but sandbox registry update failed")?;
        Ok(serde_json::to_string_pretty(&json!({
            "operation": "sandbox_remove", "container_id": id, "removed": true
        }))?)
    })())
}

fn sandbox_inventory(ctx: &Ctx) -> Result<Vec<Value>> {
    let entries = sandbox_registry(&ctx.store)?;
    let docker_ids = if entries.iter().any(|entry| entry["engine"] == "mysql") {
        std::process::Command::new("docker")
            .args([
                "ps",
                "--all",
                "--no-trunc",
                "--filter",
                "label=dbsnap.sandbox=true",
                "--format",
                "{{.ID}}",
            ])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| String::from_utf8(out.stdout).ok())
    } else {
        None
    };
    let state = |exists: Option<bool>| match exists {
        Some(true) => "present",
        Some(false) => "missing",
        None => "unknown",
    };
    let mut inventory = Vec::with_capacity(entries.len());
    for entry in entries {
        let snapshot = entry["snapshot_id"].as_str();
        let item = match entry["engine"].as_str() {
            Some("sqlite") => {
                let path = entry["path"]
                    .as_str()
                    .filter(|p| Path::new(p).is_absolute());
                let exists = path.and_then(|path| match fs::symlink_metadata(path) {
                    Ok(info) if info.file_type().is_file() => Some(true),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
                    _ => None,
                });
                json!({"engine":"sqlite","path":path,"snapshot_id":snapshot,"state":state(exists)})
            }
            Some("mysql") => {
                let id = entry["container_id"]
                    .as_str()
                    .filter(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()));
                let exists = id
                    .zip(docker_ids.as_deref())
                    .map(|(id, ids)| ids.lines().any(|line| line == id));
                json!({"engine":"mysql","container_id":id,"snapshot_id":snapshot,"state":state(exists)})
            }
            // Legacy PostgreSQL registry entries do not have an engine field.
            Some("postgres") | None if entry.get("name").is_some() => {
                let name = entry["name"].as_str().filter(|name| {
                    name.strip_prefix("__dbsnap_sandbox_")
                        .is_some_and(|suffix| {
                            suffix.len() == 6 && suffix.bytes().all(|b| b.is_ascii_hexdigit())
                        })
                });
                let exists = match (&ctx.db.target, name) {
                    (DbTarget::Postgres(target), Some(name))
                        if entry["target"].as_str() == Some(target.display().as_str()) =>
                    {
                        engine::postgres::PostgresEngine::new(target.clone())
                            .managed_exists(name)
                            .ok()
                    }
                    _ => None,
                };
                json!({"engine":"postgres","database":name,"snapshot_id":snapshot,"state":state(exists)})
            }
            _ => json!({"snapshot_id":snapshot,"state":"unknown"}),
        };
        inventory.push(item);
    }
    Ok(inventory)
}

pub fn doctor_at(root: &Path, explicit: Option<&str>) -> Result<String> {
    if Storage::find(root).is_none() {
        bail!("no dbsnap project; run init first");
    }
    redact((|| doctor_with(&context_at(root, explicit, false, false)?))())
}

pub fn doctor_with(ctx: &Ctx) -> Result<String> {
    let _lock = ctx.store.lock()?;
    check_binding(ctx)?;
    let meta = ctx.store.load_metadata()?;
    let probe = engine::make(&ctx.db)?.probe();
    let mut journals = Vec::new();
    for file in ["mysql-restore.json", "postgres-restore.json"] {
        let path = ctx.store.root.join(file);
        if path.exists() {
            let parsed = fs::read(&path)
                .map_err(anyhow::Error::from)
                .and_then(|v| Ok(serde_json::from_slice::<Value>(&v)?));
            let recovery_id = if file == "mysql-restore.json" {
                parsed
                    .as_ref()
                    .ok()
                    .and_then(Value::as_str)
                    .filter(|id| {
                        !id.is_empty()
                            && id.len() <= 64
                            && id
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    })
                    .map(str::to_owned)
            } else {
                None
            };
            journals.push(json!({"file":file,"valid_json":parsed.is_ok(),"recovery_id":recovery_id,"action":"run dbsnap recover after resolving storage/connection problems; preserve journal"}));
        }
    }
    let recorded: HashSet<String> = meta
        .snapshots
        .iter()
        .chain(&meta.pending_gc)
        .filter_map(|r| r.artifact_rel().map(str::to_owned))
        .collect();
    let mut untracked = Vec::new();
    for directory in ["snapshots", "manifests", "tmp"] {
        for entry in fs::read_dir(ctx.store.root.join(directory))? {
            let entry = entry?;
            let rel = format!("{directory}/{}", entry.file_name().to_string_lossy());
            if !recorded.contains(&rel) {
                untracked.push(rel);
            }
        }
    }
    untracked.sort();
    let (sandboxes, sandbox_registry_error) = match sandbox_inventory(ctx) {
        Ok(entries) => (entries, None),
        Err(error) => (Vec::new(), Some(util::mask(&format!("{error:#}")))),
    };
    let connection = match probe {
        Ok(info) => json!({"ok":true,"detail":util::mask(&info.detail)}),
        Err(error) => json!({"ok":false,"error":util::mask(&format!("{error:#}"))}),
    };
    Ok(serde_json::to_string_pretty(
        &json!({"operation":"doctor","connection":connection,
        "engine":ctx.db.engine.to_string(),"snapshot_count":meta.snapshots.len(),"pending_gc_count":meta.pending_gc.len(),"free_bytes":ctx.store.free_bytes().ok(),
        "pending_recovery":journals,"untracked_files":untracked,"sandboxes":sandboxes,
        "sandbox_registry_error":sandbox_registry_error,"mutations_performed":false,
        "note":"Untracked files are reported, never deleted. CAS garbage and server-side orphans are not scanned. Run verify to check snapshot contents."}),
    )?)
}

pub fn recover_at(root: &Path, explicit: Option<&str>) -> Result<String> {
    redact((|| {
        recover_with(&context_at(root, explicit, false, false)?)
    })())
}

pub fn recover_with(ctx: &Ctx) -> Result<String> {
    check_binding(ctx)?;
    match &ctx.db.target {
        DbTarget::Mysql(_) => {
            let id: String = serde_json::from_slice(&fs::read(ctx.store.root.join("mysql-restore.json")).context("no readable MySQL recovery journal; do not guess a recovery ID")?)?;
            restore_with(ctx, Some(id), true)
        }
        DbTarget::Postgres(target) => {
            let _lock = ctx.store.lock()?;
            engine::postgres::PostgresEngine::new(target.clone()).recover_interrupted(&ctx.store)?;
            Ok("PostgreSQL recovery checked; ambiguous states still require manual inspection".into())
        }
        DbTarget::Sqlite(_) => bail!("SQLite has no server recovery journal; inspect doctor/verify and select an explicit snapshot to restore"),
    }
}

pub fn exec_at(root: &Path, explicit: Option<&str>, command: &[String]) -> Result<String> {
    redact((|| {
        let (program, args) = command
            .split_first()
            .context("exec requires a command after --")?;
        let ctx = context_at(root, explicit, false, false)?;
        check_binding(&ctx)?;
        if ctx.store.root.join("mysql-restore.json").exists()
            || ctx.store.root.join("postgres-restore.json").exists()
        {
            bail!("resolve pending recovery before exec");
        }
        let tag = format!("exec-{}", util::new_id(&ctx.store.load_metadata()?.ids()));
        let saved: Value = serde_json::from_str(&save_json_with(
            &ctx,
            Some(tag.clone()),
            engine::SaveMethod::Clone,
        )?)?;
        let id = saved["snapshot_id"]
            .as_str()
            .context("checkpoint ID missing")?;
        let snapshot = ctx.store.load_metadata()?.find(Some(id))?;
        // Child output may contain credentials. Keep raw spool files outside
        // the project/catalog; only masked output is returned to the caller.
        let spool = format!("dbsnap-exec-{}-{}", std::process::id(), snapshot.id);
        let stdout_path = std::env::temp_dir().join(format!("{spool}-stdout"));
        let stderr_path = std::env::temp_dir().join(format!("{spool}-stderr"));
        let _stdout = TemporaryFile::create(stdout_path.clone())?;
        let _stderr = TemporaryFile::create(stderr_path.clone())?;
        let status = std::process::Command::new(program)
            .args(args)
            .current_dir(root)
            .stdout(fs::OpenOptions::new().write(true).open(&stdout_path)?)
            .stderr(fs::OpenOptions::new().write(true).open(&stderr_path)?)
            .status()
            .with_context(|| format!("starting command; checkpoint {} retained", snapshot.id))?;
        fn read_output(path: &Path) -> Result<(String, bool)> {
            use std::io::Read;
            let mut bytes = Vec::new();
            fs::File::open(path)?.take(65536).read_to_end(&mut bytes)?;
            Ok((
                util::mask(&String::from_utf8_lossy(&bytes)),
                fs::metadata(path)?.len() > 65536,
            ))
        }
        let (stdout, stdout_truncated) = read_output(&stdout_path)?;
        let (stderr, stderr_truncated) = read_output(&stderr_path)?;
        let report = json!({"operation":"exec","snapshot_id":snapshot.id,"tag":tag,"exit_code":status.code(),
            "success":status.success(),"stdout":stdout,"stderr":stderr,
            "stdout_truncated":stdout_truncated,"stderr_truncated":stderr_truncated,"automatic_restore":false,
            "recovery_command":format!("dbsnap restore {}", snapshot.id)});
        let text = serde_json::to_string_pretty(&report)?;
        if !status.success() {
            bail!("{text}");
        }
        Ok(text)
    })())
}

pub fn diff_live_at(root: &Path, explicit: Option<&str>, selector: &str) -> Result<String> {
    redact((|| {
        diff_live_with(&context_at(root, explicit, false, false)?, selector)
    })())
}

pub fn diff_live_with(ctx: &Ctx, selector: &str) -> Result<String> {
    check_binding(ctx)?;
    if ctx.store.root.join("mysql-restore.json").exists()
        || ctx.store.root.join("postgres-restore.json").exists()
    {
        bail!("resolve pending recovery before capturing live state");
    }
    let from = ctx.store.load_metadata()?.find(Some(selector))?;
    let tag = format!(
        "compare-live-{}",
        util::new_id(&ctx.store.load_metadata()?.ids())
    );
    let saved: Value =
        serde_json::from_str(&save_json_with(ctx, Some(tag), engine::SaveMethod::Clone)?)?;
    let id = saved["snapshot_id"]
        .as_str()
        .context("live checkpoint ID missing")?;
    let comparison = diff_with(ctx, &from.id, id)
        .with_context(|| format!("live checkpoint {id} retained even though comparison failed"))?;
    Ok(serde_json::to_string_pretty(
        &json!({"operation":"diff_live","from":from.id,"live_snapshot_id":id,"live_snapshot_retained":true,"comparison":comparison,"note":"Captures and retains a regular snapshot; PostgreSQL clone creation briefly fences target connections."}),
    )?)
}
