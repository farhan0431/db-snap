//! Offline archive inspection through pg_restore. Never supplies a database.
use crate::storage::{compress, SnapshotRecord, Storage, TemporaryFile};
use crate::util;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

fn extract(
    store: &Storage,
    record: &SnapshotRecord,
    mode: &str,
) -> Result<(TemporaryFile, std::path::PathBuf)> {
    let source = store.snapshot_path(record)?;
    if record.engine != "postgres" || record.method != "pg-dump" {
        bail!("archive diff requires pg-dump snapshots");
    }
    if compress::hash_file(&source)? != record.blake3
        || std::fs::metadata(&source)?.len() != record.size_bytes
    {
        bail!("PostgreSQL archive integrity check failed");
    }
    let path = store.tmp_dir().join(format!(
        "pg-diff-{mode}-{}.sql",
        util::new_id(&store.load_metadata()?.ids())
    ));
    let guard = TemporaryFile::create(path.clone())?;
    let reader = super::client_tools::candidates(
        "pg_restore",
        &[
            "libpq",
            "postgresql",
            "libpq@18",
            "postgresql@18",
            "libpq@17",
            "postgresql@17",
            "libpq@16",
            "postgresql@16",
            "postgresql@15",
            "postgresql@14",
        ],
    )
    .into_iter()
    .next()
    .context("pg_restore is required to inspect custom archives offline")?;
    let output = Command::new(reader)
        .args([mode, "--no-owner", "--no-privileges", "--file=-"])
        .arg(source)
        .stdout(std::fs::OpenOptions::new().write(true).open(&path)?)
        .stderr(Stdio::piped())
        .output()
        .context("pg_restore is required to inspect custom archives offline")?;
    if !output.status.success() {
        bail!(
            "offline pg_restore inspection failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok((guard, path))
}

fn schema(store: &Storage, record: &SnapshotRecord) -> Result<String> {
    let (_guard, path) = extract(store, record, "--schema-only")?;
    let lines = BufReader::new(std::fs::File::open(path)?)
        .lines()
        .collect::<std::io::Result<Vec<_>>>()?;
    let length = lines.len();
    // Normalize only pg_restore's outer generated envelope. Never strip
    // arbitrary comments/blank lines from SQL or stored-program bodies.
    Ok(lines
        .into_iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let header = index < 16
                && (line.starts_with("-- Dumped from database version")
                    || line.starts_with("-- Dumped by pg_dump version")
                    || line.starts_with("-- Started on ")
                    || line.starts_with("\\restrict "));
            let footer = index + 8 >= length
                && (line.starts_with("-- Completed on ") || line.starts_with("\\unrestrict "));
            (!header && !footer).then_some(line)
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn counts(reader: impl BufRead) -> Result<BTreeMap<String, u64>> {
    let mut result = BTreeMap::new();
    let mut table = None;
    let mut header = String::new();
    for line in reader.lines() {
        let line = line?;
        if let Some(name) = &table {
            if line == "\\." {
                table = None;
            } else {
                *result.get_mut(name).context("COPY table missing")? += 1;
            }
        } else if line.starts_with("COPY ") || !header.is_empty() {
            if !header.is_empty() {
                header.push('\n');
            }
            header.push_str(&line);
            if header.ends_with(" FROM stdin;") {
                let source = header
                    .strip_prefix("COPY ")
                    .context("unsupported COPY header")?;
                let name = copy_table(source)?;
                if result.insert(name.to_owned(), 0).is_some() {
                    bail!("duplicate COPY table in archive");
                }
                table = Some(name.to_owned());
                header.clear();
            }
        } else if line.starts_with("INSERT INTO ") || line.starts_with("COPY ") {
            bail!("archive diff requires standard COPY data");
        }
    }
    if table.is_some() || !header.is_empty() {
        bail!("truncated COPY data");
    }
    Ok(result)
}

fn copy_table(source: &str) -> Result<&str> {
    let bytes = source.as_bytes();
    let mut quoted = false;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            if quoted && bytes.get(index + 1) == Some(&b'"') {
                index += 2;
                continue;
            }
            quoted = !quoted;
        }
        if !quoted && bytes[index] == b' ' && bytes.get(index + 1) == Some(&b'(') {
            return Ok(&source[..index]);
        }
        index += 1;
    }
    bail!("unsupported COPY column list")
}

pub(super) fn compare(
    store: &Storage,
    left: &SnapshotRecord,
    right: &SnapshotRecord,
) -> Result<super::SchemaDiff> {
    let a_schema = schema(store, left)?;
    let b_schema = schema(store, right)?;
    let (_a_guard, a_path) = extract(store, left, "--data-only")?;
    let (_b_guard, b_path) = extract(store, right, "--data-only")?;
    let a = counts(BufReader::new(std::fs::File::open(a_path)?))?;
    let b = counts(BufReader::new(std::fs::File::open(b_path)?))?;
    let mut changes = Vec::new();
    let mut removed_tables = Vec::new();
    if a_schema != b_schema {
        changes
            .push("changed PostgreSQL archive schema (normalized pg_restore schema output)".into());
    }
    for (name, old) in &a {
        match b.get(name) {
            None => {
                removed_tables.push(name.clone());
                changes.push(format!("removed table {name}"));
            }
            Some(new) if new != old => changes.push(format!("table {name} rows: {old} -> {new}")),
            _ => {}
        }
    }
    for name in b.keys() {
        if !a.contains_key(name) {
            changes.push(format!("added table {name}"));
        }
    }
    if changes.is_empty() {
        changes.push("no archive schema or table row-count differences".into());
    }
    Ok(super::SchemaDiff {
        text: format!("{}\n(row contents and sequence values are not compared; schema changes are archive-wide; standard COPY archives only; SQL was not executed)", changes.join("\n")),
        removed_tables,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copy_rows_are_counted_without_interpreting_data() {
        let sql="COPY public.items (id, value) FROM stdin;\n1\tline\\nsecond\n2\tCOPY fake (id) FROM stdin;\n\\.\n";
        assert_eq!(counts(sql.as_bytes()).unwrap()["public.items"], 2);
        assert!(counts("COPY t (id) FROM stdin;\n1\n".as_bytes()).is_err());
        assert!(counts("INSERT INTO t VALUES (1);\n".as_bytes()).is_err());
        assert_eq!(
            copy_table("public.\"odd (table\" (\"column (name\") FROM stdin;").unwrap(),
            "public.\"odd (table\""
        );
    }
}
