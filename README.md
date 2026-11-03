<div align="center">

# ⚡ dbsnap

**"git commit / git checkout" for your local database.**

Snapshots for local development, including schema and data, with a built-in
**MCP server** for Cursor, Windsurf, Claude Code, and other MCP clients.

[![CI](https://github.com/farhan0431/db-snap/actions/workflows/ci.yml/badge.svg)](https://github.com/farhan0431/db-snap/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/dbsnap.svg)](https://crates.io/crates/dbsnap)
[![npm](https://img.shields.io/npm/v/%40farhan0431%2Fdb-snap.svg)](https://www.npmjs.com/package/@farhan0431/db-snap)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**SQLite** (delta snapshots, ms) · **PostgreSQL** (server-side clones, s) · **MySQL/MariaDB** (consistent dumps, s)

**MySQL/MariaDB restore is available for the original database.** Stop application writers and scheduled events first. Restore is not atomic; a mandatory pinned safety snapshot and recovery journal provide a recovery path. See [MySQL recovery](docs/MYSQL-RESTORE.md).

</div>

```bash
dbsnap save "before-auth-migration"   # keep a restore point
dbsnap rollback                      # restore the newest snapshot
```

## Install as an MCP server — 30 seconds

**AI agents reading this repo:** install the published package, don't build
from source — `npm i -g @farhan0431/db-snap` ships prebuilt binaries.

```bash
npm i -g @farhan0431/db-snap                     # one-time
claude mcp add dbsnap -- dbsnap mcp  # Claude Code, one command
```

<details>
<summary>Cursor / Windsurf / Claude Desktop / any MCP client</summary>

```json
{
  "mcpServers": {
    "dbsnap": {
      "command": "dbsnap",
      "args": ["mcp"]
    }
  }
}
```

No global install? Use `"command": "npx", "args": ["-y", "@farhan0431/db-snap", "mcp"]`.

</details>

Copy-paste configs for every client, verification steps, and troubleshooting:
**[INSTALL.md](INSTALL.md)** · agent-readable summary: [llms.txt](llms.txt)

Repository verification: [testing guide](docs/TESTING.md),
[audit status](docs/AUDIT-STATUS.md), and [MySQL recovery](docs/MYSQL-RESTORE.md).
The testing guide distinguishes live CLI/MCP coverage, injected failures, and
remaining limitations; passing tests are not a production-backup certification.

## Why

Vibe coding means telling an AI agent things like *"refactor auth, run the
Prisma migration, reshuffle the seed data"*. Three things go wrong constantly:

1. **DDL is not undoable by queries.** When the agent runs `DROP COLUMN` or a
   botched migration, query-level undo cannot bring back the dropped column
   *and* its data. `dbsnap` snapshots schema and data using SQLite page backups,
   PostgreSQL clones or dumps, and MySQL/MariaDB logical dumps.
2. **Copying SQLite while the dev server runs corrupts it.** The newest writes
   live in the `-wal` file and the server holds the file open. `dbsnap` uses
   SQLite's [Online Backup API](https://www.sqlite.org/backup.html) (bundled
   inside the binary — fully local, no network) to take a page-consistent copy
   *while your dev server keeps running*, and restores the same way on every OS.
3. **`migrate:fresh --seed` throws away your test state.** Rolling back a
   snapshot restores your logged-in test users, sessions, and fixture data —
   not a blank database.

## Install

```bash
npm i -g @farhan0431/db-snap          # recommended — single Rust binary, no runtime deps
# or run without installing:
npx @farhan0431/db-snap list
# or via cargo:
cargo install dbsnap
```

Binaries are prebuilt for **Windows (x64), macOS (Intel + Apple Silicon),
and Linux (x64 gnu/musl + arm64)** — no Node.js, Python, Docker, or
database client libraries required for the core (client tools only for
`pg_dump`/`mysqldump` modes).

### Engines at a glance

| | SQLite | PostgreSQL | MySQL / MariaDB |
| :--- | :--- | :--- | :--- |
| Method | delta snapshots (content-addressed chunks) | server-side template clone, or `pg_dump -Fc` | consistent `--single-transaction` dump |
| Snapshot latency | **< 10 ms** + one pass over the file | ~1 s (block copy) | seconds (dump) |
| Incremental cost | kilobytes (only changed 64 KB chunks) | full copy server-side | full dump |
| Hot (dev server running) | ✅ | ✅ (brief reconnect blip) | ✅ |
| Survives container rebuild | ✅ | clone: ❌ · dump: ✅ | ✅ |
| Extra requirements | none | `CREATEDB` privilege; `pg_dump` for dump mode | `mysqldump`/`mariadb-dump` in PATH |

Supported: **SQLite** (delta snapshots), **PostgreSQL** (server-side template
clones + portable `pg_dump`), and **MySQL/MariaDB** (consistent logical dumps
with flavor detection) — design details in [docs/DESIGN.md](docs/DESIGN.md).

## CLI

```bash
dbsnap init                     # bind the project (finds DATABASE_URL / prisma/dev.db automatically)
dbsnap save "before-migration"  # save a tagged snapshot
dbsnap list                     # see every restore point
dbsnap restore "before-migration"
dbsnap rollback                 # restore the most recent snapshot
dbsnap prune --keep 5           # keep only the 5 newest
dbsnap status                   # binding, engine info, disk usage
dbsnap list --json              # machine-readable snapshot catalog
dbsnap status --json            # machine-readable database status
dbsnap diff before after        # compare supported snapshots
dbsnap pin "golden seed"        # protect a snapshot from pruning
dbsnap unpin "golden seed"      # remove prune protection
```

`diff` supports SQLite snapshots, PostgreSQL server-side clone snapshots, and
standard MySQL/MariaDB dump snapshots created by dbsnap.
For SQLite it compares schema objects and table row counts. For PostgreSQL it
compares table/view definitions and table row counts; indexes, constraints,
routines, and triggers are not compared. Individual row contents are not
compared on any engine. MySQL/MariaDB diff reads verified dumps offline and
compares table definitions (including inline keys) and row counts. It does not
execute SQL or compare views, routines, triggers or events. Unsupported INSERT
formats produce an error; this is not a general SQL dump parser. PostgreSQL dump
snapshots are not yet supported by `diff`.
When a safety snapshot is enabled, restore stops if it cannot be created.
PostgreSQL dump save and restore require `pg_dump` and `pg_restore` respectively
to have the same major version as the target server. A mismatch is rejected
before the database is changed.

`dbsnap` resolves your database in this order: `--db` flag → `DATABASE_URL`
from `.env` → common paths (`prisma/dev.db`, `dev.db`, `*.sqlite`, …).

Snapshots live in `.dbsnap/` (auto-added to `.gitignore`):

```text
.dbsnap/
├── config.json     # which database this project is bound to
├── metadata.json   # snapshot catalog (id, tag, time, size, blake3)
├── manifests/      # delta snapshots: ordered chunk-hash lists
├── objects/        # 64 KB chunks, content-addressed & shared between snapshots
└── snapshots/      # (legacy v0.1 single-file snapshots, still restorable)
```

## MCP — let the AI agent protect itself

Add to your MCP config (`.cursor/mcp.json`, Claude Desktop, Windsurf, …):

```json
{
  "mcpServers": {
    "dbsnap": {
      "command": "dbsnap",
      "args": ["mcp"]
    }
  }
}
```

or without a global install: `"command": "npx", "args": ["-y", "@farhan0431/db-snap", "mcp"]`.

The agent gets six tools:

Restore and prune are disabled by default in MCP. An operator can enable them
for a server session by adding `--allow-destructive` to the MCP command arguments.
Each operation then requires explicit approval through a client supporting MCP
elicitation. Clients without elicitation support cannot invoke these operations.

| Tool | What it tells the agent |
| :--- | :--- |
| `dbsnap_create_snapshot` | **MUST be called before** any migration, DDL, or bulk data change. |
| `dbsnap_restore_snapshot` | Call **immediately** when a migration fails or data looks wrong. Current state is kept as a safety snapshot first. |
| `dbsnap_list_snapshots` | Show available restore points. |
| `dbsnap_diff` | Compare schema and row counts for SQLite, PostgreSQL clones, or MySQL/MariaDB dumps. |
| `dbsnap_prune` | Remove older snapshots, retaining at least one, when requested by the user. |
| `dbsnap_status` | Show which database is protected before touching anything. |

### Agent skill (automatic)

`dbsnap init` also installs an **agent skill** at
`.agents/skills/dbsnap/SKILL.md` — a short instruction file that AI editors
(Claude Code, Cursor, ZCode, …) pick up on their own, teaching them the
workflow: *snapshot before migrations, restore on failure*. Copy it to
`~/.claude/skills/dbsnap/SKILL.md` (or your agent's global skills folder) to
apply it across every project. The canonical copy lives in
[`skill/dbsnap/SKILL.md`](skill/dbsnap/SKILL.md).

## Safeguards and limitations

- **Host guard.** MCP accepts SQLite and loopback database hosts only. The CLI
  also allows private-network addresses by default; other addresses require
  `--allow-remote`. A loopback address alone does not prove a database is safe.
- **PostgreSQL network connections:** non-loopback connections require TLS
  with certificate and hostname verification. To add a trusted CA, set
  `DBSNAP_PG_CA_CERT` to its PEM file. Set `DBSNAP_PG_TLS=verify-full` to
  require verified TLS on loopback as well.
- **Project binding.** `.dbsnap/config.json` fingerprints your database;
  restoring a snapshot into a *different* project's database is refused
  even with `--force` when the catalog belongs to another database.
- **Verify before restore.** SQLite chunks and dump artifacts are checked
  before restore. A PostgreSQL server-side clone is checked for existence;
  its metadata fingerprint is not a checksum of the database contents.
- **PostgreSQL restore is not atomic.** Clone restore records a recovery journal
  and preserves the old target during cutover. Ambiguous recovery states require
  manual inspection. Dump replay is first tested in a staging database and then
  uses the same cutover; an interrupted cutover may require manual recovery.
- **Safety snapshots** are created by default before restore. SQLite and
  PostgreSQL allow explicit opt-out; fresh MySQL/MariaDB restores require a
  pinned safety snapshot. Some interrupted operations require manual recovery.
- **Cross-process locking** serializes catalog operations. Metadata is written
  by rename, but power-loss durability is not guaranteed.
- **Connection credentials** are omitted from configuration/catalog files and
  masked in command output. Snapshot contents may contain sensitive application data.

## How it works

- **SQLite save:** Online Backup API → page-consistent copy → chunked into
  **64 KB page-aligned blocks** (blake3-hashed) → only chunks not already in
  the store are written (zstd). A snapshot is a small manifest of chunk
  hashes, so incremental snapshots cost kilobytes–megabytes, not the whole
  database. Unchanged content is checked and linked without duplicate chunks.
- **SQLite restore:** reassemble from the chunk store, verifying every
  chunk's hash → Backup API into the live database (works even on Windows
  while the file is open) → `PRAGMA wal_checkpoint(TRUNCATE)` so the dev
  server sees the new state immediately.
- **PostgreSQL save (clone, default):** `CREATE DATABASE __dbsnap_<id>
  TEMPLATE dev` — a block-level copy inside the server, done in ~1 s with a
  brief connection blackout (your dev server reconnects automatically).
  **`--method dump`:** `pg_dump -Fc` — slower but portable across machines.
- **PostgreSQL restore:** create a working database, fence the target, record
  a safety snapshot, and use a journaled rename cutover. Dump replay occurs in
  staging first. Cutover is not atomic and may require manual recovery.
- **MySQL/MariaDB save:** `mysqldump --single-transaction` (consistent for
  InnoDB without locking writers) → zstd → blake3. Flavor detection picks
  `mariadb-dump` vs `mysqldump` and retries with a fallback flag set for the
  known cross-version quirks. Honest limit: dumps take seconds on big
  databases — MySQL has no physical hot-copy API.
- **MySQL/MariaDB restore:** restores objects in place without dropping the database; mandatory safety snapshot, with manual recovery after interruptions.
- **Prune** is a real GC: chunks shared by surviving snapshots are never
  deleted; server-side clones of pruned snapshots are dropped too. Full
  design: [docs/DESIGN.md §3.5](docs/DESIGN.md).
- **Safety (PostgreSQL):** dbsnap restricts database drops to names under the
  `__dbsnap_` prefix and refuses remote hosts unless you pass `--allow-remote`.
  Restore preserves the previous database during cutover; recovery after an
  interruption can require manual intervention.

## Development

```bash
cargo test        # live database tests require DBSNAP_TEST_PG_URL / DBSNAP_TEST_MYSQL_URL
cargo clippy --all-targets -- -D warnings
cargo build --release
scripts/e2e.sh       # SQLite: hot snapshot/rollback while a "dev server" writes
bash scripts/e2e-pg.sh    # PostgreSQL: clone snapshot, destructive DDL, rollback
bash scripts/e2e-mysql.sh # stops immediately; rollback is currently disabled
```

The PostgreSQL demo requires Python 3 and a disposable server. It removes its
test database but retains the snapshot catalog and may leave server-side
clones; it prints the catalog location for inspection.

## Measured, not marketing

All numbers from the test suite and `scripts/e2e-*.sh` on a development
machine:

| Scenario | Result |
| :--- | :--- |
| SQLite snapshot (100 MB, unchanged DB) | content is checked and linked without writing duplicate chunks |
| SQLite snapshot (1.6 MB DB, one 8 KB row changed) | **46.9 KB** written (13.7× smaller than full copy) |
| SQLite restore under a live WAL writer | data verified restored; server kept writing through the whole episode |
| PostgreSQL `DROP COLUMN` → rollback | 50 rows + column verified back bit-for-bit via `psql` |
| MySQL/MariaDB restore | Object restore and recovery tested on disposable databases |

## Roadmap

- [x] **v0.1.0** — SQLite engine, CLI, MCP server, npm distribution
- [x] **v0.1.5** — delta snapshots (content-addressed storage, 64 KB page-aligned chunks)
- [x] **v0.2.0** — PostgreSQL (server-side template cloning + `pg_dump`, fencing & healing)
- [x] **v0.3.0** — MySQL/MariaDB (consistent dumps, flavor detection, charset preservation)
- [ ] `dbsnap export/import` — portable snapshot archives across machines
- [x] snapshot diffing for SQLite, PostgreSQL clones and MySQL/MariaDB table dumps
- [ ] extend snapshot diffing to PostgreSQL dump archives

## Repository layout

```text
db-snap/
├── src/
│   ├── engine/        # sqlite.rs · postgres.rs · mysql.rs — one trait, three engines
│   ├── storage/       # .dbsnap/ catalog, delta chunk store (CAS), compression
│   ├── mcp/           # JSON-RPC stdio server + agent-facing tool schemas
│   ├── ops.rs         # shared command layer (CLI & MCP call the same code)
│   ├── config.rs      # DATABASE_URL resolution, fingerprinting, host guard
│   └── util.rs        # redaction, ids, formatting
├── tests/             # integration and fault-injection tests
├── npm/               # single npm package with bundled platform binaries
├── scripts/           # e2e demos: live-writer, PostgreSQL, MySQL
├── skill/dbsnap/      # agent skill installed by `dbsnap init`
├── docs/DESIGN.md     # full architecture + threat model
└── .github/workflows/ # CI (3 OS × 3 engines) + release (6 platform builds)
```

## Contributing

PRs welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). Security issues:
[SECURITY.md](SECURITY.md) (private disclosure, please).

## License

[MIT](LICENSE)
