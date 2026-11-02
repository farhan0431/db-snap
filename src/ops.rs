use crate::config::{self, ResolvedDb};
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
/// differs from the stored fingerprint (design §4-T2).
pub fn context_at(root: &Path, explicit: Option<&str>, force: bool) -> Result<Ctx> {
    let db = config::resolve(explicit)?;
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

pub fn init_at(root: &Path, explicit: Option<&str>) -> Result<String> {
    let db = config::resolve(explicit)?;
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
        db.path.display(),
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
                "\n  warning  : database file does not exist yet — it will be created on first save",
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
) -> Result<String> {
    let ctx = context_at(root, explicit, force)?;
    save_with(&ctx, tag)
}

pub fn save_with(ctx: &Ctx, tag: Option<String>) -> Result<String> {
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
    let outcome = engine.save(&ctx.store, &tag, previous.as_ref(), &taken)?;
    meta.snapshots.push(outcome.record.clone());
    ctx.store.write_metadata(&meta)?;

    let r = &outcome.record;
    let mut msg = format!(
        "saved snapshot {} \"{}\" — {} compressed ({} original)",
        r.id,
        r.tag,
        util::format_size(r.size_bytes),
        util::format_size(r.original_bytes)
    );
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
) -> Result<String> {
    let ctx = context_at(root, explicit, force)?;
    restore_with(&ctx, id_or_tag, safety)
}

pub fn restore_with(ctx: &Ctx, id_or_tag: Option<String>, safety: bool) -> Result<String> {
    let mut meta = ctx.store.load_metadata()?;
    let record = meta.find(id_or_tag.as_deref())?;
    let engine = engine::make(&ctx.db)?;

    // Verify integrity BEFORE anything touches the live database (§4-T3).
    let staged = engine.stage_restore(&ctx.store, &record)?;

    // Safety snapshot of the current state first (§4-T7).
    let mut safety_id: Option<String> = None;
    if safety {
        let taken = meta.ids();
        match engine.save(
            &ctx.store,
            &util::safety_tag(),
            meta.latest().as_ref(),
            &taken,
        ) {
            Ok(outcome) => {
                safety_id = Some(outcome.record.id.clone());
                meta.snapshots.push(outcome.record);
            }
            Err(e) => {
                eprintln!("warning: pre-restore safety snapshot failed: {e:#}");
            }
        }
    }

    if let Err(e) = engine.apply_restore(&staged) {
        fs::remove_file(&staged).ok();
        let hint = safety_id
            .as_ref()
            .map(|id| format!("\nthe pre-restore state was saved as snapshot {id} — recover with: dbsnap restore {id}"))
            .unwrap_or_default();
        bail!("restore failed: {e:#}{hint}");
    }
    fs::remove_file(&staged).ok();

    prune_auto_snapshots(&mut meta, &ctx.store)?;
    ctx.store.write_metadata(&meta)?;

    let mut msg = format!(
        "restored database to snapshot {} \"{}\"",
        record.id, record.tag
    );
    if let Some(id) = safety_id {
        msg.push_str(&format!(
            "\nprevious state kept as safety snapshot {id} (rollback: dbsnap restore {id})"
        ));
    }
    Ok(msg)
}

/// Cap the number of automatic safety snapshots retained (design §4-T7).
fn prune_auto_snapshots(meta: &mut Metadata, store: &Storage) -> Result<()> {
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
    let referenced: HashSet<&str> = meta
        .snapshots
        .iter()
        .filter_map(|s| s.file_path())
        .collect();
    for v in victims {
        if let Some(p) = v.file_path() {
            if !referenced.contains(p) {
                fs::remove_file(store.root.join(p)).ok();
            }
        }
    }
    Ok(())
}

pub fn list_at(root: &Path, explicit: Option<&str>) -> Result<String> {
    let ctx = context_at(root, explicit, false)?;
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
        .map(|s| util::format_size(s.size_bytes))
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
    let ctx = context_at(root, explicit, false)?;
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
        let referenced: HashSet<&str> = meta
            .snapshots
            .iter()
            .filter_map(|s| s.file_path())
            .collect();
        for v in victims {
            if let Some(p) = v.file_path() {
                // Never touch files still referenced by a surviving snapshot
                // (§4-T13 — dedup-linked snapshots share files).
                if !referenced.contains(p) && fs::remove_file(ctx.store.root.join(p)).is_ok() {
                    freed += v.size_bytes;
                    removed += 1;
                }
            }
        }
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
    let ctx = context_at(root, explicit, false)?;
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
        "database  : {} ({})\nengine    : {}{}\nsnapshots : {} ({} unique files, {} on disk)\ntmp       : {tmp_leftovers} leftover files{free}",
        ctx.db.engine,
        ctx.db.path.display(),
        info.detail,
        size,
        meta.snapshots.len(),
        unique_files.len(),
        util::format_size(on_disk)
    ))
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
