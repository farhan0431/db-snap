//! Bounded, primary-key-based preview for SQLite snapshots. Never prints values.
use super::*;
use anyhow::Context;
use rusqlite::{types::ValueRef, Connection, OpenFlags};
use serde_json::json;
use std::collections::BTreeMap;

type RowInventory = (Vec<(String, String, i64)>, BTreeMap<[u8; 32], [u8; 32]>);

struct Staged(std::path::PathBuf);
impl Drop for Staged {
    fn drop(&mut self) {
        fs::remove_file(&self.0).ok();
    }
}

fn inspect(path: &Path, table: &str, limit: usize) -> Result<RowInventory> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let identifier = format!("\"{}\"", table.replace('"', "\"\""));
    let mut statement = db.prepare(&format!("PRAGMA table_info({identifier})"))?;
    let columns = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if columns.is_empty() {
        bail!("table is missing or has no comparable columns");
    }
    let mut keys: Vec<(i64, usize)> = columns
        .iter()
        .enumerate()
        .filter_map(|(i, c)| (c.2 > 0).then_some((c.2, i)))
        .collect();
    keys.sort_unstable();
    if keys.is_empty() {
        bail!("row comparison requires an explicit primary key");
    }
    let mut select = db.prepare(&format!("SELECT * FROM {identifier} LIMIT {}", limit + 1))?;
    if select.column_count() != columns.len() {
        bail!("generated/hidden columns are not supported by row comparison");
    }
    let mut rows = select.query([])?;
    let mut result = BTreeMap::new();
    while let Some(row) = rows.next()? {
        if result.len() >= limit {
            bail!("row preview limit exceeded; increase --limit deliberately (maximum 100000)");
        }
        let mut key = blake3::Hasher::new();
        let mut content = blake3::Hasher::new();
        for (_, index) in &keys {
            let value = row.get_ref(*index)?;
            if value == ValueRef::Null {
                bail!("NULL primary key is unsupported");
            }
            hash_value(&mut key, value);
        }
        for i in 0..columns.len() {
            hash_value(&mut content, row.get_ref(i)?);
        }
        if result
            .insert(*key.finalize().as_bytes(), *content.finalize().as_bytes())
            .is_some()
        {
            bail!("duplicate primary key in preview");
        }
    }
    Ok((columns, result))
}

fn hash_value(hash: &mut blake3::Hasher, value: ValueRef<'_>) {
    let (kind, bytes): (u8, Vec<u8>) = match value {
        ValueRef::Null => (0, vec![]),
        ValueRef::Integer(v) => (1, v.to_le_bytes().to_vec()),
        ValueRef::Real(v) => (2, v.to_bits().to_le_bytes().to_vec()),
        ValueRef::Text(v) => (3, v.to_vec()),
        ValueRef::Blob(v) => (4, v.to_vec()),
    };
    hash.update(&[kind]);
    hash.update(&(bytes.len() as u64).to_le_bytes());
    hash.update(&bytes);
}

pub fn diff_rows_at(
    root: &Path,
    explicit: Option<&str>,
    from: &str,
    to: &str,
    table: &str,
    limit: usize,
) -> Result<String> {
    redact((|| {
        diff_rows_with(
            &context_at(root, explicit, false, false)?,
            from,
            to,
            table,
            limit,
        )
    })())
}

pub fn diff_rows_with(
    ctx: &Ctx,
    from: &str,
    to: &str,
    table: &str,
    limit: usize,
) -> Result<String> {
    let _lock = ctx.store.lock()?;
    ensure_active_binding(ctx)?;
    if !(1..=100000).contains(&limit) {
        bail!("limit must be between 1 and 100000");
    }
    let DbTarget::Sqlite(path) = &ctx.db.target else {
        bail!("bounded row diff currently supports SQLite only");
    };
    let meta = ctx.store.load_metadata()?;
    let left = meta.find(Some(from))?;
    let right = meta.find(Some(to))?;
    if left.engine != "sqlite" || right.engine != "sqlite" {
        bail!("row diff requires SQLite snapshots");
    }
    let engine = engine::sqlite::SqliteEngine::new(path.clone());
    let left_path = Staged(engine.stage(&ctx.store, &left)?);
    let right_path = Staged(engine.stage(&ctx.store, &right)?);
    let (a_schema, a) = inspect(&left_path.0, table, limit).context("reading first snapshot")?;
    let (b_schema, b) = inspect(&right_path.0, table, limit).context("reading second snapshot")?;
    if a_schema != b_schema {
        bail!("table column layout changed; inspect schema diff before row comparison");
    }
    let added = b.keys().filter(|k| !a.contains_key(*k)).count();
    let removed = a.keys().filter(|k| !b.contains_key(*k)).count();
    let updated = a
        .iter()
        .filter(|(k, v)| b.get(*k).is_some_and(|new| new != *v))
        .count();
    Ok(serde_json::to_string_pretty(
        &json!({"operation":"diff_rows","table":table,"from":left.id,"to":right.id,"added":added,"removed":removed,"updated":updated,"row_values_disclosed":false,"limit":limit}),
    )?)
}
