use anyhow::Result;
use dbsnap::engine::SaveMethod;
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
fn save_restore_cycle() -> Result<()> {
    let root = temp_root();
    let db = root.join("prisma/dev.db");
    create_db(&db, 5);

    let out = init(&root, &db)?;
    assert!(out.contains("initialized dbsnap"));
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
        result.unwrap_err().to_string().contains("engine mismatch"),
        "error should explain the engine mismatch"
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
fn pg_setup_test_db(test: &str) -> Option<String> {
    use std::str::FromStr;
    let base = pg_test_url()?;
    let mut parsed = url::Url::parse(&base).ok()?;
    let cfg = postgres::Config::from_str(&base).ok()?;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dbname = format!(
        "dbsnap_t{}_{}_{}",
        test.replace('-', "_"),
        std::process::id(),
        n
    );
    let mut admin = cfg.connect(postgres::NoTls).ok()?;
    admin
        .batch_execute(&format!(
            "DROP DATABASE IF EXISTS {dbname}; CREATE DATABASE {dbname};"
        ))
        .ok()?;
    parsed.set_path(&format!("/{dbname}"));
    Some(parsed.to_string())
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
    let Some(url) = pg_setup_test_db("clone") else {
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
fn postgres_dump_roundtrip() -> Result<()> {
    let Some(url) = pg_setup_test_db("dump") else {
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
