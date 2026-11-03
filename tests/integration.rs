use anyhow::Result;
use dbsnap::engine::SaveMethod;
use dbsnap::ops;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn mcp_refuses_network_targets_before_creating_project_files() {
    for url in [
        "postgres://user@10.0.0.1/app",
        "postgres://user@192.168.1.5/app",
        "mysql://user@172.16.0.1/app",
        "mysql://user@host.docker.internal/app",
        "mysql://user@127.evil/app",
        "mysql://user@*/app",
    ] {
        let root = temp_root();
        assert!(ops::mcp_context_at(&root, Some(url)).is_err(), "{url}");
        assert!(!root.join(".dbsnap").exists(), "{url}");
    }
    for url in [
        "postgres://user@localhost/app",
        "postgres://user@127.0.0.1/app",
        "mysql://user@[::1]/app",
    ] {
        let db = dbsnap::config::parse_db_string(url, false).unwrap();
        assert!(dbsnap::config::validate_mcp_target(&db).is_ok(), "{url}");
    }
}

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

/// Thin wrappers so every test below stays readable despite the full ops
/// signatures (force / allow_remote / method).
fn init(root: &Path, db: &Path) -> Result<String> {
    ops::init_at(root, Some(db.to_str().unwrap()), false)
}

fn save(root: &Path, db: &Path, tag: &str) -> Result<String> {
    ops::save_at(
        root,
        Some(db.to_str().unwrap()),
        Some(tag.into()),
        false,
        false,
        SaveMethod::Clone,
    )
}

fn save_force(root: &Path, db: &Path, tag: &str) -> Result<String> {
    ops::save_at(
        root,
        Some(db.to_str().unwrap()),
        Some(tag.into()),
        true,
        false,
        SaveMethod::Clone,
    )
}

fn restore(root: &Path, db: &Path, tag: Option<&str>, safety: bool) -> Result<String> {
    ops::restore_at(
        root,
        Some(db.to_str().unwrap()),
        tag.map(str::to_string),
        safety,
        false,
        false,
    )
}

fn record_by_tag(root: &Path, tag: &str) -> dbsnap::storage::SnapshotRecord {
    let store = dbsnap::storage::Storage::find(root).unwrap();
    store.load_metadata().unwrap().find(Some(tag)).unwrap()
}

/// First chunk object found under .dbsnap/objects/ (for corruption tests).
fn first_object(root: &Path) -> PathBuf {
    fn walk(dir: &Path) -> Option<PathBuf> {
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if let Some(f) = walk(&p) {
                    return Some(f);
                }
            } else if p.is_file() {
                return Some(p);
            }
        }
        None
    }
    let store = dbsnap::storage::Storage::find(root).unwrap();
    walk(&store.objects_dir()).expect("at least one chunk object must exist")
}

#[test]
fn failed_safety_snapshot_aborts_sqlite_restore() -> Result<()> {
    use dbsnap::engine::{sqlite::SqliteEngine, DatabaseEngine};
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 2);
    init(&root, &db)?;
    save(&root, &db, "seed")?;
    let store = dbsnap::storage::Storage::find(&root).unwrap();
    let record = record_by_tag(&root, "seed");
    fs::rename(&db, root.join("original.db"))?;
    fs::create_dir(&db)?;
    let error = SqliteEngine::new(db.clone())
        .restore(&store, &record, Some("auto-before-restore-test"))
        .err()
        .expect("a failed safety snapshot must abort restore");
    assert!(format!("{error:#}").contains("safety snapshot failed"));
    assert!(db.is_dir(), "restore must not replace the live target");
    assert_eq!(count_rows(&root.join("original.db")), 2);
    Ok(())
}

#[test]
fn commands_find_project_database_from_subdirectory() -> Result<()> {
    let root = temp_root();
    let db = root.join("prisma/dev.db");
    create_db(&db, 3);
    init(&root, &db)?;
    let nested = root.join("src/controllers");
    fs::create_dir_all(&nested)?;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dbsnap"))
        .current_dir(&nested)
        .env_remove("DATABASE_URL")
        .arg("status")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("SQLite"));
    Ok(())
}

#[test]
fn json_commands_and_duplicate_tags() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 3);
    init(&root, &db)?;
    save(&root, &db, "before migration")?;
    save(&root, &db, "before migration")?;
    let error = restore(&root, &db, Some("before migration"), false).unwrap_err();
    assert!(error.to_string().contains("unambiguous snapshot ID"));
    let list: serde_json::Value = serde_json::from_str(&ops::list_json_at(&root, db.to_str())?)?;
    assert_eq!(list.as_array().unwrap().len(), 2);
    let status: serde_json::Value =
        serde_json::from_str(&ops::status_json_at(&root, db.to_str())?)?;
    assert_eq!(status["engine"], "sqlite");
    assert_eq!(status["snapshot_count"], 2);
    Ok(())
}

#[test]
fn mcp_snapshot_accepts_missing_tag() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 2);
    let ctx = ops::context_at(&root, db.to_str(), false, false)?;
    let response = dbsnap::mcp::handler::handle(
        &ctx,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "dbsnap_create_snapshot", "arguments": {} }
        }),
    )
    .unwrap();
    assert_eq!(response["result"]["isError"], false);
    assert!(response.to_string().contains("snap-"));
    Ok(())
}

#[test]
fn sqlite_snapshot_includes_uncheckpointed_wal_changes() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 1);
    let writer = Connection::open(&db)?;
    writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")?;
    init(&root, &db)?;
    save(&root, &db, "before")?;
    writer.execute("INSERT INTO users (name) VALUES ('new')", [])?;
    save(&root, &db, "after")?;
    writer.execute("DELETE FROM users WHERE name='new'", [])?;
    restore(&root, &db, Some("after"), false)?;
    assert_eq!(count_rows(&db), 2);
    Ok(())
}

#[test]
fn mcp_prune_rejects_zero_and_keeps_requested_snapshots() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 2);
    let ctx = ops::context_at(&root, db.to_str(), false, false)?;
    ops::save_with(&ctx, Some("one".into()), SaveMethod::Clone)?;
    ops::save_with(&ctx, Some("two".into()), SaveMethod::Clone)?;
    let call = |keep| {
        dbsnap::mcp::handler::handle_with_policy(
            &ctx,
            &serde_json::json!({
                "jsonrpc":"2.0", "id":1, "method":"tools/call",
                "params":{"name":"dbsnap_prune", "arguments":{"keep":keep}}
            }),
            true,
        )
        .unwrap()
    };
    assert_eq!(call(0)["result"]["isError"], true);
    assert_eq!(ctx.store.load_metadata()?.snapshots.len(), 2);
    assert_eq!(call(1)["result"]["isError"], false);
    assert_eq!(ctx.store.load_metadata()?.snapshots.len(), 1);
    Ok(())
}

#[test]
fn mcp_denies_destructive_operations_by_default() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 2);
    let ctx = ops::context_at(&root, db.to_str(), false, false)?;
    ops::save_with(&ctx, Some("one".into()), SaveMethod::Clone)?;
    add_rows(&db, 2, 3);
    for (name, args) in [
        (
            "dbsnap_restore_snapshot",
            serde_json::json!({"tag_or_id":"one"}),
        ),
        ("dbsnap_prune", serde_json::json!({"keep":1})),
    ] {
        let response = dbsnap::mcp::handler::handle(
            &ctx,
            &serde_json::json!({
                "jsonrpc":"2.0", "id":1, "method":"tools/call",
                "params":{"name":name, "arguments":args}
            }),
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], true);
    }
    assert_eq!(count_rows(&db), 3);
    assert_eq!(ctx.store.load_metadata()?.snapshots.len(), 1);
    Ok(())
}

#[test]
fn mcp_destructive_calls_require_explicit_user_elicitation() -> Result<()> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 1);
    init(&root, &db)?;
    save(&root, &db, "baseline")?;
    add_rows(&db, 1, 2);
    let mut child = Command::new(env!("CARGO_BIN_EXE_dbsnap"))
        .current_dir(&root)
        .args(["mcp", "--allow-destructive", "--db", db.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut read = || -> Result<serde_json::Value> {
        let mut line = String::new();
        if output.read_line(&mut line)? == 0 {
            anyhow::bail!("MCP server closed early");
        }
        Ok(serde_json::from_str(&line)?)
    };
    writeln!(
        input,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{"elicitation":{}}}})
    )?;
    input.flush()?;
    assert_eq!(read()?["id"], 1);
    for (id, accept) in [(2, false), (3, true)] {
        writeln!(
            input,
            "{}",
            serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"dbsnap_restore_snapshot","arguments":{"tag_or_id":"baseline"}}})
        )?;
        input.flush()?;
        let request = read()?;
        assert_eq!(request["method"], "elicitation/create");
        assert!(request["params"]["message"]
            .as_str()
            .unwrap()
            .contains("snapshot"));
        let answer = if accept {
            serde_json::json!({"action":"accept","content":{"confirm":true}})
        } else {
            serde_json::json!({"action":"decline"})
        };
        writeln!(
            input,
            "{}",
            serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":answer})
        )?;
        input.flush()?;
        let response = read()?;
        assert_eq!(response["id"], id);
        assert_eq!(response["result"]["isError"], !accept);
        assert_eq!(count_rows(&db), if accept { 1 } else { 2 });
    }
    let before_prune = dbsnap::storage::Storage::find(&root)
        .unwrap()
        .load_metadata()?
        .snapshots
        .len();
    writeln!(
        input,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"dbsnap_prune","arguments":{"keep":1}}})
    )?;
    input.flush()?;
    let approval = read()?;
    assert_eq!(approval["method"], "elicitation/create");
    writeln!(
        input,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":approval["id"],"result":{"action":"decline"}})
    )?;
    input.flush()?;
    assert_eq!(read()?["result"]["isError"], true);
    assert_eq!(
        dbsnap::storage::Storage::find(&root)
            .unwrap()
            .load_metadata()?
            .snapshots
            .len(),
        before_prune
    );
    drop(input);
    child.wait()?;
    let mut unsupported = Command::new(env!("CARGO_BIN_EXE_dbsnap"))
        .current_dir(&root)
        .args(["mcp", "--allow-destructive", "--db", db.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut input = unsupported.stdin.take().unwrap();
    writeln!(
        input,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}})
    )?;
    writeln!(
        input,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"dbsnap_prune","arguments":{"keep":1}}})
    )?;
    drop(input);
    let mut lines = BufReader::new(unsupported.stdout.take().unwrap()).lines();
    let _: serde_json::Value = serde_json::from_str(&lines.next().unwrap()?)?;
    let denied: serde_json::Value = serde_json::from_str(&lines.next().unwrap()?)?;
    assert_eq!(denied["id"], 5);
    assert_eq!(denied["result"]["isError"], true);
    unsupported.wait()?;
    Ok(())
}

#[test]
fn prune_aborts_when_surviving_manifest_is_unreadable() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 1);
    init(&root, &db)?;
    save(&root, &db, "old")?;
    add_rows(&db, 1, 4);
    save(&root, &db, "new")?;
    let store = dbsnap::storage::Storage::find(&root).unwrap();
    let old = record_by_tag(&root, "old");
    let new = record_by_tag(&root, "new");
    fs::write(store.snapshot_path(&new)?, b"broken")?;
    let unrelated = store.tmp_dir().join("unrelated.txt");
    fs::write(&unrelated, b"keep")?;
    let error = ops::prune_at(&root, db.to_str(), 1).unwrap_err();
    assert!(error.to_string().contains("cannot safely prune"));
    assert!(store.snapshot_path(&old)?.exists());
    assert!(unrelated.exists());
    assert_eq!(store.load_metadata()?.snapshots.len(), 2);
    Ok(())
}

#[test]
fn save_waits_for_project_lock() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 1);
    init(&root, &db)?;
    let ctx = ops::context_at(&root, db.to_str(), false, false)?;
    let lock = ctx.store.lock()?;
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        tx.send(ops::save_with(
            &ctx,
            Some("locked".into()),
            SaveMethod::Clone,
        ))
        .unwrap();
    });
    match rx.recv_timeout(std::time::Duration::from_millis(300)) {
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            drop(lock);
            rx.recv_timeout(std::time::Duration::from_secs(5))??;
        }
        Ok(Err(_)) => drop(lock), // Some platforms reject a second lock immediately.
        other => panic!("save must not succeed while the lock is held: {other:?}"),
    }
    worker.join().unwrap();
    Ok(())
}

#[test]
fn sqlite_diff_compares_schema_and_row_counts_without_changing_live_data() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 1);
    init(&root, &db)?;
    save(&root, &db, "before")?;
    add_rows(&db, 1, 3);
    Connection::open(&db)?.execute_batch("CREATE TABLE projects (id INTEGER PRIMARY KEY);")?;
    save(&root, &db, "after")?;
    let diff = ops::diff_at(&root, db.to_str(), "before", "after")?;
    assert!(diff.contains("table users rows: 1 -> 3"), "{diff}");
    assert!(diff.contains("added table projects"), "{diff}");
    assert!(diff.contains("row contents are not compared"));
    assert_eq!(count_rows(&db), 3);
    let ctx = ops::context_at(&root, db.to_str(), false, false)?;
    let response = dbsnap::mcp::handler::handle(
        &ctx,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "dbsnap_diff", "arguments": { "from": "before", "to": "after" } }
        }),
    )
    .unwrap();
    assert_eq!(response["result"]["isError"], false);
    assert!(response.to_string().contains("added table projects"));
    Ok(())
}

#[test]
fn pinned_snapshot_survives_prune_and_new_snapshot_is_not_pinned() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 1);
    init(&root, &db)?;
    save(&root, &db, "golden")?;
    ops::pin_at(&root, db.to_str(), "golden", true)?;
    save(&root, &db, "new")?;
    assert!(
        !record_by_tag(&root, "new").pinned,
        "linked snapshots must not inherit a pin"
    );
    ops::prune_at(&root, db.to_str(), 0)?;
    assert!(record_by_tag(&root, "golden").pinned);
    assert_eq!(
        dbsnap::storage::Storage::find(&root)
            .unwrap()
            .load_metadata()?
            .snapshots
            .len(),
        1
    );
    ops::pin_at(&root, db.to_str(), "golden", false)?;
    ops::prune_at(&root, db.to_str(), 0)?;
    assert!(dbsnap::storage::Storage::find(&root)
        .unwrap()
        .load_metadata()?
        .snapshots
        .is_empty());
    Ok(())
}

#[test]
fn save_restore_cycle() -> Result<()> {
    let root = temp_root();
    let db = root.join("prisma/dev.db");
    create_db(&db, 5);

    let out = init(&root, &db)?;
    assert!(out.contains("initialized"));
    assert!(
        out.contains("skill installed"),
        "init installs the agent skill"
    );

    save(&root, &db, "seed")?;

    add_rows(&db, 5, 15);
    assert_eq!(count_rows(&db), 15);

    // rollback = restore newest (seed) → data kembali 5 baris
    let out = restore(&root, &db, None, true)?;
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

    init(&root, &db)?;
    let first = save(&root, &db, "a")?;
    let second = save(&root, &db, "b")?;

    // DB tidak berubah → snapshot kedua harus di-link, bukan menulis ulang
    assert!(second.contains("linked"), "second: {second}");
    assert!(first.contains("compressed"));

    // satu manifest + satu chunk object — tidak ada file kedua
    let store = dbsnap::storage::Storage::find(&root).unwrap();
    let manifests = fs::read_dir(store.manifests_dir())?.count();
    let (obj_count, _) = count_objects(&store.objects_dir());
    assert_eq!(manifests, 1, "one manifest for two identical snapshots");
    assert_eq!(obj_count, 1, "one chunk for a 3-row database");
    Ok(())
}

fn count_objects(dir: &Path) -> (usize, u64) {
    let mut count = 0;
    let mut bytes = 0;
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                let (c, b) = count_objects(&p);
                count += c;
                bytes += b;
            } else {
                count += 1;
                bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    (count, bytes)
}

#[test]
fn reordered_manifest_is_rejected_before_restore() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 1);
    Connection::open(&db)?
        .execute_batch("CREATE TABLE big (v BLOB); INSERT INTO big VALUES (randomblob(180000));")?;
    init(&root, &db)?;
    save(&root, &db, "original")?;
    add_rows(&db, 1, 2);
    let store = dbsnap::storage::Storage::find(&root).unwrap();
    let record = record_by_tag(&root, "original");
    let path = store.snapshot_path(&record)?;
    let mut bytes = fs::read(&path)?;
    assert!(bytes.len() >= 92);
    for i in 0..32 {
        bytes.swap(28 + i, 60 + i);
    }
    fs::write(path, bytes)?;
    let error = restore(&root, &db, Some("original"), false).unwrap_err();
    assert!(format!("{error:#}").contains("manifest integrity"));
    assert_eq!(count_rows(&db), 2);
    Ok(())
}

#[test]
fn saving_unchanged_database_rejects_corrupt_existing_chunks() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 2);
    init(&root, &db)?;
    save(&root, &db, "first")?;
    fs::write(first_object(&root), b"corrupted")?;
    assert!(save(&root, &db, "second").is_err());
    let store = dbsnap::storage::Storage::find(&root).unwrap();
    assert_eq!(store.load_metadata()?.snapshots.len(), 1);
    Ok(())
}

#[test]
fn oversized_delta_chunk_is_rejected_before_allocation() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 1);
    init(&root, &db)?;
    let store = dbsnap::storage::Storage::find(&root).unwrap();
    let oversized = vec![42u8; dbsnap::storage::cas::CHUNK_SIZE + 1];
    let hash = blake3::hash(&oversized).to_hex().to_string();
    let path = store.checked_chunk_path(&hash)?;
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(path, zstd::stream::encode_all(&oversized[..], 3)?)?;
    let error = store.read_chunk_verified(&hash).unwrap_err();
    assert!(error.to_string().contains("chunk size limit"));
    Ok(())
}

#[test]
fn corrupt_delta_object_is_rejected_before_touching_db() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 4);
    init(&root, &db)?;
    save(&root, &db, "good")?;

    add_rows(&db, 4, 40);
    assert_eq!(count_rows(&db), 40);

    // korupsi satu chunk object
    let obj = first_object(&root);
    let mut bytes = fs::read(&obj)?;
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    fs::write(&obj, &bytes)?;

    let result = restore(&root, &db, None, false);
    assert!(result.is_err(), "corrupt chunk must fail restore");
    assert_eq!(
        count_rows(&db),
        40,
        "live db must be untouched after failed restore"
    );
    Ok(())
}

#[test]
fn prune_keeps_chunks_referenced_by_survivors() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 2);
    init(&root, &db)?;

    save(&root, &db, "one")?;
    // snapshot kedua identik → berbagi manifest + chunk dengan yang pertama
    save(&root, &db, "two")?;
    add_rows(&db, 2, 3);
    save(&root, &db, "three")?;

    // simpan dua terbaru → "one" dihapus, tapi manifest-nya dipakai "two"
    // dan chunk-nya dipakai "three" — semuanya harus tetap hidup
    ops::prune_at(&root, Some(db.to_str().unwrap()), 2)?;

    let store = dbsnap::storage::Storage::find(&root).unwrap();
    let meta = store.load_metadata()?;
    assert_eq!(meta.snapshots.len(), 2);

    // survivor masih bisa di-restore penuh
    restore(&root, &db, Some("two"), false)?;
    assert_eq!(count_rows(&db), 2, "restored from a shared manifest");
    Ok(())
}

#[test]
fn delta_only_writes_changed_chunks() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 0);
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE blobs (id INTEGER PRIMARY KEY, data TEXT);")
            .unwrap();
        for i in 0..64 {
            conn.execute(
                "INSERT INTO blobs (data) VALUES (?1)",
                [blob_text(1000 + i as u64)],
            )
            .unwrap();
        }
    }
    init(&root, &db)?;
    save(&root, &db, "v1")?;
    let v1 = record_by_tag(&root, "v1");

    // satu baris baru: hanya halaman pertama (change counter) + chunk terakhir
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute("INSERT INTO blobs (data) VALUES (?1)", [blob_text(9999)])
            .unwrap();
    }
    save(&root, &db, "v2")?;
    let v2 = record_by_tag(&root, "v2");

    assert!(
        v2.size_bytes < v1.size_bytes / 2,
        "delta snapshot must be far smaller: v1={} v2={}",
        v1.size_bytes,
        v2.size_bytes
    );

    // kedua versi tetap bisa di-restore
    restore(&root, &db, Some("v1"), false)?;
    assert_eq!(blob_rows(&db), 64);
    restore(&root, &db, Some("v2"), false)?;
    assert_eq!(blob_rows(&db), 65);
    Ok(())
}

fn blob_rows(path: &Path) -> i64 {
    let conn =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    conn.query_row("SELECT COUNT(*) FROM blobs", [], |r| r.get(0))
        .unwrap()
}

fn blob_text(seed: u64) -> String {
    let mut x = seed;
    (0..8192)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            char::from(b'a' + ((x >> 33) % 26) as u8)
        })
        .collect()
}

#[test]
fn legacy_file_snapshot_still_restorable() -> Result<()> {
    // v0.1 menyimpan snapshot sebagai satu file .zst — format lama harus
    // tetap bisa di-restore oleh engine delta.
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 5);
    init(&root, &db)?;

    let store = dbsnap::storage::Storage::find(&root).unwrap();
    let staged = store.tmp_dir().join("legacy-src.db");
    fs::copy(&db, &staged)?;
    let (original, compressed, hash) =
        dbsnap::storage::compress::compress_to(&staged, &store.snapshots_dir().join("leg001.zst"))?;
    fs::remove_file(&staged).ok();

    let mut meta = store.load_metadata()?;
    meta.snapshots.push(dbsnap::storage::SnapshotRecord {
        id: "leg001".into(),
        tag: "legacy".into(),
        created_at: chrono::Utc::now(),
        engine: "sqlite".into(),
        location: dbsnap::storage::Location::File {
            path: "snapshots/leg001.zst".into(),
        },
        size_bytes: compressed,
        original_bytes: original,
        blake3: hash,
        method: "sqlite-backup".into(),
        pinned: false,
        source_state: None,
    });
    store.write_metadata(&meta)?;

    add_rows(&db, 5, 12);
    restore(&root, &db, Some("legacy"), false)?;
    assert_eq!(count_rows(&db), 5, "legacy file snapshot restores exactly");
    Ok(())
}

#[test]
fn fingerprint_mismatch_blocks_without_force() -> Result<()> {
    let root = temp_root();
    let db_a = root.join("a.db");
    let db_b = root.join("b.db");
    create_db(&db_a, 1);
    create_db(&db_b, 1);

    init(&root, &db_a)?;
    let result = ops::save_at(
        &root,
        Some(db_b.to_str().unwrap()),
        Some("x".into()),
        false,
        false,
        SaveMethod::Clone,
    );
    assert!(result.is_err(), "mismatched target must be blocked");
    assert!(
        result.unwrap_err().to_string().contains("bound to"),
        "error should explain the binding"
    );

    // --force melewati guard
    save_force(&root, &db_b, "x")?;
    Ok(())
}

#[test]
fn catalog_cannot_be_rebound_to_another_database() -> Result<()> {
    let root = temp_root();
    let a = root.join("a.db");
    let b = root.join("b.db");
    create_db(&a, 1);
    create_db(&b, 3);
    init(&root, &a)?;
    save(&root, &a, "from-a")?;
    assert!(init(&root, &b).is_err());
    assert!(save_force(&root, &b, "wrong").is_err());
    assert!(ops::restore_at(&root, b.to_str(), Some("from-a".into()), true, true, false).is_err());
    assert_eq!(count_rows(&b), 3);
    assert_eq!(count_rows(&a), 1);
    Ok(())
}

#[test]
fn tampered_catalog_cannot_delete_files_outside_storage() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 1);
    init(&root, &db)?;
    save(&root, &db, "one")?;
    let target = root.join("important.txt");
    fs::write(&target, "keep")?;
    let store = dbsnap::storage::Storage::find(&root).unwrap();
    let path = store.root.join("metadata.json");
    let mut metadata: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    metadata["snapshots"][0]["location"] =
        serde_json::json!({"kind":"file", "path":"../important.txt"});
    fs::write(path, serde_json::to_vec(&metadata)?)?;
    assert!(ops::prune_at(&root, db.to_str(), 0).is_err());
    assert_eq!(fs::read_to_string(target)?, "keep");
    Ok(())
}

#[test]
fn sqlite_init_does_not_create_a_missing_database() -> Result<()> {
    let root = temp_root();
    let db = root.join("missing.sqlite");
    let result = init(&root, &db)?;
    assert!(result.contains("unreachable"));
    assert!(!db.exists());
    Ok(())
}

#[test]
fn missing_artifact_fails_without_touching_db() -> Result<()> {
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 1);
    init(&root, &db)?;
    save(&root, &db, "v1")?;

    // hapus artefak snapshot (manifest delta) → stage gagal sebelum
    // menyentuh DB
    let store = dbsnap::storage::Storage::find(&root).unwrap();
    let meta = store.load_metadata()?;
    fs::remove_file(store.snapshot_path(&meta.snapshots[0])?)?;
    let result = restore(&root, &db, None, false);
    assert!(result.is_err());
    assert_eq!(count_rows(&db), 1, "live db untouched");
    Ok(())
}

#[test]
fn engine_mismatch_is_rejected() -> Result<()> {
    // Snapshot SQLite tidak boleh di-restore ke database PostgreSQL, dan
    // sebaliknya — guard engine mismatch di ops::restore_with.
    let Some(url) = pg_test_url() else {
        return Ok(()); // PostgreSQL test server tidak tersedia
    };
    let root = temp_root();
    let db = root.join("dev.db");
    create_db(&db, 1);
    init(&root, &db)?;
    save(&root, &db, "sqlite-side")?;

    let result = ops::restore_at(
        &root,
        Some(&url),
        Some("sqlite-side".into()),
        false,
        true, // force: beda fingerprint memang disengaja di test ini
        false,
    );
    assert!(result.is_err(), "cross-engine restore must be rejected");
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("different database"),
        "error should explain the catalog binding"
    );
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
    assert!(
        responses[0].contains(env!("CARGO_PKG_VERSION")),
        "init: {}",
        responses[0]
    );
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

// ---------------------------------------------------------------------------
// PostgreSQL — dijalankan hanya bila DBSNAP_TEST_PG_URL menunjuk server uji
// lokal (CI: service container postgres:16; lokal: pg_ctl sendiri).
// ---------------------------------------------------------------------------

fn pg_test_url() -> Option<String> {
    match std::env::var("DBSNAP_TEST_PG_URL") {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => None,
    }
}

/// Setiap test PG mendapat database unik sendiri — test berjalan paralel
/// terhadap server yang sama dan tidak boleh saling menabrak.
fn pg_setup_test_db(test: &str) -> Result<Option<String>> {
    use std::str::FromStr;
    let Some(base) = pg_test_url() else {
        return Ok(None);
    };
    let mut parsed = url::Url::parse(&base)?;
    let cfg = postgres::Config::from_str(&base)?;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dbname = format!(
        "dbsnap_t{}_{}_{}",
        test.replace('-', "_"),
        std::process::id(),
        n
    );
    let mut admin = cfg.connect(postgres::NoTls)?;
    admin.batch_execute(&format!("DROP DATABASE IF EXISTS {dbname}"))?;
    admin.batch_execute(&format!("CREATE DATABASE {dbname}"))?;
    parsed.set_path(&format!("/{dbname}"));
    Ok(Some(parsed.to_string()))
}

fn pg_drop_test_db(url: &str) {
    use std::str::FromStr;
    let Ok(cfg) = postgres::Config::from_str(url) else {
        return;
    };
    let dbname = cfg.get_dbname().unwrap_or("").to_string();
    if let Ok(mut admin) = cfg.connect(postgres::NoTls) {
        admin
            .batch_execute(&format!("DROP DATABASE IF EXISTS {dbname};"))
            .ok();
    }
}

fn pg_exec(url: &str, sql: &str) -> Result<()> {
    use std::str::FromStr;
    let mut conn = postgres::Config::from_str(url)?.connect(postgres::NoTls)?;
    conn.batch_execute(sql)?;
    Ok(())
}

fn pg_count(url: &str) -> Result<i64> {
    use std::str::FromStr;
    let mut conn = postgres::Config::from_str(url)?.connect(postgres::NoTls)?;
    Ok(conn.query_one("SELECT COUNT(*) FROM items", &[])?.get(0))
}

#[test]
fn postgres_clone_roundtrip() -> Result<()> {
    let Some(url) = pg_setup_test_db("clone")? else {
        return Ok(());
    };
    let root = temp_root();
    pg_exec(
        &url,
        "DROP TABLE IF EXISTS items;
         CREATE TABLE items (id serial PRIMARY KEY, v text);
         INSERT INTO items (v) SELECT 'v' || g FROM generate_series(1, 5) g;",
    )?;

    init(&root, Path::new(&url))?;
    save(&root, Path::new(&url), "pgv1")?;
    let listing = ops::list_json_at(&root, Some(&url))?;
    assert!(listing.contains("metadata_fingerprint_not_content_checksum"));

    // "migrasi" destruktif
    pg_exec(&url, "ALTER TABLE items DROP COLUMN v;")?;

    // rollback: fence → safety clone → drop → rename → unfence
    let out = restore(&root, Path::new(&url), None, true)?;
    assert!(out.contains("restored database"));
    assert_eq!(pg_count(&url)?, 5, "clone restore must bring data back");

    // kolom v benar-benar kembali (fisik, bukan replay logis)
    use std::str::FromStr;
    let mut conn = postgres::Config::from_str(&url)?.connect(postgres::NoTls)?;
    let v: String = conn
        .query_one("SELECT v FROM items WHERE id = 1", &[])?
        .get(0);
    assert_eq!(v, "v1");
    drop(conn);
    pg_exec(&url, "DELETE FROM items;")?;
    restore(&root, Path::new(&url), Some("pgv1"), true)?;
    assert_eq!(pg_count(&url)?, 5, "the same snapshot must be reusable");

    // safety snapshot tersimpan sebagai klon server-side tercatat
    let store = dbsnap::storage::Storage::find(&root).unwrap();
    let meta = store.load_metadata()?;
    assert!(
        meta.snapshots
            .iter()
            .any(|s| matches!(s.location, dbsnap::storage::Location::PgDatabase { .. })),
        "restore must record the server-side safety clone"
    );
    Ok(())
}

#[test]
fn postgres_interrupted_rename_recovers_preserved_target() -> Result<()> {
    use std::str::FromStr;
    let Some(url) = pg_setup_test_db("recover")? else {
        return Ok(());
    };
    let root = temp_root();
    pg_exec(
        &url,
        "CREATE TABLE items (id integer); INSERT INTO items VALUES (1);",
    )?;
    init(&root, Path::new(&url))?;
    let target = dbsnap::config::parse_db_string(&url, false)?;
    let dbsnap::config::DbTarget::Postgres(pg) = &target.target else {
        unreachable!()
    };
    let previous = "__dbsnap_previous_a1b2c3";
    let store = dbsnap::storage::Storage::find(&root).unwrap();
    fs::write(
        store.root.join("postgres-restore.json"),
        serde_json::to_vec(&serde_json::json!({
            "version":1, "target":pg.display(), "previous":previous
        }))?,
    )?;
    let base = pg_test_url().unwrap();
    let mut admin = postgres::Config::from_str(&base)?.connect(postgres::NoTls)?;
    admin.batch_execute(&format!(
        "ALTER DATABASE \"{}\" ALLOW_CONNECTIONS false",
        pg.database
    ))?;
    admin.batch_execute(&format!(
        "ALTER DATABASE \"{}\" RENAME TO \"{}\"",
        pg.database, previous
    ))?;
    let status = ops::status_at(&root, Some(&url))?;
    assert!(status.contains("PostgreSQL"));
    assert_eq!(pg_count(&url)?, 1);
    assert!(!store.root.join("postgres-restore.json").exists());
    pg_drop_test_db(&url);
    Ok(())
}

#[test]
fn postgres_verified_tls_accepts_ca_and_rejects_wrong_hostname() -> Result<()> {
    let Ok(url) = std::env::var("DBSNAP_TEST_PG_TLS_URL") else {
        return Ok(());
    };
    let ca = std::env::var_os("DBSNAP_TEST_PG_TLS_CA").expect("TLS test CA is required");
    let root = temp_root();
    let run = |target: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_dbsnap"))
            .current_dir(&root)
            .env("DATABASE_URL", target)
            .env("DBSNAP_PG_TLS", "verify-full")
            .env("DBSNAP_PG_CA_CERT", &ca)
            .arg("status")
            .output()
    };
    let ok = run(&url)?;
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    // Use a separate project so fingerprint binding cannot mask TLS failure.
    let bad_root = temp_root();
    let bad_url = url.replace("localhost", "127.0.0.1");
    let bad = std::process::Command::new(env!("CARGO_BIN_EXE_dbsnap"))
        .current_dir(&bad_root)
        .env("DATABASE_URL", bad_url)
        .env("DBSNAP_PG_TLS", "verify-full")
        .env("DBSNAP_PG_CA_CERT", ca)
        .arg("status")
        .output()?;
    assert!(
        !bad.status.success(),
        "TLS must reject a certificate for another hostname"
    );
    Ok(())
}

#[test]
fn postgres_clone_process_interruption_is_recoverable_or_stops_safely() -> Result<()> {
    for phase in [
        "pg_clone_after_fence",
        "pg_clone_after_safety_snapshot",
        "pg_clone_after_previous_rename",
        "pg_clone_after_cutover",
    ] {
        let Some(url) = pg_setup_test_db(phase)? else {
            return Ok(());
        };
        let root = temp_root();
        pg_exec(
            &url,
            "CREATE TABLE items (id integer); INSERT INTO items VALUES (1);",
        )?;
        init(&root, Path::new(&url))?;
        save(&root, Path::new(&url), "baseline")?;
        pg_exec(&url, "INSERT INTO items VALUES (2);")?;
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_dbsnap"))
            .current_dir(&root)
            .env("DATABASE_URL", &url)
            .env("DBSNAP_TEST_FAILPOINT", phase)
            .args(["restore", "baseline"])
            .output()?;
        assert_eq!(
            result.status.code(),
            Some(86),
            "{phase}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let store = dbsnap::storage::Storage::find(&root).unwrap();
        if phase == "pg_clone_after_cutover" {
            let err = ops::status_at(&root, Some(&url)).unwrap_err();
            assert!(format!("{err:#}").contains("both target and preserved database"));
            assert!(store.root.join("postgres-restore.json").exists());
            assert_eq!(pg_count(&url)?, 1);
        } else {
            ops::status_at(&root, Some(&url))?;
            assert_eq!(pg_count(&url)?, 2, "{phase}");
            assert!(!store.root.join("postgres-restore.json").exists());
        }
    }
    Ok(())
}

#[test]
fn postgres_save_interruption_recovers_connection_fence() -> Result<()> {
    let Some(url) = pg_setup_test_db("savefence")? else {
        return Ok(());
    };
    let root = temp_root();
    pg_exec(
        &url,
        "CREATE TABLE items (id integer); INSERT INTO items VALUES (1);",
    )?;
    init(&root, Path::new(&url))?;
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_dbsnap"))
        .current_dir(&root)
        .env("DATABASE_URL", &url)
        .env("DBSNAP_TEST_FAILPOINT", "pg_save_after_fence")
        .args(["save", "interrupted"])
        .output()?;
    assert_eq!(
        result.status.code(),
        Some(86),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(pg_count(&url).is_err());
    ops::status_at(&root, Some(&url))?;
    assert_eq!(pg_count(&url)?, 1);
    Ok(())
}

#[test]
fn postgres_dump_roundtrip() -> Result<()> {
    let Some(url) = pg_setup_test_db("dump")? else {
        return Ok(());
    };
    // pg_dump/pg_restore harus ada di PATH
    if std::process::Command::new("pg_dump")
        .arg("--version")
        .output()
        .is_err()
    {
        return Ok(());
    }
    let root = temp_root();
    pg_exec(
        &url,
        "DROP TABLE IF EXISTS items;
         CREATE TABLE items (id serial PRIMARY KEY, v text);
         INSERT INTO items (v) SELECT 'v' || g FROM generate_series(1, 7) g;",
    )?;

    init(&root, Path::new(&url))?;
    ops::save_at(
        &root,
        Some(&url),
        Some("pgdump".into()),
        false,
        false,
        SaveMethod::Dump,
    )?;
    let rec = record_by_tag(&root, "pgdump");
    assert_eq!(rec.method, "pg-dump");
    assert!(rec.size_bytes > 0);

    pg_exec(&url, "DROP TABLE items;")?;
    assert!(pg_count(&url).is_err(), "table is gone");

    restore(&root, Path::new(&url), Some("pgdump"), false)?;
    assert_eq!(pg_count(&url)?, 7, "dump restore must recreate the table");
    pg_drop_test_db(&url);
    Ok(())
}

#[test]
fn postgres_dump_client_version_mismatch_is_rejected_before_save() -> Result<()> {
    use std::str::FromStr;
    let Some(url) = pg_setup_test_db("clientversion")? else {
        return Ok(());
    };
    let output = std::process::Command::new("pg_dump")
        .arg("--version")
        .output()?;
    let version = String::from_utf8_lossy(&output.stdout);
    let client_major = version
        .split_whitespace()
        .find_map(|w| w.split('.').next()?.parse::<u32>().ok())
        .unwrap();
    let mut conn = postgres::Config::from_str(&url)?.connect(postgres::NoTls)?;
    let number: String = conn.query_one("SHOW server_version_num", &[])?.get(0);
    if client_major == number.parse::<u32>()? / 10000 {
        return Ok(());
    }
    drop(conn);
    let root = temp_root();
    pg_exec(
        &url,
        "CREATE TABLE items (id integer); INSERT INTO items VALUES (1);",
    )?;
    init(&root, Path::new(&url))?;
    let error = ops::save_at(
        &root,
        Some(&url),
        Some("dump".into()),
        false,
        false,
        SaveMethod::Dump,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("does not match PostgreSQL server"));
    assert_eq!(pg_count(&url)?, 1);
    assert!(dbsnap::storage::Storage::find(&root)
        .unwrap()
        .load_metadata()?
        .snapshots
        .is_empty());
    pg_drop_test_db(&url);
    Ok(())
}

#[test]
fn postgres_failed_dump_replay_does_not_touch_target() -> Result<()> {
    let Some(url) = pg_setup_test_db("badreplay")? else {
        return Ok(());
    };
    let root = temp_root();
    pg_exec(
        &url,
        "CREATE TABLE items (id integer); INSERT INTO items VALUES (1);",
    )?;
    init(&root, Path::new(&url))?;
    ops::save_at(
        &root,
        Some(&url),
        Some("dump".into()),
        false,
        false,
        SaveMethod::Dump,
    )?;
    pg_exec(&url, "INSERT INTO items VALUES (2);")?;
    let store = dbsnap::storage::Storage::find(&root).unwrap();
    let mut metadata = store.load_metadata()?;
    let record = metadata
        .snapshots
        .iter_mut()
        .find(|r| r.tag == "dump")
        .unwrap();
    let path = store.snapshot_path(record)?;
    fs::write(&path, b"not a postgres archive")?;
    record.blake3 = dbsnap::storage::compress::hash_file(&path)?;
    store.write_metadata(&metadata)?;
    let error = restore(&root, Path::new(&url), Some("dump"), true).unwrap_err();
    assert!(format!("{error:#}").contains("target was not changed"));
    assert_eq!(pg_count(&url)?, 2);
    Ok(())
}

// ---------------------------------------------------------------------------
// MySQL/MariaDB — dijalankan hanya bila DBSNAP_TEST_MYSQL_URL menunjuk
// server uji lokal (CI: service mysql:8 / mariadb:11; lokal: server sendiri).
// Kontrol-plane memakai spawned client `mysql`, sesuai desain §2.3.
// ---------------------------------------------------------------------------

struct MyParts {
    host: String,
    port: String,
    user: String,
    password: String,
}

fn my_parts(url: &str) -> MyParts {
    let rest = url
        .strip_prefix("mysql://")
        .or_else(|| url.strip_prefix("mariadb://"))
        .unwrap_or(url);
    let (auth, hostport_db) = rest.split_once('@').unwrap_or(("", rest));
    let (user, password) = auth.split_once(':').unwrap_or((auth, ""));
    let (hostport, _db) = hostport_db.split_once('/').unwrap_or((hostport_db, ""));
    let (host, port) = hostport.split_once(':').unwrap_or((hostport, "3306"));
    MyParts {
        host: host.trim_matches('[').trim_matches(']').to_string(),
        port: port.to_string(),
        user: user.to_string(),
        password: password.to_string(),
    }
}

fn my_run(url: &str, db: Option<&str>, sql: &str) -> Result<String> {
    use std::process::Command;
    let p = my_parts(url);
    let mut cmd = Command::new("mysql");
    cmd.args([
        "-h", &p.host, "-P", &p.port, "-u", &p.user, "-N", "-B", "-e", sql,
    ])
    .env("MYSQL_PWD", &p.password);
    if let Some(db) = db {
        cmd.arg(db);
    }
    let out = cmd.output()?;
    if !out.status.success() {
        anyhow::bail!(
            "mysql failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn my_test_url() -> Option<String> {
    match std::env::var("DBSNAP_TEST_MYSQL_URL") {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => None,
    }
}

/// Setiap test MySQL mendapat database unik sendiri (test berjalan paralel
/// terhadap server yang sama).
fn my_setup_test_db(test: &str) -> Result<Option<String>> {
    let Some(base) = my_test_url() else {
        return Ok(None);
    };
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dbname = format!(
        "dbsnap_t{}_{}_{}",
        test.replace('-', "_"),
        std::process::id(),
        n
    );
    my_run(
        &base,
        None,
        &format!("DROP DATABASE IF EXISTS {dbname}; CREATE DATABASE {dbname};"),
    )?;
    let (prefix, _) = base
        .rsplit_once('/')
        .ok_or_else(|| anyhow::anyhow!("invalid MySQL test URL"))?;
    Ok(Some(format!("{prefix}/{dbname}")))
}

fn my_drop_test_db(url: &str) {
    if let Some(prefix) = url.rsplit_once('/').map(|(p, _)| p.to_string()) {
        let dbname = url.rsplit('/').next().unwrap_or("");
        let _ = my_run(&prefix, None, &format!("DROP DATABASE IF EXISTS {dbname};"));
    }
}

fn my_count(url: &str) -> Result<i64> {
    let db = url.rsplit('/').next().unwrap_or("");
    Ok(my_run(url, Some(db), "SELECT COUNT(*) FROM items")?
        .trim()
        .parse()
        .unwrap_or(-1))
}

#[test]
fn mysql_snapshot_is_saved_and_unsafe_restore_is_blocked() -> Result<()> {
    let Some(url) = my_setup_test_db("dump")? else {
        return Ok(());
    };
    let root = temp_root();
    let db = url.rsplit('/').next().unwrap_or("").to_string();

    my_exec_url(
        &url,
        Some(&db),
        "DROP TABLE IF EXISTS items;
         CREATE TABLE items (id INT PRIMARY KEY, v TEXT);
         INSERT INTO items VALUES (1,'a'),(2,'b'),(3,'c'),(4,'d'),(5,'e');",
    )?;

    init(&root, Path::new(&url))?;
    save(&root, Path::new(&url), "myv1")?;
    let rec = record_by_tag(&root, "myv1");
    assert_eq!(rec.method, "mysql-dump");

    // migrasi destruktif: kolom v hilang bersama datanya
    my_exec_url(&url, Some(&db), "ALTER TABLE items DROP COLUMN v;")?;

    let error = restore(&root, Path::new(&url), Some("myv1"), true).unwrap_err();
    assert!(format!("{error:#}").contains("temporarily disabled"));
    assert_eq!(my_count(&url)?, 5, "blocked restore must not alter target");
    assert!(my_run(&url, Some(&db), "SELECT v FROM items").is_err());
    let store = dbsnap::storage::Storage::find(&root).unwrap();
    let meta = store.load_metadata()?;
    assert_eq!(meta.snapshots.len(), 1, "snapshot remains available");

    my_drop_test_db(&url);
    Ok(())
}

/// Helper dengan URL (password via MYSQL_PWD di dalam my_run) — dipakai
/// setup di atas.
fn my_exec_url(url: &str, db: Option<&str>, sql: &str) -> Result<()> {
    my_run(url, db, sql).map(|_| ())
}

#[test]
fn credentials_never_leak_into_output_or_metadata() -> Result<()> {
    // Dijalankan terhadap MySQL uji yang password-nya ada di URL —
    // password TIDAK boleh muncul di output CLI, MCP, maupun file .dbsnap.
    let Some(url) = my_test_url() else {
        return Ok(());
    };
    let password = my_parts(&url).password;
    if password.is_empty() {
        return Ok(()); // server uji tanpa password — tidak ada yang diuji
    }
    let root = temp_root();

    let init_out = init(&root, Path::new(&url))?;
    save(&root, Path::new(&url), "leak-check")?;
    let list_out = ops::list_at(&root, Some(&url))?;
    let status_out = ops::status_at(&root, Some(&url))?;

    for (label, text) in [
        ("init output", init_out),
        ("list output", list_out),
        ("status output", status_out),
    ] {
        assert!(
            !text.contains(&password),
            "{label} must not contain the database password: {text}"
        );
    }

    // A password appearing in the database name makes a substring test
    // ambiguous; require an unambiguous test configuration instead of skipping.
    let dbname_contains_password = url
        .rsplit('/')
        .next()
        .map(|d| d.contains(&password))
        .unwrap_or(false);
    assert!(
        !dbname_contains_password,
        "test database name must not contain the password"
    );
    let store = dbsnap::storage::Storage::find(&root).unwrap();
    for file in ["config.json", "metadata.json"] {
        let raw = fs::read_to_string(store.root.join(file))?;
        assert!(
            !raw.contains(&password),
            "{file} must never contain the database password"
        );
    }
    Ok(())
}
