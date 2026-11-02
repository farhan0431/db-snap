# db-snap — Desain Arsitektur Multi-Engine (SQLite · PostgreSQL · MySQL/MariaDB)

> Lanjutan dari `rancangan_arsitektur_dbsnap.md`. Dokumen ini mendefinisikan desain lintas-engine,
> threat model keamanan, strategi stabilitas, dan target performa.
> Prinsip nomor satu: **Do No Harm** — alat ini boleh gagal, tapi tidak boleh menghancurkan data yang tidak boleh ia sentuh.

---

## 1. Fakta Fisik Tiap Engine (dasar semua keputusan)

Ketiga engine punya karakteristik penyimpanan yang berbeda fundamental. Desain yang benar harus mengikuti fisiknya, bukan memaksakan satu metode ke semua:

| | SQLite | PostgreSQL | MySQL / MariaDB |
| :--- | :--- | :--- | :--- |
| Model proses | Embedded (satu file) | Server + cluster datadir | Server + datadir |
| Salinan blok-level saat server hidup | ✅ Online Backup API | ✅ `CREATE DATABASE ... TEMPLATE` (server-side) | ❌ tidak ada API-nya |
| Dump logis konsisten tanpa lock | ✅ (via backup) | ✅ `pg_dump -Fc` | ✅ `mysqldump --single-transaction` (InnoDB) |
| Restore panas (server dev masih jalan) | ✅ Backup API + WAL checkpoint | ✅ drop+rename (butuh fencing ~1 dtk) | ✅ drop+replay dump |
| Batas bawah latensi realistis | **< 10 ms** (skala MB) | **ms–detik** (salinan blok) | **detik** (dump logis) |

**Kesimpulan desain:**
- **SQLite** → snapshot fisik file, semuanya lokal, tercepat. (v0.1 — sudah final.)
- **PostgreSQL** → snapshot *server-side* berupa database tersembunyi ber-prefix `__dbsnap_` (salinan blok, cepat) + mode `--method dump` (`pg_dump -Fc`) untuk artefak portabel di disk. (v0.2)
- **MySQL/MariaDB** → tidak ada jalur fisik; snapshot = dump logis konsisten via `mysqldump`/`mariadb-dump`. Jujur di README: detik, bukan milidetik. (v0.3)

---

## 2. Desain Per-Engine

### 2.1 SQLite (v0.1, tidak berubah dari rancangan awal)

- Save: `rusqlite::backup::Backup` → stream zstd level 3 → hash blake3, chunk 64 KB, memori konstan.
- Restore: verifikasi hash dulu (decompress ke `.dbsnap/tmp/`), baru Backup API balik ke DB hidup + `PRAGMA wal_checkpoint(TRUNCATE);`.
- Penyimpanan hasil save: v0.1 full-copy ke `snapshots/<id>.zst`; sejak v0.2 manifest delta CAS (§3.5) supaya latensi & disk tidak tumbuh linear dengan ukuran DB.
- Alasan Backup API (bukan salin file biasa): **"Online" di sini bukan internet/cloud** — istilah resmi SQLite untuk backup saat DB sedang dipakai; fungsinya ada di dalam library SQLite itu sendiri (di-bundle via `rusqlite`, 100% lokal & offline). Salin file biasa gagal tiga arah: (1) mode WAL — data terbaru ada di file `-wal`, bukan `dev.db`; (2) torn copy saat server menulis di tengah proses; (3) Windows tidak bisa menimpa file terbuka, dan trik rename di Linux membuat server yang hidup terus membaca inode lama (restore "sukses" tapi aplikasi tidak melihat datanya). Backup API satu-satunya jalur save & restore panas yang benar di semua OS — `VACUUM INTO` (alternatif lokal) hanya menyelesaikan sisi save dan lebih lambat.

### 2.2 PostgreSQL (sejak v0.2.0)

**Konsep:** snapshot disimpan sebagai database tersembunyi *di dalam server Postgres yang sama* — salinan blok level storage, bukan dump.

**Save (default `--method clone`):**
```sql
-- 1. Fencing: tolak koneksi baru, putus koneksi aktif (urutan penting)
ALTER DATABASE dev_db WITH ALLOW_CONNECTIONS false;
SELECT pg_terminate_backend(pid) FROM pg_stat_activity
 WHERE datname = 'dev_db' AND pid <> pg_backend_pid();
-- 2. Salinan blok (cepat, level file server)
CREATE DATABASE __dbsnap_a1b2c3 TEMPLATE dev_db;
-- 3. Selalu: buka lagi koneksi (dijamin via guard finally + healing, lihat §5)
ALTER DATABASE dev_db WITH ALLOW_CONNECTIONS true;
```
- Disrupsi: blackout koneksi ~1 detik saat salinan; ORM (Prisma/Drizzle/SQLx) reconnect otomatis. Diunggahkan jujur di README.
- `ALTER DATABASE ... ALLOW_CONNECTIONS` & `pg_terminate_backend()` butuh hak pemilik/superuser — normal untuk dev lokal.
- Sifat: snapshot hidup di server → **hilang jika volume/container Postgres dibuang**. Untuk artefak tahan-lama: `dbsnap save --method dump` menjalankan `pg_dump -Fc` ke `.dbsnap/snapshots/<id>.dump.zst` (portabel, bisa dipindah antar mesin).

**Restore (satu jendela fencing saja):**
```sql
ALTER DATABASE dev_db WITH ALLOW_CONNECTIONS false;
SELECT pg_terminate_backend(...);
CREATE DATABASE __dbsnap_auto_<ts> TEMPLATE dev_db;   -- safety snapshot otomatis (§4-T7)
DROP DATABASE dev_db;
ALTER DATABASE __dbsnap_a1b2c3 RENAME TO dev_db;       -- rename = instan
ALTER DATABASE dev_db WITH ALLOW_CONNECTIONS true;
```
- Restore dari `--method dump`: `pg_restore -d postgres --create --clean` (lebih lambat, hanya dipakai bila snapshot clone sudah tidak ada di server).

**Aturan keras PostgreSQL:**
1. Hanya pernah membuat/mengubah/menghapus database ber-prefix `__dbsnap_` atau DB target yang fingerprint-nya cocok (§4-T2). **Tidak pernah** menyentuh database lain, `template0`, `template1`, `postgres`.
2. `ALLOW_CONNECTIONS true` dijalankan idempoten di **awal dan akhir** setiap operasi PG terhadap DB target — jika proses mati di tengah fencing, operasi berikutnya menyelamatkannya (healing). `dbsnap status` menampilkan DB yang masih ter-fencing.
3. Kontrol-plane SQL (create/drop/rename/terminate, baca `pg_database`) memakai **`tokio-postgres`** (pure Rust, tanpa libpq). `pg_dump`/`pg_restore` hanya untuk mode dump — dispawn tanpa shell, argumen lewat argv.

### 2.3 MySQL / MariaDB (v0.3)

**Konsep:** tidak ada jalur fisik → dump logis konsisten. Tool ini memanggil `mysqldump`/`mysql` (atau `mariadb-dump`/`mariadb`) yang di-spawn tanpa shell.

**Deteksi flavor (wajib — flag keduanya tidak identik):**
- Baca `SELECT VERSION()` → string berisi "MariaDB" → prefer `mariadb-dump`/`mariadb`, fallback `mysqldump`/`mysql`; sebaliknya untuk MySQL.
- Flag set dikurasi per flavor + mekanisme **retry dengan fallback set** untuk error klasik yang dikenal:
  - MySQL 8 client vs server < 8: error `column_statistics` → retry tanpa `--column-statistics`.
  - `--set-gtid-purged=OFF` hanya untuk MySQL; mariadb-dump tidak mengenalinya → tidak dikirim ke MariaDB.
  - `--no-tablespaces` (hindari butuh privilege PROCESS di MySQL 8) — aman untuk dev.

**Save:**
```bash
mysqldump --single-transaction --routines --triggers --events \
          --hex-blob --no-tablespaces --databases myapp_dev \
  | zstd | tulis .dbsnap/snapshots/<id>.sql.zst  (hash blake3 sambil streaming)
```
- `--single-transaction` = konsisten untuk InnoDB **tanpa menahan writer** (tidak mengganggu dev server).
- Keterbatasan diunggahkan: MyISAM tidak terjamin konsistensinya dengan `--single-transaction` (opsi `--lock-all-tables` tersedia via `--dump-extra-args`).
- Host `localhost` **selalu diterjemahkan ke `127.0.0.1`** untuk klien TCP — di Linux klien `mysql` dengan host `localhost` mencoba unix socket dan sering gagal di setup Docker. (Gotcha nyata.)

**Restore (berurutan, stop pada error pertama — tanpa `--force`):**
1. Verifikasi hash snapshot (decompress ke `.dbsnap/tmp/`).
2. Safety snapshot otomatis dari kondisi sekarang (§4-T7).
3. Catat `DEFAULT_CHARACTER_SET_NAME`/`DEFAULT_COLLATION_NAME` dari `information_schema.SCHEMATA`.
4. `DROP DATABASE` → buat ulang dengan charset/collation yang dicatat → replay dump.
5. Bila replay gagal di tengah: pesan error **selalu** menyertakan perintah rollback satu-baris ke safety snapshot.

**Prasyarat pengguna:** `mysqldump` + `mysql` (atau varian mariadb) ada di PATH — mesin dev dengan MySQL praktis selalu memilikinya. Dicek di `dbsnap init` dengan pesan yang jelas bila absen.

---

## 3. Arsitektur Bersama (supaya tiga engine menyatu rapi)

### 3.1 Trait `DatabaseEngine` (final)

```rust
#[async_trait::async_trait]
pub trait DatabaseEngine: Send + Sync {
    /// Deteksi versi/flavor server, ukuran data, kesehatan koneksi.
    async fn probe(&self) -> Result<EngineInfo>;
    /// Buat snapshot; mengembalikan record metadata (lokasi, hash, ukuran).
    async fn save(&self, opts: &SaveOpts) -> Result<SnapshotRecord>;
    /// Restore + safety snapshot otomatis + verifikasi integritas.
    async fn restore(&self, snap: &SnapshotRecord, opts: &RestoreOpts) -> Result<()>;
    /// Hapus artefak milik dbsnap (file .zst / DROP DATABASE __dbsnap_*).
    async fn destroy(&self, snap: &SnapshotRecord) -> Result<()>;
    /// Kapabilitas untuk UI/validasi: hot_safe, speed_class, artifact_kind.
    fn capabilities(&self) -> Capabilities;
}
```
- Registry engine dipilih dari **skema URL** `DATABASE_URL`: `file:`/path → SQLite, `postgres://`/`postgresql://` → PG, `mysql://`/`mariadb://` → MySQL. Dirancang sejak v0.1 agar v0.2/v0.3 tinggal menambah implementasi.
- `storage/` tetap engine-agnostic: `location` dalam metadata menentukan di mana snapshot hidup.

### 3.2 Skema `metadata.json` v2

```json
{
  "version": 2,
  "project_fingerprint": { "engine": "postgres", "host": "127.0.0.1",
                            "port": 5432, "database": "myapp_dev" },
  "snapshots": [
    {
      "id": "a1b2c3",
      "tag": "before-auth-refactor",
      "created_at": "2026-11-02T09:15:00Z",
      "engine": "postgres",
      "location": { "kind": "pg_database", "name": "__dbsnap_a1b2c3" },
      "size_bytes": 268435456,
      "blake3": "8f9d2a1c...",
      "method": "clone"
    }
  ]
}
```
- `location.kind`: `"file"` (SQLite/MySQL/PG-dump — path relatif `.dbsnap/`), `"manifest"` (delta CAS, sejak v0.1.5), atau `"pg_database"` (server-side).
- **Tidak ada kredensial yang pernah disimpan** — fingerprint hanya engine+host+port+database (SQLite: path absolut kanonikal).
- Penulisan metadata selalu *atomic*: tulis `.dbsnap/tmp/metadata.json.tmp` → `rename` (anti-torn, §4-T4).

### 3.3 Layout `.dbsnap/` final

```text
.dbsnap/
├── config.json        # fingerprint proyek (dibuat saat init, diverifikasi tiap perintah)
├── metadata.json
├── lock               # file lock fs4 — semua operasi harus memegangnya
├── tmp/               # decompress/verify sebelum menyentuh DB; dibersihkan prune
├── snapshots/
│   └── <id>.zst       # mode full-copy (v0.1): NAMA FILE HANYA ID — tag hidup di metadata (§4-T9)
├── manifests/
│   └── <id>.m         # mode delta (v0.1.5+, §3.5): daftar hash chunk, binari
└── objects/
    └── <hash[0:2]>/<hash>   # chunk 64KB ter-zstd — content-addressed, dibagi antar snapshot
```

### 3.4 Tambahan Cargo (di atas deps v0.1)

| Crate | Fase | Alasan |
| :--- | :--- | :--- |
| `url` | v0.1 | Parsing aman `DATABASE_URL` semua skema |
| `fs4` | v0.1 | File lock lintas-platform + cek ruang disk bebas |
| `async-trait` | v0.2 | Trait async untuk engine |
| `tokio-postgres` | v0.2 | Kontrol-plane PG pure Rust (tanpa libpq) |
| — | v0.3 | MySQL via spawned clients, tanpa crate tambahan (binary tetap kecil) |

Tokio disempitkan dari `features = ["full"]` menjadi `["rt-multi-thread", "macros", "io-std", "process", "time", "fs"]` — memangkas ukuran & waktu kompilasi (§6).

---

### 3.5 Mesin Snapshot Delta — Content-Addressed Storage (CAS) [sejak v0.1.5]

**Masalah:** full-copy per snapshot membuat latensi save dan pemakaian disk tumbuh linear terhadap ukuran DB (DB 1 GB × 20 snapshot ≈ +5 GB, tiap save 3–5 detik).

**Kunci yang membuat solusinya hampir sempurna khusus SQLite:** SQLite menulis halaman 4KB **secara in-place pada offset tetap** — data tidak pernah bergeser. Artinya chunking *fixed page-aligned* (chunk 64 KB = 16 halaman) menghasilkan dedup hampir sempurna **tanpa rolling hash / CDC** (CDC seperti FastCDC yang dipakai restic/borg hanya diperlukan bila data bergeser — kasusnya dump teks MySQL, §2.3).

**Mekanisme:**
- **Objek store:** `.dbsnap/objects/<hash[0:2]>/<hash>` — tiap chunk 64 KB disimpan ter-zstd, alamatnya blake3-nya sendiri. Snapshot = **manifest** kecil (`.dbsnap/manifests/<id>.m`): header + daftar hash chunk berurutan (DB 1 GB ≈ 16 ribu hash ≈ 512 KB).
- **Save:** Backup API → file temp → baca sekali sambil blake3 per chunk → **hanya chunk yang belum ada di store yang ditulis** (zstd hanya di chunk baru — CPU kompresi juga turun drastis).
- **Fast path DB tidak berubah:** `PRAGMA data_version` + ukuran + mtime identik dengan snapshot terakhir → pakai ulang manifest, 0 I/O signifikan.
- **Restore:** kumpulkan chunk sesuai manifest → tulis ke temp sambil **verifikasi hash per chunk** → Backup API ke DB hidup. Verifikasi integritas T3 menjadi gratis (tidak perlu hash file terpisah).
- **Prune/GC:** refcount antar manifest; objek yang tidak lagi dirujuk snapshot mana pun dihapus. Prune tetap hanya menyentuh objek milik dbsnap (T13).

**Angka yang dituju (DB 1 GB, migrasi mengubah 1–5% halaman, NVMe):**

| | Full-copy (v0.1) | Delta CAS (v0.2+) |
| :--- | :--- | :--- |
| Latensi save | ~3–5 dtk (zstd 1 GB mendominasi) | **< 1 dtk** (baca + blake3, keduanya GB/s) |
| Pertumbuhan disk per snapshot | ~250 MB | **~10–50 MB** |
| 20 snapshot | +5 GB | +0.2–1 GB |

DB kecil–menengah (10–200 MB — mayoritas kasus dev) sudah instan di mode full-copy, sehingga v0.1 tetap nyaman; CAS menjadi wajib untuk DB ≥ 1 GB dan pola "snapshot sebelum setiap operasi berisiko" ala agent.

**Prior art:** restic/borg/Kopia (CAS + CDC), Proxmox Backup Server (chunk fixed page-aligned untuk image disk), Litestream (replikasi WAL kontinu — masalah berbeda: DR kontinu, bukan snapshot diskrit dev).

---

## 4. Threat Model — "Jangan Merusak Proyek Orang Lain"

| # | Ancaman | Mitigasi (status: desain wajib) |
| :-- | :--- | :--- |
| T1 | `DATABASE_URL` menunjuk database **remote/production** | **Host guard**: hanya `localhost`, `127.0.0.1`, `::1`, RFC1918 (`10.x`, `172.16–31.x`, `192.168.x`), `host.docker.internal`, unix socket. Selain itu → error keras, kecuali flag `--allow-remote` eksplisit. **Mode MCP: local-only di-hard-code** — tool MCP tidak punya jalur untuk menarget remote, titik. |
| T2 | Snapshot proyek A di-restore ke database proyek B (agent salah direktori) | `config.json` menyimpan fingerprint; setiap perintah memverifikasi target sekarang ≠ fingerprint → tolak, kecuali `--cross-project` eksplisit. Fingerprint juga tertanam di tiap record snapshot. |
| T3 | Restore file snapshot yang korup/terpotong → menimpa DB yang sehat dengan sampah | **Verify-before-restore**: decompress ke `tmp/`, hitung blake3, bandingkan metadata — baru satu byte pun menyentuh DB bila valid. |
| T4 | Metadata.json sobek (proses mati saat save) | Tulis atomic tmp→rename; file snapshot juga ditulis ke tmp lalu di-rename **setelah** metadata commit. |
| T5 | Dua proses dbsnap bersamaan (human + agent) | `lock` file via `fs4` — blocking dengan timeout 10 dtk, pesan error jelas siapa yang memegang. MCP server memakai lock yang sama. |
| T6 | Proses mati di tengah fencing PG (koneksi developer terkunci permanen) | Re-enable `ALLOW_CONNECTIONS` idempoten di awal+akhir tiap operasi PG; `dbsnap status` mendeteksi & memperbaiki sisa fencing. |
| T7 | Restore gagal di tengah jalan (PG drop sudah jalan / MySQL replay setengah) | **Safety snapshot otomatis** kondisi sekarang sebelum restore (default ON, `--no-safety-snapshot` untuk opt-out; retensi otomatis 3 terakhir ber-tag `auto-*`). Rollback satu perintah selalu tersedia. |
| T8 | Disk penuh saat save/restore → DB target korup | Cek ruang bebas (fs4) sebelum operasi; tolak bila < 100 MB atau < 2× ukuran snapshot. |
| T9 | Filename/path injection lewat `tag` | Nama file = `<id>.zst` (id dibuat internal, hex); tag divalidasi `[A-Za-z0-9._-]`, maks 64 char, disimpan di metadata. |
| T10 | SQL/shell injection dari input agent | Spawn **tanpa shell** (argv murni); identifier di-quote ketat (PG `"` / MySQL backtick); id/tag divalidasi. |
| T11 | Kebocoran kredensial lewat log/error/metadata/MCP output | Password di-strip dari semua output; koneksi ditampilkan sebagai `postgres://***@127.0.0.1:5432/myapp_dev`. Tidak ada credential di file apa pun di `.dbsnap/`. |
| T12 | Decompression bomb (`.zst` jahat di `tmp/`) | Cap byte hasil decompress = `original_bytes` di metadata +10%; melebihi → abort sebelum sentuh DB. |
| T13 | Prune menghapus file yang bukan miliknya | Prune **hanya** menghapus path yang tercatat di metadata (bukan glob `*.zst`); orphan dilaporkan `dbsnap status`, tidak pernah dihapus diam-diam. |
| T14 | `.dbsnap/` ter-commit ke Git orang lain | `init` menambahkan `.dbsnap/` ke `.gitignore` + peringatan bila terdeteksi di index Git. |
| T15 | Agent spam restore berkali-kali lewat MCP | Tool MCP berparameter minimal (id/tag saja); lock T5 membuatnya serial; tiap restore butuh verifikasi T3 — dampak spam = CPU, bukan kerusakan data. |

---

## 5. Stabilitas — Skenario Kegagalan & Perlakuannya

| Skenario | Perlakuan |
| :--- | :--- |
| SQLite `database is locked` saat save (dev server menahan write) | `busy_timeout` 5 dtk → retry sekali → error jelas dengan saran. Backup API sendiri aman terhadap reader aktif. |
| `wal_checkpoint(TRUNCATE)` gagal (masih ada reader lama) | Best-effort: snapshot tetap valid, restore tetap sukses; warning di stderr. Checkpoint berikutnya akan membersihkan. |
| Restore SQLite via Backup API gagal di tengah | API bersifat transaksional — target dikembalikan ke kondisi semula oleh `backup_finish` (rusqlite menangani via drop guard). |
| Koneksi PG terputus saat clone | Operasi dibatalkan; database sumber tidak pernah diubah (clone hanya CREATE). Sisa `__dbsnap_*` gagal di-drop otomatis. |
| mysqldump gagal karena mismatch flag flavor | Retry otomatis dengan flag-set fallback (§2.3); error asli tetap ditampilkan bila dua-duanya gagal. |
| Replay dump MySQL gagal di tengah | Database kondisi "setengah" → pesan error wajib menampilkan perintah rollback ke safety snapshot (T7). |
| Proses dibunuh (SIGKILL) kapan pun | Semua mutasi bertahap: lock melepas sendiri (flock), tmp dibereskan oleh operasi berikutnya, fencing PG dirawat healing (T6), metadata atomic (T4). |
| Server PG/MySQL versi lama | `probe()` membaca versi; fitur yang butuh versi tertentu dicegah dengan pesan, bukan error samar di tengah jalan. |

**Kontrak pengujian stabilitas (CI):**
- Integration test engine di GitHub Actions dengan service containers: `postgres:16`, `mysql:8`, `mariadb:11` (+ SQLite lokal murni).
- Chaos-lite: kill proses di titik acak (failpoint env var `DBSNAP_FAILPOINT=...`) lalu jalankan operasi berikutnya dan assert kondisi pulih.
- Assert metadata **tidak pernah** mengandung substring password (test otomatis T11).

---

## 6. Keoptimalan (Target Terukur)

| Aspek | Target | Cara |
| :--- | :--- | :--- |
| Latensi SQLite save/restore (DB ~100 MB) | < 10 ms operasi + I/O; terasa instan | Backup API (copy halaman) + zstd level 3 streaming |
| Save DB besar 1 GB — mode delta (v0.2+, §3.5) | < 1 dtk; disk bertambah 10–50 MB (bukan ~250 MB) | chunk page-aligned + blake3 + CAS |
| Memori proses | Konstan < 50 MB berapapun ukuran DB | Pipeline streaming 64 KB — tidak pernah buffer penuh |
| Ukuran binary release | < 3 MB ter-strip per platform | Tanpa SDK berat, tokio minimal, profile di bawah |
| Startup `dbsnap mcp` | < 20 ms | Single binary; README menyarankan install global, bukan `npx`, agar agent tidak menunggu resolusi npm |
| PostgreSQL clone (1–2 GB dev DB) | ~1–3 dtk (kecepatan disk) | Salinan blok server-side — bukan dump/replay |
| MySQL dump (1 GB, InnoDB) | ~10–60 dtk — **jujur di README** | Dump logis adalah satu-satunya metode aman untuk MySQL |

```toml
[profile.release]
lto = "thin"
codegen-units = 1
strip = true
opt-level = 3
```

---

## 7. MCP — Perubahan Multi-Engine

- `--db` diperluas menjadi `--url` (menerima semua skema) dengan perilaku auto-detect dari `.env` tetap utama.
- Tools v0.2+: `dbsnap_create_snapshot`, `dbsnap_restore_snapshot`, `dbsnap_list_snapshots`, **`dbsnap_status`** (engine, versi server, jumlah snapshot, ruang disk, orphan, sisa fencing — memberi agent visibilitas sebelum bertindak).
- Mode MCP di-hard-code **local-only** (T1) dan mewarisi lock + fingerprint guard seperti CLI.

## 8. Roadmap Revisi

| Versi | Isi | Estimasi |
| :--- | :--- | :--- |
| **v0.1.0** | MVP SQLite penuh (CLI + MCP + npm distribusi + CI) — fondasi trait & registry URL sudah disiapkan multi-engine | 5 hari |
| **v0.1.5** | **Mesin Delta CAS untuk SQLite** (§3.5): snapshot = manifest + chunk — latensi & pertumbuhan disk tidak lagi linear terhadap ukuran DB | 2 hari |
| **v0.2.0** | PostgreSQL: method clone + dump, fencing & healing, `dbsnap_status`, service container CI | 3–4 hari |
| **v0.2.1** | Hardening: failpoint tests, chaos-lite, feedback lapangan | 1 hari |
| **v0.3.0** | MySQL/MariaDB: flavor detection, curated flags + fallback retry, charset preservation | 3–4 hari |
| **v0.4.0+** | `dbsnap export/import` portabel antar mesin, diff, remote storage | backlog |

## 9. Prasyarat Pengguna per Engine (untuk README)

- **SQLite:** tidak ada — bundled.
- **PostgreSQL:** server lokal berjalan; user DB dengan hak CREATEDB/pemilik; `pg_dump` hanya bila pakai `--method dump`.
- **MySQL/MariaDB:** server lokal berjalan; `mysqldump`+`mysql` (atau `mariadb-dump`+`mariadb`) di PATH; tabel InnoDB untuk konsistensi `--single-transaction`.
