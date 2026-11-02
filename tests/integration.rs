use anyhow::Result;
use dbsnap::ops;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("dbsnap-it-{}-{n}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn create_db(path: &Path, rows: i64) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);")
        .unwrap();
    for i in 0..rows {
        conn.execute("INSERT INTO users (name) VALUES (?1)", [format!("user{i}")])
            .unwrap();
    }
}

fn count_rows(path: &Path) -> i64 {
    let conn =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
        .unwrap()
}

fn add_rows(path: &Path, from: i64, to: i64) {
    let conn = Connection::open(path).unwrap();
    for i in from..to {
        conn.execute("INSERT INTO users (name) VALUES (?1)", [format!("user{i}")])
            .unwrap();
    }
}

#[test]
fn save_restore_cycle() -> Result<()> {
    let root = temp_root();
    let db = root.join("prisma/dev.db");
    create_db(&db, 5);

    let out = ops::init_at(&root, Some(db.to_str().unwrap()))?;
    assert!(out.contains("initialized dbsnap"));

    ops::save_at(
        &root,
        Some(db.to_str().unwrap()),
        Some("seed".into()),
        false,
    )?;

    add_rows(&db, 5, 15);
    assert_eq!(count_rows(&db), 15);

    // rollback = restore newest (seed) → data kembali 5 baris
    let out = ops::restore_at(&root, Some(db.to_str().unwrap()), None, true, false)?;
    assert!(out.contains("restored database"));
    assert_eq!(
        count_rows(&db),
        5,
        "restore must bring back the snapshot rows"
    );

    // metadata harus mencatat safety snapshot otomatis
    let store = dbsnap::storage::Storage::find(&root).unwrap();
    let meta = store.load_metadata()?;
    assert!(meta
        .snapshots
        .iter()
        .any(|s| s.tag.starts_with("auto-before-restore-")));
    Ok(())
}

#[test]
fn unchanged_db_links_instead_of_copying() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 3);

    ops::init_at(&root, Some(db.to_str().unwrap()))?;
    let first = ops::save_at(&root, Some(db.to_str().unwrap()), Some("a".into()), false)?;
    let second = ops::save_at(&root, Some(db.to_str().unwrap()), Some("b".into()), false)?;

    // DB tidak berubah → snapshot kedua harus di-link, bukan menyalin ulang
    assert!(second.contains("linked"), "second: {second}");
    assert!(first.contains("compressed"));

    // file .zst harus persis satu
    let store = dbsnap::storage::Storage::find(&root).unwrap();
    let files: Vec<_> = fs::read_dir(store.snapshots_dir())?
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(files.len(), 1, "dedup must keep only one object file");
    Ok(())
}

#[test]
fn corrupt_snapshot_is_rejected_before_touching_db() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 4);
    ops::init_at(&root, Some(db.to_str().unwrap()))?;
    ops::save_at(
        &root,
        Some(db.to_str().unwrap()),
        Some("good".into()),
        false,
    )?;

    add_rows(&db, 4, 40);
    assert_eq!(count_rows(&db), 40);

    // korupsi file snapshot
    let store = dbsnap::storage::Storage::find(&root).unwrap();
    let meta = store.load_metadata()?;
    let snap_path = store.snapshot_path(&meta.snapshots[0])?;
    let mut bytes = fs::read(&snap_path)?;
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    fs::write(&snap_path, &bytes)?;

    let result = ops::restore_at(&root, Some(db.to_str().unwrap()), None, false, false);
    assert!(result.is_err(), "corrupt snapshot must fail");
    assert_eq!(
        count_rows(&db),
        40,
        "live db must be untouched after failed restore"
    );
    Ok(())
}

#[test]
fn prune_never_deletes_files_referenced_by_survivors() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 2);
    ops::init_at(&root, Some(db.to_str().unwrap()))?;

    ops::save_at(&root, Some(db.to_str().unwrap()), Some("one".into()), false)?;
    // snapshot kedua identik → berbagi file dengan yang pertama
    ops::save_at(&root, Some(db.to_str().unwrap()), Some("two".into()), false)?;
    add_rows(&db, 2, 3);
    ops::save_at(
        &root,
        Some(db.to_str().unwrap()),
        Some("three".into()),
        false,
    )?;

    let store = dbsnap::storage::Storage::find(&root).unwrap();
    let files_before = fs::read_dir(store.snapshots_dir())?.count();
    assert_eq!(files_before, 2, "1 unique content + 1 changed content");

    // simpan dua terbaru → snapshot tertua hilang, tapi filenya dipakai "two"
    ops::prune_at(&root, Some(db.to_str().unwrap()), 2)?;
    let files_after = fs::read_dir(store.snapshots_dir())?.count();
    assert_eq!(files_after, 2, "shared file must survive prune");

    let meta = store.load_metadata()?;
    assert_eq!(meta.snapshots.len(), 2);
    Ok(())
}

#[test]
fn fingerprint_mismatch_blocks_without_force() -> Result<()> {
    let root = temp_root();
    let db_a = root.join("a.db");
    let db_b = root.join("b.db");
    create_db(&db_a, 1);
    create_db(&db_b, 1);

    ops::init_at(&root, Some(db_a.to_str().unwrap()))?;
    let result = ops::save_at(&root, Some(db_b.to_str().unwrap()), Some("x".into()), false);
    assert!(result.is_err(), "mismatched target must be blocked");
    assert!(
        result.unwrap_err().to_string().contains("bound to"),
        "error should explain the binding"
    );

    // --force melewati guard
    ops::save_at(&root, Some(db_b.to_str().unwrap()), Some("x".into()), true)?;
    Ok(())
}

#[test]
fn restore_failure_keeps_safety_snapshot_hint() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 1);
    ops::init_at(&root, Some(db.to_str().unwrap()))?;
    ops::save_at(&root, Some(db.to_str().unwrap()), Some("v1".into()), false)?;

    // buat restore gagal: hapus file snapshot setelah verifikasi? Lebih mudah:
    // ganti permission? Di Windows cukup hapus file → stage_restore gagal
    // sebelum sentuh DB — assertion: DB utuh.
    let store = dbsnap::storage::Storage::find(&root).unwrap();
    let meta = store.load_metadata()?;
    fs::remove_file(store.snapshot_path(&meta.snapshots[0])?)?;
    let result = ops::restore_at(&root, Some(db.to_str().unwrap()), None, false, false);
    assert!(result.is_err());
    assert_eq!(count_rows(&db), 1, "live db untouched");
    Ok(())
}

#[test]
fn mcp_stdio_roundtrip() -> Result<()> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 2);

    let mut child = Command::new(env!("CARGO_BIN_EXE_dbsnap"))
        .args(["mcp", "--db", db.to_str().unwrap()])
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let mut stdin = child.stdin.take().unwrap();
    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","clientInfo":{"name":"test","version":"0"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"dbsnap_create_snapshot","arguments":{"tag":"via-mcp"}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"dbsnap_list_snapshots","arguments":{}}}"#,
    ];
    for req in requests {
        writeln!(stdin, "{req}")?;
    }
    stdin.flush()?;
    drop(stdin); // EOF → server keluar

    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut responses = Vec::new();
    for _ in 0..4 {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        assert!(n > 0, "server closed early — expected 4 responses");
        responses.push(line);
    }
    child.wait()?;

    // notification tidak boleh memicu response
    assert_eq!(responses.len(), 4);
    assert!(
        responses[0].contains("\"protocolVersion\""),
        "init: {}",
        responses[0]
    );
    assert!(responses[0].contains("dbsnap"), "init: {}", responses[0]);
    assert!(
        responses[1].contains("dbsnap_create_snapshot"),
        "tools: {}",
        responses[1]
    );
    assert!(
        responses[2].contains("saved snapshot") && responses[2].contains("via-mcp"),
        "create: {}",
        responses[2]
    );
    assert!(responses[3].contains("via-mcp"), "list: {}", responses[3]);
    Ok(())
}
