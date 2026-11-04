//! Bounded, primary-key-based preview across snapshot formats.
use super::*;
use anyhow::Context;
use colored::Colorize;
use rusqlite::{types::ValueRef, Connection, OpenFlags};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt::Write as _;

type Column = (String, String, i64);
type Digest = [u8; 32];
const MAX_PREVIEW_BYTES: usize = 16 * 1024 * 1024;

#[derive(Default)]
pub struct RowOptions<'a> {
    pub show_values: bool,
    pub git: bool,
    pub replay_dumps: bool,
    pub docker_image: Option<&'a str>,
}

struct Row {
    digest: Digest,
    values: Vec<Value>,
}

struct Inventory {
    columns: Vec<Column>,
    keys: Vec<usize>,
    rows: BTreeMap<Digest, Row>,
    bytes: usize,
}

impl Inventory {
    fn new(columns: Vec<Column>) -> Result<Self> {
        let mut keys: Vec<_> = columns
            .iter()
            .enumerate()
            .filter_map(|(index, column)| (column.2 > 0).then_some((column.2, index)))
            .collect();
        keys.sort_unstable();
        if keys.is_empty() {
            bail!("row comparison requires a table with an explicit primary key");
        }
        Ok(Self {
            columns,
            keys: keys.into_iter().map(|(_, index)| index).collect(),
            rows: BTreeMap::new(),
            bytes: 0,
        })
    }

    fn insert(&mut self, values: Vec<Value>, limit: usize, show_values: bool) -> Result<()> {
        if self.rows.len() >= limit {
            bail!("row preview limit exceeded; increase --limit deliberately (maximum 100000)");
        }
        if values.len() != self.columns.len() || self.keys.iter().any(|&i| values[i].is_null()) {
            bail!("invalid row layout or NULL primary key");
        }
        let encoded = serde_json::to_vec(&values)?;
        self.bytes = self.bytes.saturating_add(encoded.len());
        if self.bytes > MAX_PREVIEW_BYTES {
            bail!("row preview exceeds 16 MiB of encoded values per snapshot");
        }
        let key: Vec<_> = self.keys.iter().map(|&index| &values[index]).collect();
        let key = *blake3::hash(&serde_json::to_vec(&key)?).as_bytes();
        let row = Row {
            digest: *blake3::hash(&encoded).as_bytes(),
            values: if show_values { values } else { Vec::new() },
        };
        if self.rows.insert(key, row).is_some() {
            bail!("duplicate primary key in preview");
        }
        Ok(())
    }
}

struct Staged(std::path::PathBuf);
impl Drop for Staged {
    fn drop(&mut self) {
        fs::remove_file(&self.0).ok();
    }
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn text_or_hex(bytes: &[u8]) -> Value {
    match std::str::from_utf8(bytes) {
        Ok(text) => json!(text),
        Err(_) => json!({"hex": hex(bytes)}),
    }
}

fn sqlite_inventory(path: &Path, table: &str, limit: usize, values: bool) -> Result<Inventory> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let identifier = quote(table);
    let mut statement = db.prepare(&format!("PRAGMA table_info({identifier})"))?;
    let columns = statement
        .query_map([], |r| Ok((r.get(1)?, r.get(2)?, r.get(5)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut inventory = Inventory::new(columns)?;
    let mut select = db.prepare(&format!("SELECT * FROM {identifier} LIMIT {}", limit + 1))?;
    if select.column_count() != inventory.columns.len() {
        bail!("generated/hidden columns are not supported by row comparison");
    }
    let mut rows = select.query([])?;
    while let Some(row) = rows.next()? {
        let mut cells = Vec::with_capacity(inventory.columns.len());
        for index in 0..inventory.columns.len() {
            cells.push(match row.get_ref(index)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(v) => json!(v),
                ValueRef::Real(v) if v.is_finite() => json!(v),
                ValueRef::Real(v) => json!({"real": v.to_string()}),
                ValueRef::Text(v) => text_or_hex(v),
                ValueRef::Blob(v) => json!({"blob_hex": hex(v)}),
            });
        }
        inventory.insert(cells, limit, values)?;
    }
    Ok(inventory)
}

fn postgres_inventory(
    store: &Storage,
    pg: &ServerTarget,
    record: &SnapshotRecord,
    table: &str,
    limit: usize,
    options: &RowOptions<'_>,
) -> Result<Inventory> {
    use engine::postgres::{drop_managed_db, PostgresEngine};
    let engine = PostgresEngine::new(pg.clone());
    let temporary = if record.method == "pg-dump" {
        if !options.replay_dumps {
            bail!("PostgreSQL dump preview requires explicit --replay-dumps for trusted SQL");
        }
        Some(engine.create_sandbox(store, record, &store.load_metadata()?.ids())?)
    } else {
        None
    };
    let result = (|| {
        let name = match &temporary {
            Some(name) => name,
            None => PostgresEngine::clone_name(record)?,
        };
        let mut conn = engine.connect(name)?;
        let mut tx = conn.build_transaction().read_only(true).start()?;
        tx.batch_execute("SET LOCAL search_path TO pg_catalog; SET LOCAL TimeZone TO 'UTC'; SET LOCAL DateStyle TO 'ISO, YMD'; SET LOCAL IntervalStyle TO 'postgres'; SET LOCAL bytea_output TO 'hex'; SET LOCAL extra_float_digits TO 3")?;
        let (schema, table) = table.split_once('.').unwrap_or(("public", table));
        let columns = tx
            .query(
                "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), COALESCE(k.pos, 0)
             FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
             JOIN pg_attribute a ON a.attrelid=c.oid
             LEFT JOIN pg_index i ON i.indrelid=c.oid AND i.indisprimary
             LEFT JOIN LATERAL unnest(i.indkey) WITH ORDINALITY AS k(attnum,pos)
                 ON k.attnum=a.attnum AND k.pos <= i.indnkeyatts
             WHERE n.nspname=$1 AND c.relname=$2 AND c.relkind='r'
               AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum",
                &[&schema, &table],
            )?
            .iter()
            .map(|row| (row.get(0), row.get(1), row.get(2)))
            .collect();
        let mut inventory = Inventory::new(columns)?;
        let cells = inventory
            .columns
            .iter()
            .map(|c| format!("{}::pg_catalog.text", quote(&c.0)))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT ARRAY[{cells}] FROM ONLY {}.{} LIMIT {}",
            quote(schema),
            quote(table),
            limit + 1
        );
        use postgres::fallible_iterator::FallibleIterator;
        let mut rows = tx.query_raw(&sql, std::iter::empty::<&str>())?;
        while let Some(row) = rows.next()? {
            let cells: Vec<Option<String>> = row.try_get(0)?;
            inventory.insert(
                cells
                    .into_iter()
                    .map(|v| v.map_or(Value::Null, Value::String))
                    .collect(),
                limit,
                options.show_values,
            )?;
        }
        Ok(inventory)
    })();
    if let Some(name) = temporary {
        finish_cleanup(result, drop_managed_db(pg, &name), &name)
    } else {
        result
    }
}

fn finish_cleanup<T>(result: Result<T>, cleanup: Result<()>, name: &str) -> Result<T> {
    match (result, cleanup) {
        (result, Ok(())) => result,
        (Ok(_), Err(cleanup)) => Err(cleanup.context(format!("cleanup failed for {name}"))),
        (Err(error), Err(cleanup)) => bail!("{error:#}; cleanup failed for {name}: {cleanup:#}"),
    }
}

fn mysql_query(id: &str, client: &str, sql: &str) -> Result<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new("docker")
        .args([
            "exec",
            id,
            client,
            "--no-defaults",
            "-uroot",
            "--batch",
            "--raw",
            "--skip-column-names",
            "--default-character-set=utf8mb4",
            "-e",
            sql,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut output = Vec::new();
    let read = child
        .stdout
        .take()
        .context("missing sandbox query output")?
        .take((MAX_PREVIEW_BYTES * 2 + 1) as u64)
        .read_to_end(&mut output);
    if read.is_err() || output.len() > MAX_PREVIEW_BYTES * 2 {
        let _ = child.kill();
        let _ = child.wait();
        bail!("MySQL sandbox row query exceeded the output budget or failed to read");
    }
    if !child.wait()?.success() {
        bail!("MySQL sandbox row query failed");
    }
    Ok(String::from_utf8(output)?)
}

fn unhex(value: &str) -> Result<Vec<u8>> {
    if !value.is_ascii() || value.len() % 2 != 0 {
        bail!("invalid hexadecimal data from sandbox query");
    }
    (0..value.len())
        .step_by(2)
        .map(|i| Ok(u8::from_str_radix(&value[i..i + 2], 16)?))
        .collect()
}

fn mysql_inventory(
    store: &Storage,
    target: &ServerTarget,
    record: &SnapshotRecord,
    table: &str,
    limit: usize,
    options: &RowOptions<'_>,
) -> Result<Inventory> {
    use engine::mysql_sandbox;
    let image = options
        .docker_image
        .context("MySQL/MariaDB row preview requires explicit --docker-image for trusted dumps")?;
    let client = if image == "mariadb:11" {
        "mariadb"
    } else {
        "mysql"
    };
    let id = mysql_sandbox::start(store, record, image)?;
    let result = (|| {
        let schema = &target.database;
        let columns = mysql_query(
            &id,
            client,
            &format!(
                "SELECT HEX(c.COLUMN_NAME),HEX(c.COLUMN_TYPE),COALESCE(k.ORDINAL_POSITION,0)
             FROM information_schema.COLUMNS c
             JOIN information_schema.TABLES t USING (TABLE_SCHEMA,TABLE_NAME)
             LEFT JOIN information_schema.KEY_COLUMN_USAGE k
              ON k.TABLE_SCHEMA=c.TABLE_SCHEMA AND k.TABLE_NAME=c.TABLE_NAME
              AND k.COLUMN_NAME=c.COLUMN_NAME AND k.CONSTRAINT_NAME='PRIMARY'
             WHERE BINARY c.TABLE_SCHEMA=UNHEX('{}') AND BINARY c.TABLE_NAME=UNHEX('{}')
             AND t.TABLE_TYPE='BASE TABLE' ORDER BY c.ORDINAL_POSITION",
                hex(schema.as_bytes()),
                hex(table.as_bytes())
            ),
        )?
        .lines()
        .map(|line| {
            let mut fields = line.split('\t');
            let name = String::from_utf8(unhex(fields.next().context("missing column name")?)?)?;
            let typ = String::from_utf8(unhex(fields.next().context("missing column type")?)?)?;
            let pk = fields
                .next()
                .context("missing primary key position")?
                .parse::<i64>()?;
            Ok((name, typ, pk))
        })
        .collect::<Result<Vec<_>>>()?;
        let mut inventory = Inventory::new(columns)?;
        let ident = |s: &str| format!("`{}`", s.replace('`', "``"));
        let fields = inventory
            .columns
            .iter()
            .map(|c| format!("HEX(CAST({} AS BINARY))", ident(&c.0)))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT {fields} FROM {}.{} LIMIT {}",
            ident(schema),
            ident(table),
            limit + 1
        );
        for line in mysql_query(&id, client, &sql)?.lines() {
            let cells = line
                .split('\t')
                .map(|cell| {
                    if cell == "NULL" {
                        Ok(Value::Null)
                    } else {
                        Ok(text_or_hex(&unhex(cell)?))
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            inventory.insert(cells, limit, options.show_values)?;
        }
        Ok(inventory)
    })();
    finish_cleanup(result, mysql_sandbox::remove(&id), &id)
}

pub fn diff_rows_at(
    root: &Path,
    explicit: Option<&str>,
    from: &str,
    to: &str,
    table: &str,
    limit: usize,
) -> Result<String> {
    diff_rows_options_at(
        root,
        explicit,
        from,
        to,
        table,
        limit,
        &RowOptions::default(),
    )
}

pub fn diff_rows_options_at(
    root: &Path,
    explicit: Option<&str>,
    from: &str,
    to: &str,
    table: &str,
    limit: usize,
    options: &RowOptions<'_>,
) -> Result<String> {
    redact((|| {
        diff_rows_options_with(
            &context_at(root, explicit, false, false)?,
            from,
            to,
            table,
            limit,
            options,
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
    diff_rows_options_with(ctx, from, to, table, limit, &RowOptions::default())
}

pub fn diff_rows_options_with(
    ctx: &Ctx,
    from: &str,
    to: &str,
    table: &str,
    limit: usize,
    options: &RowOptions<'_>,
) -> Result<String> {
    let _lock = ctx.store.lock()?;
    super::health::check_binding(ctx)?;
    if !(1..=100000).contains(&limit) {
        bail!("limit must be between 1 and 100000");
    }
    if options.show_values && !options.git {
        bail!("--show-values requires --format git");
    }
    if table.is_empty() || table.contains('\0') {
        bail!("invalid table name");
    }
    let meta = ctx.store.load_metadata()?;
    let left = meta.find(Some(from))?;
    let right = meta.find(Some(to))?;
    let inspect = |record: &SnapshotRecord| {
        if record.engine != ctx.db.engine.to_string() {
            bail!("snapshot engine differs from binding");
        }
        match &ctx.db.target {
            DbTarget::Sqlite(path) => {
                let mut staging_record = record.clone();
                staging_record.id = util::new_id(&meta.ids());
                let staged = Staged(
                    engine::sqlite::SqliteEngine::new(path.clone())
                        .stage(&ctx.store, &staging_record)?,
                );
                sqlite_inventory(&staged.0, table, limit, options.show_values)
            }
            DbTarget::Postgres(pg) => {
                postgres_inventory(&ctx.store, pg, record, table, limit, options)
            }
            DbTarget::Mysql(target) => {
                mysql_inventory(&ctx.store, target, record, table, limit, options)
            }
        }
    };
    let a = inspect(&left).context("reading first snapshot")?;
    let b = inspect(&right).context("reading second snapshot")?;
    if a.columns != b.columns {
        if options.git {
            return Ok(git_schema(
                &left.id, &right.id, table, &a.columns, &b.columns,
            ));
        }
        bail!("table column layout changed; inspect schema diff before row comparison");
    }
    let added = b.rows.keys().filter(|k| !a.rows.contains_key(*k)).count();
    let removed = a.rows.keys().filter(|k| !b.rows.contains_key(*k)).count();
    let updated = a
        .rows
        .iter()
        .filter(|(k, v)| b.rows.get(*k).is_some_and(|new| new.digest != v.digest))
        .count();
    if options.git {
        let mut report = format!(
            "--- {}\n+++ {}\n@@ {} @@\n",
            left.id,
            right.id,
            json!(table)
        );
        if options.show_values {
            for (key, before) in &a.rows {
                match b.rows.get(key) {
                    None => git_change(&mut report, &a, Some(before), None),
                    Some(after) if before.digest != after.digest => {
                        git_change(&mut report, &a, Some(before), Some(after))
                    }
                    _ => {}
                }
            }
            for (key, after) in &b.rows {
                if !a.rows.contains_key(key) {
                    git_change(&mut report, &b, None, Some(after));
                }
            }
        }
        let _ = writeln!(
            report,
            "{}  {}  {}",
            format!("+{added} added").green(),
            format!("-{removed} removed").red(),
            format!("~{updated} changed").yellow()
        );
        return Ok(report);
    }
    Ok(serde_json::to_string_pretty(
        &json!({"operation":"diff_rows","table":table,"from":left.id,"to":right.id,"added":added,"removed":removed,"updated":updated,"row_values_disclosed":false,"limit":limit}),
    )?)
}

fn git_schema(from: &str, to: &str, table: &str, before: &[Column], after: &[Column]) -> String {
    let mut report = format!("--- {from}\n+++ {to}\n@@ {} schema @@\n", json!(table));
    for (marker, columns, other) in [('-', before, after), ('+', after, before)] {
        for (index, (name, kind, pk)) in columns.iter().enumerate() {
            if other.get(index) == Some(&columns[index]) {
                continue;
            }
            let line = format!(
                "{marker} {} {} (position {}, primary-key position {pk})",
                json!(name),
                json!(kind),
                index + 1
            );
            let _ = writeln!(
                report,
                "{}",
                if marker == '-' {
                    line.red()
                } else {
                    line.green()
                }
            );
        }
    }
    report.push_str("Row preview requires matching column layouts.\n");
    report
}

fn git_change(out: &mut String, inventory: &Inventory, before: Option<&Row>, after: Option<&Row>) {
    let row = before.or(after).expect("diff has at least one row");
    let keys = inventory
        .keys
        .iter()
        .map(|&i| {
            format!(
                "{}={}",
                json!(inventory.columns[i].0),
                display_cell(&row.values[i])
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(out, "@@ {keys} @@");
    for (i, column) in inventory.columns.iter().enumerate() {
        let old = before.map(|r| &r.values[i]);
        let new = after.map(|r| &r.values[i]);
        if old == new {
            continue;
        }
        if let Some(value) = old {
            let _ = writeln!(
                out,
                "{}",
                format!("- {}: {}", json!(column.0), display_cell(value)).red()
            );
        }
        if let Some(value) = new {
            let _ = writeln!(
                out,
                "{}",
                format!("+ {}: {}", json!(column.0), display_cell(value)).green()
            );
        }
    }
}

fn display_cell(value: &Value) -> String {
    match value {
        Value::String(text) => json!(util::mask(text)).to_string(),
        other => other.to_string(),
    }
}
