use crate::config::{self, DbTarget, ResolvedDb, ServerTarget};
use crate::engine;
use crate::storage::{Metadata, SnapshotRecord, Storage};
use crate::util;
use anyhow::{bail, Result};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

#[path = "health.rs"]
mod health;
#[path = "rows.rs"]
mod rows;
pub use health::{
    diff_live_at, diff_live_with, doctor_at, doctor_with, exec_at, recover_at, recover_with,
    sandbox_at, verify_at, verify_with,
};
pub use rows::{diff_rows_at, diff_rows_with};

/// Agent-facing skill installed into the project on `dbsnap init` — teaches
/// AI agents when and how to snapshot/restore.
const AGENT_SKILL_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/skill/dbsnap/SKILL.md"
));

/// Everything a command needs: the resolved database and its `.dbsnap/`.
pub struct Ctx {
    pub db: ResolvedDb,
    pub store: Storage,
}

const AUTO_SAFETY_KEEP: usize = 3;

/// Find or create `.dbsnap/` for `root`, resolving the database (explicit
/// flag → DATABASE_URL/.env → auto-detect). `force` rebinds when the target
/// differs from the stored fingerprint (§4-T2); `allow_remote` lifts the
/// local-host guard for this command (§4-T1) — MCP always passes false.
pub fn context_at(
    root: &Path,
    explicit: Option<&str>,
    force: bool,
    allow_remote: bool,
) -> Result<Ctx> {
    let existing = Storage::find(root);
    let project_root = existing
        .as_ref()
        .and_then(|s| s.root.parent())
        .unwrap_or(root)
        .to_path_buf();
    let db = config::resolve_at(&project_root, explicit, allow_remote)?;
    let fingerprint = db.fingerprint();
    if let Some(store) = existing {
        ensure_catalog_binding(&store, &fingerprint)?;
        let bound = store.load_config()?;
        if bound != fingerprint {
            if !force {
                bail!(
                    "this project is bound to {} ({}), but the target database is {} ({})\ncheck the target; to re-bind, run `dbsnap init` with the intended database. `save` and `restore` also support --force",
                    bound.engine,
                    bound.target,
                    fingerprint.engine,
                    fingerprint.target
                );
            }
            let _lock = store.lock()?;
            ensure_catalog_binding(&store, &fingerprint)?;
            let store = Storage::create(&project_root, &fingerprint)?;
            return Ok(Ctx { db, store });
        }
        Ok(Ctx { db, store })
    } else {
        let store = Storage::create(root, &fingerprint)?;
        Ok(Ctx { db, store })
    }
}

fn ensure_catalog_binding(store: &Storage, fingerprint: &config::Fingerprint) -> Result<()> {
    let meta = store.load_metadata()?;
    if store.root.join("metadata.json").exists() && meta.project_fingerprint != *fingerprint {
        bail!("the snapshot catalog belongs to a different database; refusing to rebind or use its snapshots. Use a separate project directory for the new database");
    }
    Ok(())
}

fn ensure_active_binding(ctx: &Ctx) -> Result<()> {
    let fingerprint = ctx.db.fingerprint();
    if ctx.store.load_config()? != fingerprint {
        bail!("the project binding changed; restart the operation and verify the database target");
    }
    ensure_catalog_binding(&ctx.store, &fingerprint)?;
    if let DbTarget::Postgres(target) = &ctx.db.target {
        engine::postgres::PostgresEngine::new(target.clone()).recover_interrupted(&ctx.store)?;
    }
    Ok(())
}

/// Check the MCP target before creating or changing any project files.
pub fn mcp_context_at(root: &Path, explicit: Option<&str>) -> Result<Ctx> {
    let existing = Storage::find(root);
    let project_root = existing
        .as_ref()
        .and_then(|s| s.root.parent())
        .unwrap_or(root);
    let db = config::resolve_at(project_root, explicit, false)?;
    config::validate_mcp_target(&db)?;
    context_at(root, explicit, false, false)
}

/// Final redaction layer for every public command: passwords parsed from
/// URLs are registered globally (config::server_from) and scrubbed from
/// both success text and error chains before they can reach a human or an
/// AI agent (§4-T11).
fn redact(result: Result<String>) -> Result<String> {
    result
        .map(|s| util::mask(&s))
        .map_err(|e| anyhow::anyhow!(util::mask(&format!("{e:#}"))))
}

pub fn init_at(root: &Path, explicit: Option<&str>, allow_remote: bool) -> Result<String> {
    redact((|| {
        let existing = Storage::find(root);
        let project_root = existing
            .as_ref()
            .and_then(|s| s.root.parent())
            .unwrap_or(root);
        let db = config::resolve_at(project_root, explicit, allow_remote)?;
        let fingerprint = db.fingerprint();
        let _lock = existing.as_ref().map(Storage::lock).transpose()?;
        if let Some(store) = &existing {
            ensure_catalog_binding(store, &fingerprint)?;
        }
        let existed = existing.is_some();
        let store = Storage::create(project_root, &fingerprint)?;

        let mut msg = if existed {
            "re-bound project database".to_string()
        } else {
            "initialized dbsnap".to_string()
        };
        msg.push_str(&format!(
            "\n  database : {} ({})\n  storage  : {}",
            db.engine,
            db.display(),
            store.root.display()
        ));

        let engine = engine::make(&db)?;
        match engine.probe() {
            Ok(info) => {
                let size = info
                    .size_bytes
                    .map(|s| format!(", {}", util::format_size(s)))
                    .unwrap_or_default();
                msg.push_str(&format!("\n  engine   : {}{size}", info.detail));
            }
            Err(_) => {
                msg.push_str(
                "\n  warning  : database unreachable — check the server / file path before saving",
            );
            }
        }

        msg.push_str("\n  git      : .dbsnap/ registered in .gitignore");

        // Install the agent skill so AI editors pick up the workflow automatically.
        let skill_path = project_root
            .join(".agents")
            .join("skills")
            .join("dbsnap")
            .join("SKILL.md");
        if !skill_path.exists() {
            if let Some(parent) = skill_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&skill_path, AGENT_SKILL_MD)?;
            msg.push_str(&format!(
                "\n  agent    : skill installed at {}",
                skill_path.display()
            ));
        }
        msg.push_str("\nnext: dbsnap save \"initial-seed\"");
        Ok(msg)
    })())
}

pub fn save_at(
    root: &Path,
    explicit: Option<&str>,
    tag: Option<String>,
    force: bool,
    allow_remote: bool,
    method: engine::SaveMethod,
) -> Result<String> {
    redact((|| {
        let ctx = context_at(root, explicit, force, allow_remote)?;
        save_with(&ctx, tag, method)
    })())
}

pub fn save_with(ctx: &Ctx, tag: Option<String>, method: engine::SaveMethod) -> Result<String> {
    save_report_with(ctx, tag, method, false)
}

pub fn save_json_with(
    ctx: &Ctx,
    tag: Option<String>,
    method: engine::SaveMethod,
) -> Result<String> {
    save_report_with(ctx, tag, method, true)
}

fn save_report_with(
    ctx: &Ctx,
    tag: Option<String>,
    method: engine::SaveMethod,
    json: bool,
) -> Result<String> {
    let _lock = ctx.store.lock()?;
    ensure_active_binding(ctx)?;
    let tag = match tag {
        Some(t) => {
            util::validate_tag(&t)?;
            t
        }
        None => util::default_tag(),
    };
    let mut meta = ctx.store.load_metadata()?;
    let taken = meta.ids();
    let previous = meta.latest();
    let engine = engine::make(&ctx.db)?;
    let outcome = engine.save(
        &ctx.store,
        &tag,
        &engine::SaveOpts { method },
        previous.as_ref(),
        &taken,
    )?;
    meta.snapshots.push(outcome.record.clone());
    ctx.store.write_metadata(&meta)?;

    let r = &outcome.record;
    if json {
        return Ok(serde_json::to_string(
            &serde_json::json!({"operation":"save","snapshot_id":r.id,"tag":r.tag,"method":r.method,"linked":outcome.linked}),
        )?);
    }
    let detail = match r.method.as_str() {
        "pg-clone" => format!(
            "server-side clone, {} in database",
            util::format_size(r.original_bytes)
        ),
        "pg-dump" | "mysql-dump" => format!(
            "{} dump ({} original)",
            util::format_size(r.size_bytes),
            util::format_size(r.original_bytes)
        ),
        _ => format!(
            "{} compressed ({} original)",
            util::format_size(r.size_bytes),
            util::format_size(r.original_bytes)
        ),
    };
    let mut msg = format!("saved snapshot {} \"{}\" — {detail}", r.id, r.tag);
    if outcome.linked {
        msg.push_str("\nidentical to the previous snapshot — linked, no extra storage used");
    }
    Ok(msg)
}

pub fn restore_at(
    root: &Path,
    explicit: Option<&str>,
    id_or_tag: Option<String>,
    safety: bool,
    force: bool,
    allow_remote: bool,
) -> Result<String> {
    redact((|| {
        let ctx = context_at(root, explicit, force, allow_remote)?;
        restore_with(&ctx, id_or_tag, safety)
    })())
}

pub fn restore_with(ctx: &Ctx, id_or_tag: Option<String>, safety: bool) -> Result<String> {
    let _lock = ctx.store.lock()?;
    ensure_active_binding(ctx)?;
    let meta = ctx.store.load_metadata()?;
    let record = meta.find(id_or_tag.as_deref())?;
    if record.engine != ctx.db.engine.to_string() {
        bail!(
            "snapshot {} is a {} snapshot but the bound database is {} — engine mismatch",
            record.id,
            record.engine,
            ctx.db.engine
        );
    }
    let engine = engine::make(&ctx.db)?;

    // The engine verifies integrity first (§4-T3) and creates + persists the
    // safety snapshot (§4-T7) before any destructive step.
    let outcome = engine.restore(
        &ctx.store,
        &record,
        safety.then(util::safety_tag).as_deref(),
    )?;

    // Retention for auto safety snapshots (§4-T7).
    let mut meta = ctx.store.load_metadata()?;
    prune_auto_snapshots(&mut meta, &ctx.store, pg_target(&ctx.db))?;
    ctx.store.write_metadata(&meta)?;

    let mut msg = format!(
        "restored database to snapshot {} \"{}\"",
        record.id, record.tag
    );
    if let Some(r) = &outcome.safety {
        msg.push_str(&format!(
            "\nprevious state kept as safety snapshot {} (rollback: dbsnap restore {})",
            r.id, r.id
        ));
    }
    Ok(msg)
}

/// Resolve the exact snapshot before requesting human confirmation.
pub fn resolve_restore_id(ctx: &Ctx, selector: Option<&str>) -> Result<String> {
    let _lock = ctx.store.lock()?;
    let fingerprint = ctx.db.fingerprint();
    if ctx.store.load_config()? != fingerprint {
        bail!("project binding changed");
    }
    ensure_catalog_binding(&ctx.store, &fingerprint)?;
    if ctx.store.root.join("postgres-restore.json").exists() {
        bail!("pending recovery must be resolved before requesting restore approval");
    }
    Ok(ctx.store.load_metadata()?.find(selector)?.id)
}

fn pg_target(db: &ResolvedDb) -> Option<&ServerTarget> {
    match &db.target {
        DbTarget::Postgres(pg) => Some(pg),
        _ => None,
    }
}

/// Cap the number of automatic safety snapshots retained (design §4-T7).
fn prune_auto_snapshots(
    meta: &mut Metadata,
    store: &Storage,
    pg: Option<&ServerTarget>,
) -> Result<()> {
    let mut autos: Vec<SnapshotRecord> = meta
        .snapshots
        .iter()
        .filter(|s| s.tag.starts_with("auto-before-restore-") && !s.pinned)
        .cloned()
        .collect();
    if autos.len() <= AUTO_SAFETY_KEEP {
        return Ok(());
    }
    autos.sort_by_key(|a| std::cmp::Reverse(a.created_at));
    let victims: Vec<SnapshotRecord> = autos.split_off(AUTO_SAFETY_KEEP);
    meta.snapshots
        .retain(|s| !victims.iter().any(|v| v.id == s.id));
    validate_prune_survivors(store, meta, &victims)?;
    meta.pending_gc.extend(victims.clone());
    store.write_metadata(meta)?;
    retry_gc(store, meta, pg)?;
    Ok(())
}

fn validate_prune_survivors(
    store: &Storage,
    survivors: &Metadata,
    victims: &[SnapshotRecord],
) -> Result<()> {
    if victims
        .iter()
        .any(|v| matches!(v.location, crate::storage::Location::Manifest { .. }))
    {
        for snapshot in &survivors.snapshots {
            if let crate::storage::Location::Manifest { path } = &snapshot.location {
                let manifest = store.read_manifest_file(path).map_err(|e| {
                    anyhow::anyhow!(
                        "cannot safely prune: survivor manifest {path} cannot be read: {e:#}"
                    )
                })?;
                crate::storage::cas::verify_manifest(&manifest, &snapshot.blake3, snapshot.original_bytes)
                    .map_err(|e| anyhow::anyhow!("cannot safely prune: survivor manifest {path} failed integrity check: {e:#}"))?;
            }
        }
    }
    Ok(())
}

/// Delete a set of victim records' artifacts. Anything still referenced by a
/// surviving record is kept (dedup-linked snapshots share files and chunks —
/// §4-T13). Returns (records removed, bytes freed).
fn remove_records(
    store: &Storage,
    survivors: &Metadata,
    victims: &[SnapshotRecord],
    pg: Option<&ServerTarget>,
) -> Result<(usize, u64)> {
    let survivor_files: HashSet<&str> = survivors
        .snapshots
        .iter()
        .filter_map(|s| s.file_path())
        .collect();
    let mut survivor_manifests: HashSet<&str> = HashSet::new();
    let mut survivor_chunks: HashSet<String> = HashSet::new();
    let mut survivor_pgdbs: HashSet<&str> = HashSet::new();
    // Manifest reading is the only O(catalog × chunks) part here — skip it
    // entirely when no delta snapshot is involved in the prune.
    let needs_manifest_scan = victims
        .iter()
        .any(|v| matches!(v.location, crate::storage::Location::Manifest { .. }));
    for s in &survivors.snapshots {
        match &s.location {
            crate::storage::Location::Manifest { path } => {
                survivor_manifests.insert(path.as_str());
                if needs_manifest_scan {
                    let m = store.read_manifest_file(path).map_err(|e| {
                        anyhow::anyhow!(
                            "cannot safely prune: survivor manifest {path} cannot be read: {e:#}"
                        )
                    })?;
                    survivor_chunks.extend(m.hexes());
                }
            }
            crate::storage::Location::PgDatabase { name } => {
                survivor_pgdbs.insert(name.as_str());
            }
            crate::storage::Location::File { .. } => {}
        }
    }

    let mut removed = 0usize;
    let mut freed = 0u64;
    for v in victims {
        match &v.location {
            crate::storage::Location::File { path } => {
                if !survivor_files.contains(path.as_str())
                    && fs::remove_file(store.checked_artifact_path(path, "snapshots")?).is_ok()
                {
                    freed += v.size_bytes;
                }
            }
            crate::storage::Location::Manifest { path } => {
                if !survivor_manifests.contains(path.as_str()) {
                    let mut chunks_removed = true;
                    // Free the manifest's unique chunks, then the manifest.
                    if let Ok(m) = store.read_manifest_file(path) {
                        for hex in m.hexes() {
                            if !survivor_chunks.contains(&hex) {
                                let chunk = store.checked_chunk_path(&hex)?;
                                match fs::metadata(&chunk) {
                                    Ok(md) => {
                                        if fs::remove_file(&chunk).is_ok() {
                                            freed += md.len();
                                        } else {
                                            chunks_removed = false;
                                        }
                                    }
                                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                                    Err(_) => chunks_removed = false,
                                }
                            }
                        }
                    } else if store.checked_artifact_path(path, "manifests")?.exists() {
                        chunks_removed = false;
                    }
                    if chunks_removed {
                        fs::remove_file(store.checked_artifact_path(path, "manifests")?).ok();
                    }
                }
            }
            crate::storage::Location::PgDatabase { name } => {
                // Server-side clone: drop the managed database (§2.2 rule 1 —
                // only names under the __dbsnap_ prefix are ever dropped).
                // Best-effort: an unreachable server must not fail the prune.
                if !survivor_pgdbs.contains(name.as_str()) {
                    if let Some(pg) = pg {
                        if let Err(e) = crate::engine::postgres::drop_managed_db(pg, name) {
                            eprintln!(
                                "warning: {}",
                                util::mask(&format!("could not drop {name}: {e:#}"))
                            );
                        }
                    }
                }
            }
        }
        removed += 1;
    }
    Ok((removed, freed))
}

pub fn list_at(root: &Path, explicit: Option<&str>) -> Result<String> {
    redact((|| {
        let ctx = context_at(root, explicit, false, false)?;
        list_with(&ctx)
    })())
}

pub fn diff_at(root: &Path, explicit: Option<&str>, from: &str, to: &str) -> Result<String> {
    redact((|| {
        let ctx = context_at(root, explicit, false, false)?;
        diff_with(&ctx, from, to)
    })())
}

pub fn diff_with(ctx: &Ctx, from: &str, to: &str) -> Result<String> {
    let _lock = ctx.store.lock()?;
    ensure_active_binding(ctx)?;
    let meta = ctx.store.load_metadata()?;
    let left = meta.find(Some(from))?;
    let right = meta.find(Some(to))?;
    match &ctx.db.target {
        DbTarget::Sqlite(path) if left.engine == "sqlite" && right.engine == "sqlite" => {
            engine::sqlite::SqliteEngine::new(path.clone()).diff(&ctx.store, &left, &right)
        }
        DbTarget::Postgres(pg) if left.engine == "postgres" && right.engine == "postgres" => {
            engine::postgres::PostgresEngine::new(pg.clone()).diff(&ctx.store, &left, &right)
        }
        DbTarget::Mysql(target) if left.engine == "mysql" && right.engine == "mysql" => {
            engine::mysql::MysqlEngine::new(target.clone()).diff(&ctx.store, &left, &right)
        }
        _ => bail!("snapshot diff is not supported for these snapshot types"),
    }
}

pub fn list_json_at(root: &Path, explicit: Option<&str>) -> Result<String> {
    redact((|| {
        let ctx = context_at(root, explicit, false, false)?;
        let _lock = ctx.store.lock()?;
        ensure_active_binding(&ctx)?;
        let mut snapshots = ctx.store.load_metadata()?.snapshots;
        snapshots.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        let snapshots: Vec<serde_json::Value> = snapshots
            .iter()
            .map(|snapshot| {
                let mut value = serde_json::to_value(snapshot)?;
                value["integrity_kind"] = serde_json::json!(if snapshot.method == "pg-clone" {
                    "metadata_fingerprint_not_content_checksum"
                } else {
                    "content_checksum"
                });
                Ok::<_, serde_json::Error>(value)
            })
            .collect::<std::result::Result<_, _>>()?;
        Ok(serde_json::to_string(&snapshots)?)
    })())
}

pub fn list_with(ctx: &Ctx) -> Result<String> {
    let _lock = ctx.store.lock()?;
    ensure_active_binding(ctx)?;
    let meta = ctx.store.load_metadata()?;
    if meta.snapshots.is_empty() {
        return Ok("no snapshots yet — create one with: dbsnap save \"my-tag\"".to_string());
    }
    let mut snaps = meta.snapshots.clone();
    snaps.sort_by_key(|a| std::cmp::Reverse(a.created_at));

    let mut ids: Vec<String> = snaps.iter().map(|s| s.id.clone()).collect();
    let mut tags: Vec<String> = snaps
        .iter()
        .map(|s| {
            if s.pinned {
                format!("[PINNED] {}", s.tag)
            } else {
                s.tag.clone()
            }
        })
        .collect();
    let mut createds: Vec<String> = snaps
        .iter()
        .map(|s| util::relative_time(s.created_at))
        .collect();
    let mut sizes: Vec<String> = snaps
        .iter()
        .map(|s| {
            let size = util::format_size(s.size_bytes);
            // delta snapshots: "+" marks the incremental cost of this snapshot
            if s.is_delta() {
                format!("+{size}")
            } else {
                size
            }
        })
        .collect();
    for col in [&mut ids, &mut tags, &mut createds, &mut sizes] {
        let width = col.iter().map(|c| c.chars().count()).max().unwrap_or(0);
        for c in col.iter_mut() {
            let pad = width - c.chars().count();
            c.push_str(&" ".repeat(pad));
        }
    }

    let header = format!("{:<8}  {:<40}  {:<14}  {}", "ID", "TAG", "CREATED", "SIZE");
    let mut out = header;
    out.push('\n');
    for i in 0..snaps.len() {
        let tag = if tags[i].chars().count() > 40 {
            format!("{}…", tags[i].chars().take(39).collect::<String>())
        } else {
            tags[i].clone()
        };
        out.push_str(&format!(
            "{:<8}  {:<40}  {:<14}  {}\n",
            ids[i],
            tag,
            createds[i].trim_end(),
            sizes[i].trim_end()
        ));
    }
    out.pop();
    if snaps.iter().any(|s| s.method == "pg-clone") {
        out.push_str("\nPostgreSQL clones have a metadata fingerprint, not a content checksum.");
    }
    Ok(out)
}

pub fn prune_at(root: &Path, explicit: Option<&str>, keep: usize) -> Result<String> {
    redact((|| {
        let ctx = context_at(root, explicit, false, false)?;
        prune_with(&ctx, keep)
    })())
}

pub fn prune_with(ctx: &Ctx, keep: usize) -> Result<String> {
    let _lock = ctx.store.lock()?;
    ensure_active_binding(ctx)?;
    let mut meta = ctx.store.load_metadata()?;
    meta.snapshots
        .sort_by_key(|a| std::cmp::Reverse(a.created_at));

    let mut freed = 0u64;
    let mut removed = 0usize;
    if keep < meta.snapshots.len() {
        let candidates = meta.snapshots.split_off(keep);
        let (protected, victims): (Vec<SnapshotRecord>, Vec<SnapshotRecord>) =
            candidates.into_iter().partition(|s| s.pinned);
        meta.snapshots.extend(protected);
        validate_prune_survivors(&ctx.store, &meta, &victims)?;
        meta.pending_gc.extend(victims.clone());
        ctx.store.write_metadata(&meta)?;
        if std::env::var("DBSNAP_TEST_FAILPOINT").as_deref() == Ok("prune_after_metadata") {
            bail!("injected interruption after prune metadata commit; retry prune to collect recorded artifacts");
        }
        let (n, f) = retry_gc(&ctx.store, &mut meta, pg_target(&ctx.db))?;
        removed = n;
        freed = f;
    }
    if !meta.pending_gc.is_empty() {
        let (n, f) = retry_gc(&ctx.store, &mut meta, pg_target(&ctx.db))?;
        removed += n;
        freed += f;
    }
    ctx.store.write_metadata(&meta)?;

    let pending_hint = if meta.pending_gc.is_empty() {
        String::new()
    } else {
        format!("\n{} recorded artifacts remain pending GC; inspect doctor and retry prune after fixing the cause",meta.pending_gc.len())
    };
    if removed == 0 {
        Ok(format!(
            "nothing to prune — {} snapshots kept{pending_hint}",
            meta.snapshots.len()
        ))
    } else {
        Ok(format!(
            "pruned {removed} snapshots, freed {} — {} snapshots kept{pending_hint}",
            util::format_size(freed),
            meta.snapshots.len()
        ))
    }
}

fn retry_gc(
    store: &Storage,
    meta: &mut Metadata,
    pg: Option<&ServerTarget>,
) -> Result<(usize, u64)> {
    let victims = meta.pending_gc.clone();
    validate_prune_survivors(store, meta, &victims)?;
    let (_, freed) = remove_records(store, meta, &victims, pg)?;
    let mut remaining = Vec::new();
    for record in &victims {
        let remains = match &record.location {
            crate::storage::Location::File { path } => {
                store.checked_artifact_path(path, "snapshots")?.exists()
            }
            crate::storage::Location::Manifest { path } => {
                store.checked_artifact_path(path, "manifests")?.exists()
            }
            crate::storage::Location::PgDatabase { name } => match pg {
                Some(target) => engine::postgres::PostgresEngine::new(target.clone())
                    .managed_exists(name)
                    .unwrap_or(true),
                None => true,
            },
        };
        // Shared survivor artifacts are already retained by design, not GC
        // failures. Never delete them merely to empty the queue.
        let shared = meta.snapshots.iter().any(|s| s.location == record.location);
        if remains && !shared {
            remaining.push(record.clone());
        }
    }
    let removed = victims.len() - remaining.len();
    meta.pending_gc = remaining;
    store.write_metadata(meta)?;
    Ok((removed, freed))
}

pub fn pin_at(root: &Path, explicit: Option<&str>, selector: &str, pinned: bool) -> Result<String> {
    redact((|| {
        let ctx = context_at(root, explicit, false, false)?;
        let _lock = ctx.store.lock()?;
        ensure_active_binding(&ctx)?;
        if !pinned && ctx.store.root.join("mysql-restore.json").exists() {
            bail!("finish MySQL recovery before unpinning snapshots");
        }
        let mut meta = ctx.store.load_metadata()?;
        let record = meta.find(Some(selector))?;
        let snapshot = meta
            .snapshots
            .iter_mut()
            .find(|s| s.id == record.id)
            .expect("record came from metadata");
        snapshot.pinned = pinned;
        ctx.store.write_metadata(&meta)?;
        Ok(format!(
            "snapshot {} {}",
            record.id,
            if pinned { "pinned" } else { "unpinned" }
        ))
    })())
}

pub fn status_at(root: &Path, explicit: Option<&str>) -> Result<String> {
    redact((|| {
        let ctx = context_at(root, explicit, false, false)?;
        status_with(&ctx)
    })())
}

pub fn status_json_at(root: &Path, explicit: Option<&str>) -> Result<String> {
    redact((|| {
        let ctx = context_at(root, explicit, false, false)?;
        let _lock = ctx.store.lock()?;
        ensure_active_binding(&ctx)?;
        let info = engine::make(&ctx.db)?.probe()?;
        let meta = ctx.store.load_metadata()?;
        let (chunks, chunk_bytes) = scan_dir_size(&ctx.store.objects_dir());
        Ok(serde_json::to_string(&serde_json::json!({
            "engine": ctx.db.engine.to_string(),
            "database": ctx.db.display(),
            "engine_detail": info.detail,
            "database_size_bytes": info.size_bytes,
            "snapshot_count": meta.snapshots.len(),
            "chunk_count": chunks,
            "chunk_bytes": chunk_bytes,
            "disk_free_bytes": ctx.store.free_bytes().ok(),
            "mysql_recovery_pending": ctx.store.root.join("mysql-restore.json").exists(),
        }))?)
    })())
}

pub fn status_with(ctx: &Ctx) -> Result<String> {
    let _lock = ctx.store.lock()?;
    ensure_active_binding(ctx)?;
    let meta = ctx.store.load_metadata()?;
    let engine = engine::make(&ctx.db)?;
    let info = engine.probe()?;

    let unique_files: HashSet<&str> = meta
        .snapshots
        .iter()
        .filter_map(|s| s.file_path())
        .collect();
    let on_disk: u64 = unique_files
        .iter()
        .filter_map(|p| fs::metadata(ctx.store.root.join(p)).ok())
        .map(|m| m.len())
        .sum();
    let tmp_leftovers = fs::read_dir(ctx.store.tmp_dir())
        .map(|rd| rd.flatten().count())
        .unwrap_or(0);
    let (objects, object_bytes) = scan_dir_size(&ctx.store.objects_dir());
    let manifests = fs::read_dir(ctx.store.manifests_dir())
        .map(|rd| rd.flatten().count())
        .unwrap_or(0);

    let size = info
        .size_bytes
        .map(|s| format!(", {}", util::format_size(s)))
        .unwrap_or_default();
    let free = ctx
        .store
        .free_bytes()
        .map(|f| format!("\n  disk free : {}", util::format_size(f)))
        .unwrap_or_default();
    let recovery = if ctx.store.root.join("mysql-restore.json").exists() {
        "\nWARNING: MySQL recovery is pending; restore the pinned safety snapshot identified in mysql-restore.json."
    } else {
        ""
    };

    Ok(format!(
        "database  : {} ({})\nengine    : {}{}\nsnapshots : {} ({} unique files, {} on disk)\ndelta     : {manifests} manifests, {objects} chunks ({})\ntmp       : {tmp_leftovers} leftover files{free}{recovery}",
        ctx.db.engine,
        ctx.db.display(),
        info.detail,
        size,
        meta.snapshots.len(),
        unique_files.len(),
        util::format_size(on_disk),
        util::format_size(object_bytes),
    ))
}

/// (file count, total bytes) of a directory tree.
fn scan_dir_size(dir: &Path) -> (usize, u64) {
    let mut count = 0usize;
    let mut total = 0u64;
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let (c, t) = scan_dir_size(&path);
                count += c;
                total += t;
            } else if let Ok(md) = entry.metadata() {
                count += 1;
                total += md.len();
            }
        }
    }
    (count, total)
}
