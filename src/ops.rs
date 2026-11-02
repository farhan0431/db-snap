use crate::config::{self, DbTarget, ResolvedDb, ServerTarget};
use crate::engine;
use crate::storage::{Metadata, SnapshotRecord, Storage};
use crate::util;
use anyhow::{bail, Result};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

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
    let db = config::resolve(explicit, allow_remote)?;
    let fingerprint = db.fingerprint();
    if let Some(store) = Storage::find(root) {
        let bound = store.load_config()?;
        if bound != fingerprint {
            if !force {
                bail!(
                    "this project is bound to {} ({}), but the target database is {} ({})\nre-run `dbsnap init` to re-bind, or pass --force",
                    bound.engine,
                    bound.target,
                    fingerprint.engine,
                    fingerprint.target
                );
            }
            let store = Storage::create(root, &fingerprint)?;
            return Ok(Ctx { db, store });
        }
        Ok(Ctx { db, store })
    } else {
        let store = Storage::create(root, &fingerprint)?;
        Ok(Ctx { db, store })
    }
}

pub fn init_at(root: &Path, explicit: Option<&str>, allow_remote: bool) -> Result<String> {
    let db = config::resolve(explicit, allow_remote)?;
    let fingerprint = db.fingerprint();
    let existed = Storage::find(root).is_some();
    let store = Storage::create(root, &fingerprint)?;

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
    let skill_path = root
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
}

pub fn save_at(
    root: &Path,
    explicit: Option<&str>,
    tag: Option<String>,
    force: bool,
    allow_remote: bool,
    method: engine::SaveMethod,
) -> Result<String> {
    let ctx = context_at(root, explicit, force, allow_remote)?;
    save_with(&ctx, tag, method)
}

pub fn save_with(ctx: &Ctx, tag: Option<String>, method: engine::SaveMethod) -> Result<String> {
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
    let ctx = context_at(root, explicit, force, allow_remote)?;
    restore_with(&ctx, id_or_tag, safety)
}

pub fn restore_with(ctx: &Ctx, id_or_tag: Option<String>, safety: bool) -> Result<String> {
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
        .filter(|s| s.tag.starts_with("auto-before-restore-"))
        .cloned()
        .collect();
    if autos.len() <= AUTO_SAFETY_KEEP {
        return Ok(());
    }
    autos.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let victims: Vec<SnapshotRecord> = autos.split_off(AUTO_SAFETY_KEEP);
    meta.snapshots
        .retain(|s| !victims.iter().any(|v| v.id == s.id));
    remove_records(store, meta, &victims, pg);
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
) -> (usize, u64) {
    let survivor_files: HashSet<&str> = survivors
        .snapshots
        .iter()
        .filter_map(|s| s.file_path())
        .collect();
    let mut survivor_manifests: HashSet<&str> = HashSet::new();
    let mut survivor_chunks: HashSet<String> = HashSet::new();
    let mut survivor_pgdbs: HashSet<&str> = HashSet::new();
    for s in &survivors.snapshots {
        match &s.location {
            crate::storage::Location::Manifest { path } => {
                survivor_manifests.insert(path.as_str());
                if let Ok(m) = store.read_manifest_file(path) {
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
                    && fs::remove_file(store.root.join(path)).is_ok()
                {
                    freed += v.size_bytes;
                }
            }
            crate::storage::Location::Manifest { path } => {
                if !survivor_manifests.contains(path.as_str()) {
                    // Free the manifest's unique chunks, then the manifest.
                    if let Ok(m) = store.read_manifest_file(path) {
                        for hex in m.hexes() {
                            if !survivor_chunks.contains(&hex) {
                                let chunk = store.chunk_path(&hex);
                                if let Ok(md) = fs::metadata(&chunk) {
                                    let size = md.len();
                                    if fs::remove_file(&chunk).is_ok() {
                                        freed += size;
                                    }
                                }
                            }
                        }
                    }
                    fs::remove_file(store.root.join(path)).ok();
                }
            }
            crate::storage::Location::PgDatabase { name } => {
                // Server-side clone: drop the managed database (§2.2 rule 1 —
                // only names under the __dbsnap_ prefix are ever dropped).
                // Best-effort: an unreachable server must not fail the prune.
                if !survivor_pgdbs.contains(name.as_str()) {
                    if let Some(pg) = pg {
                        if let Err(e) = crate::engine::postgres::drop_managed_db(pg, name) {
                            eprintln!("warning: could not drop {name}: {e:#}");
                        }
                    }
                }
            }
        }
        removed += 1;
    }
    (removed, freed)
}

pub fn list_at(root: &Path, explicit: Option<&str>) -> Result<String> {
    let ctx = context_at(root, explicit, false, false)?;
    list_with(&ctx)
}

pub fn list_with(ctx: &Ctx) -> Result<String> {
    let meta = ctx.store.load_metadata()?;
    if meta.snapshots.is_empty() {
        return Ok("no snapshots yet — create one with: dbsnap save \"my-tag\"".to_string());
    }
    let mut snaps = meta.snapshots.clone();
    snaps.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    let mut ids: Vec<String> = snaps.iter().map(|s| s.id.clone()).collect();
    let mut tags: Vec<String> = snaps.iter().map(|s| s.tag.clone()).collect();
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
    Ok(out)
}

pub fn prune_at(root: &Path, explicit: Option<&str>, keep: usize) -> Result<String> {
    let ctx = context_at(root, explicit, false, false)?;
    prune_with(&ctx, keep)
}

pub fn prune_with(ctx: &Ctx, keep: usize) -> Result<String> {
    let mut meta = ctx.store.load_metadata()?;
    meta.snapshots
        .sort_by(|a, b| b.created_at.cmp(&a.created_at));

    let mut freed = 0u64;
    let mut removed = 0usize;
    if keep < meta.snapshots.len() {
        let victims: Vec<SnapshotRecord> = meta.snapshots.split_off(keep);
        let (n, f) = remove_records(&ctx.store, &meta, &victims, pg_target(&ctx.db));
        removed = n;
        freed = f;
    }
    clean_tmp(&ctx.store);
    ctx.store.write_metadata(&meta)?;

    if removed == 0 {
        Ok(format!(
            "nothing to prune — {} snapshots kept, tmp cleaned",
            meta.snapshots.len()
        ))
    } else {
        Ok(format!(
            "pruned {removed} snapshots, freed {} — {} snapshots kept",
            util::format_size(freed),
            meta.snapshots.len()
        ))
    }
}

pub fn status_at(root: &Path, explicit: Option<&str>) -> Result<String> {
    let ctx = context_at(root, explicit, false, false)?;
    status_with(&ctx)
}

pub fn status_with(ctx: &Ctx) -> Result<String> {
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

    Ok(format!(
        "database  : {} ({})\nengine    : {}{}\nsnapshots : {} ({} unique files, {} on disk)\ndelta     : {manifests} manifests, {objects} chunks ({})\ntmp       : {tmp_leftovers} leftover files{free}",
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

fn clean_tmp(store: &Storage) {
    if let Ok(rd) = fs::read_dir(store.tmp_dir()) {
        for entry in rd.flatten() {
            if entry.path().is_file() {
                fs::remove_file(entry.path()).ok();
            }
        }
    }
}
