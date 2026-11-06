<div align="center">

# ⚡ dbsnap

**Database checkpoints for development and migration rehearsals.**

Save schema and data, preview changes between snapshots, and rehearse a
migration on a separate database. Use the CLI directly or connect the built-in
MCP server to your coding agent.

[![CI](https://github.com/farhan0431/db-snap/actions/workflows/ci.yml/badge.svg)](https://github.com/farhan0431/db-snap/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/dbsnap.svg)](https://crates.io/crates/dbsnap)
[![npm](https://img.shields.io/npm/v/%40farhan0431%2Fdb-snap.svg)](https://www.npmjs.com/package/@farhan0431/db-snap)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**SQLite** · **PostgreSQL** · **MySQL/MariaDB**

</div>

```sh
dbsnap init
dbsnap save before-migration
# Run your migration on the development database.
dbsnap save after-migration
dbsnap diff before-migration after-migration
```

If you need to undo the change, run `dbsnap restore before-migration` after
checking the target database and the engine's restore requirements.

## What dbsnap does well

- **Save a useful restore point.** Preserve schema and data together. SQLite
  shares unchanged chunks between snapshots. PostgreSQL offers server-side
  clones or custom dumps. MySQL/MariaDB uses logical dumps.
- **Review changes before rollout.** Use `diff` for schema and table counts.
  In v0.4.1, `diff-rows` also compares rows by primary key across
  engines and can show changed values in a Git-style terminal view.
- **Rehearse on a copy.** Create a sandbox, point your app and migration runner
  at it explicitly, then check the result before touching the intended target.
- **Move a checkpoint.** v0.4.1 adds checksummed export and import. Transfer
  sanitized fixtures between machines or into a test environment.
- **Give agents a recovery path.** The built-in MCP server exposes checkpoint,
  verification, and diff tools. Restore, prune, and trusted dump replay require
  an authorized session and approval for each call.

See [database support](#database-support) and the
[manual workflow comparison and benchmark](#workflow-and-performance).

## Using dbsnap before a deployment

Run dbsnap in a development or staging database as part of a migration rehearsal
or a CI job with disposable databases. Save a checkpoint, point the migration
runner at a sandbox, then verify the migration and review its schema and row
changes. Use your existing deployment pipeline to apply the reviewed migration.

`dbsnap exec -- <program> <args>` saves a checkpoint before starting your
migration command. The command inherits its environment, so set the migration
runner's database URL separately. A failed command leaves the checkpoint
available for inspection and explicit recovery.

> **Production boundary.** Keep your production backup and recovery plan in
> your database platform or deployment pipeline. dbsnap targets development
> workflows. MySQL/MariaDB restores to the original database are non-atomic
> and require stopped application writers and scheduled events. See the
> [MySQL recovery guide](docs/MYSQL-RESTORE.md).

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

## Why a database checkpoint matters

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

## Database support

This matrix describes v0.4.1. Check your installed
version with `dbsnap --version`. For a server database, set `DATABASE_URL`
to the intended development database and run `dbsnap init` first.

| Capability | SQLite | PostgreSQL | MySQL / MariaDB |
| --- | --- | --- | --- |
| Save | Online page backup with deduplicated chunks | Server-side clone or custom dump | Transactional logical dump with client flavor detection |
| Restore | Verify and restore into the existing file | Working clone or staged dump, with journaled cutover | In-place replay with a mandatory pinned safety snapshot and journal |
| Schema and row-count diff | Tables, views, indexes, and triggers | Clone table and view definitions, or offline comparison of two compatible custom dumps | Offline comparison of table definitions and counts in standard dbsnap dumps |
| Primary-key row preview | Reads verified staged snapshot files | Reads clones directly. Custom dumps require `--replay-dumps`. | Replays each trusted dump into an isolated Docker server |
| Sandbox | New SQLite file | Separate database from a clone or trusted custom dump | Docker container with networking disabled and no published port |
| Portable archive | Packages complete SQLite data | Packages a custom dump. Exports clones by converting them to dumps. | Packages the logical dump. The destination needs the original database name. |
| CI table-drop policy | Supported | Supported for clones | Supported for standard dbsnap dumps |

### SQLite · quick checkpoints for local files

Use SQLite when you want to preserve your local schema and data without a
separate database server. The Online Backup API takes a consistent copy,
including changes recorded in WAL, and the snapshot store reuses unchanged
chunks. Keep the database file and project snapshot store on persistent
storage. The database file must already exist.

```sh
dbsnap init --db ./dev.db
dbsnap save before-migration
dbsnap sandbox before-migration --output ./experiment.db
```

Point your migration runner at the sandbox file. Row preview requires an
explicit primary key, and its row and byte limits also apply to SQLite.

### PostgreSQL · clones for daily work, dumps for transfer

Use the default clone for a fast iteration loop on a development server.
Cloning uses server storage and briefly fences connections to the source.
A clone depends on the server's data volume. Use custom dump mode when you
need a file you can carry to another compatible server.

```sh
dbsnap save before-migration
# Apply a change to your development database.
dbsnap save after-migration
dbsnap diff-rows before-migration after-migration --table public.users
dbsnap sandbox before-migration

dbsnap save portable-seed --method dump
dbsnap export portable-seed --output seed.dbsnap
```

Server-side sandbox creation and restore require database creation privileges.
Use matching-major `pg_dump` and `pg_restore` for dump operations, and make
sure the destination supports the dump's extensions. Dump restore replays in
staging before the journaled cutover. For ordinary `diff`, compare two clones
or two custom dumps. Clone verification checks existence. Dump verification
checks archive integrity.

### MySQL and MariaDB · offline schema review and isolated row preview

Use compatible `mysql` and `mysqldump` tools, or their MariaDB equivalents, on
`PATH`. Snapshot consistency relies on transactional tables such as InnoDB.
Avoid concurrent schema changes during the dump. Snapshots retain routines,
triggers, and events, while ordinary diff compares tables and row counts.

```sh
dbsnap save before-migration
# Apply a change to your development database.
dbsnap save after-migration
dbsnap diff before-migration after-migration
dbsnap diff-rows before-migration after-migration --table users --docker-image mysql:8.0
dbsnap sandbox before-migration --docker-image mysql:8.0
```

Use `mariadb:11` for MariaDB. Start with offline `diff` for a quick review,
then use Docker row preview when you need to see row-level changes.
Docker startup and dump replay contribute to preview latency.
Use `docker exec` to reach the sandbox because it has no published port.

**Restore to the original database is non-atomic.** Stop application writers,
connections, and scheduled events before restoring. If replay fails, keep the
journal and pinned safety snapshot for recovery. See the
[MySQL/MariaDB recovery guide](docs/MYSQL-RESTORE.md).

## CLI

```bash
dbsnap init                     # bind the project (finds DATABASE_URL / prisma/dev.db automatically)
dbsnap save "before-migration"  # save a tagged snapshot
dbsnap list                     # see every restore point
dbsnap restore "before-migration"
dbsnap rollback                 # restore the most recent snapshot
dbsnap prune --keep 5           # keep only the 5 newest
dbsnap status                   # binding, engine info, disk usage
dbsnap save baseline -m "Before migration" --git-branch
dbsnap tag baseline before-migration
dbsnap rm before-migration      # remove one unpinned snapshot
dbsnap list --json              # machine-readable snapshot catalog
dbsnap status --json            # machine-readable database status
dbsnap diff before after        # compare supported snapshots
dbsnap pin "golden seed"        # protect a snapshot from pruning
dbsnap unpin "golden seed"      # remove prune protection
dbsnap verify                  # check active snapshot artifact integrity
dbsnap doctor                  # diagnose connection, recovery and untracked files
dbsnap recover                 # recover using a server restore journal
dbsnap exec -- npm run db:migrate # checkpoint before a command; no automatic rollback
dbsnap rehearse before --fail-on-drop-table -- npx prisma migrate deploy
dbsnap diff-live before         # capture/retain live checkpoint and compare
dbsnap diff-live before --ephemeral # compare without keeping a restore point
dbsnap diff-rows before after --table users # bounded primary-key row-change counts
dbsnap diff-rows before after --table users --format git --show-values
dbsnap diff-rows before after --table users --column role --key-min 100 --key-max 199
dbsnap diff-report before after --fail-on-drop-table
dbsnap export before --output before.dbsnap
dbsnap import before.dbsnap --tag transferred
dbsnap sandbox before --output ./experiment.db # new independent SQLite file
dbsnap sandbox-list             # recorded sandboxes and their current state
dbsnap sandbox-remove CONTAINER_ID # remove an owned Docker sandbox
```

Status shows the latest restore point. `--git-branch` records the branch
at save time when Git is installed and HEAD points to a branch. Notes are
local catalog metadata, visible through `status` (for the latest snapshot)
and `list --json`. Keep secrets out of notes.

The CLI's `diff-live --ephemeral` still captures a temporary artifact and
can briefly fence PostgreSQL connections in clone mode. If cleanup was
interrupted after the artifact was recorded, check `doctor` and run
`dbsnap prune --keep N` with `N` at least the current snapshot count to
retry cleanup without pruning active snapshots. An interruption before the
catalog commit can leave an untracked artifact for manual inspection.

`diff` supports SQLite snapshots, PostgreSQL server-side clone snapshots, and
standard MySQL/MariaDB dump snapshots created by dbsnap.
For SQLite it compares schema objects and table row counts. For PostgreSQL it
compares table/view definitions and table row counts; indexes, constraints,
routines, and triggers are not compared. Individual row contents are not
compared on any engine. MySQL/MariaDB diff reads verified dumps offline and
compares table definitions (including inline keys) and row counts. It does not
execute SQL or compare views, routines, triggers or events. Unsupported INSERT
formats produce an error. Two PostgreSQL dump archives can also be compared
offline using `pg_restore` for
archive-wide schema changes and table COPY row counts, excluding ownership,
privileges, row contents and sequence values. Mixed clone/archive comparison is
unsupported. No SQL is replayed into the target.
When a safety snapshot is enabled, restore stops if it cannot be created.

### Portable archives, row previews, and sandboxes

**Version requirement.** Portable `export/import`, Git output, server row
previews, and PostgreSQL dump sandboxes require dbsnap v0.4.1 or later.

```sh
# Export in the source project, then import in the destination project
dbsnap export before --output before.dbsnap
dbsnap import before.dbsnap --tag transferred
dbsnap verify transferred

# Show actual changes in the terminal
dbsnap diff-rows before after --table users --format git --show-values
```

- **Treat archives as database copies.** They can contain sensitive application
  data. Keep them private and import only from trusted sources. Checksums detect
  corruption. Import creates an unpinned snapshot with a new local ID and leaves
  the live database untouched. Restoring it is a separate operation.
- **Keep row values private.** JSON and MCP return counts by default.
  `--format git --show-values` reveals stored values in terminal output.
  Comparisons require a primary key and matching layouts. If layouts differ,
  Git output shows the schema changes and skips the row comparison. The default
  limit is 10,000 rows per snapshot, configurable up to 100,000, with a 16 MiB
  encoded value budget per snapshot.
- **Replay trusted dumps on development systems.** A PostgreSQL sandbox is a
  separate database on the same server. Functions, extensions, and foreign
  connections may still reach shared resources. MCP dump replay requires an
  authorized session and approval for each call. Delete persistent sandboxes
  explicitly after use. Abrupt termination can leave temporary replay resources
  behind.
- **Keep restore safeguards enabled.** `verify` checks artifacts, or clone
  existence for PostgreSQL. A successful verification does not prove that a
  restore will succeed. MySQL/MariaDB restore on the original target remains
  non-atomic and requires stopped writers and scheduled events.

Local CLI/MCP process tests cover these workflows on SQLite, PostgreSQL 17,
MySQL 8.0, and MariaDB 11. The integration suite also checks corrupt archive
rejection and SQLite restore after import into a separate project.

See [development workflow](docs/DEVELOPMENT-WORKFLOW.md) for the command
contract, cleanup, and recovery procedures.

`dbsnap` resolves your database in this order: `--db` flag → `DATABASE_URL`
from `.env` → common paths (`prisma/dev.db`, `dev.db`, `*.sqlite`, …).

With dbsnap v0.4.2 or later, run `dbsnap init` from the directory you want to bind.
In a monorepo, this lets each subproject have its own catalog. After init,
commands run from subdirectories still find the nearest initialized project.

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

v0.4.4 adds `dbsnap rehearse` for trusted migration commands on retained
SQLite/PostgreSQL sandboxes, with timeout and JSON policy reports.
Check that the migrator uses its injected sandbox connection. Commands run
with the user's privileges. MySQL/MariaDB rehearsal is pending, and sandbox
cleanup is manual. See [migration rehearsal](docs/REHEARSE.md) for exit codes,
connection variables, MCP approval and cleanup.

The current source server exposes eleven tools:

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
| `dbsnap_verify` | Check artifact integrity or clone existence without replaying SQL. |
| `dbsnap_doctor` | Diagnose connections, journals, pending GC and untracked files. |
| `dbsnap_diff_live` | Save and retain a live checkpoint, then compare it with `from`. |
| `dbsnap_rehearse` | Run an approved trusted migration command on a retained SQLite/PostgreSQL sandbox and report changes. |
| `dbsnap_diff_rows` | Bounded added/removed/updated counts by primary key. Trusted dump replay requires explicit options. |

Tool results retain text and add `structuredContent` with `ok`, `message`,
optional typed `data`, and broad error categories. Creation returns the exact
snapshot ID in data. Output schemas and annotations help clients interpret the
result; permission checks still run in code. See the development workflow guide.

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
  TEMPLATE dev`. The server copies the database while dbsnap briefly fences
  connections. Runtime depends on the database size and storage.
  **`--method dump`:** `pg_dump -Fc` — slower but portable across machines.
- **PostgreSQL restore:** create a working database, fence the target, record
  a safety snapshot, and use a journaled rename cutover. Dump replay occurs in
  staging first. Cutover is not atomic and may require manual recovery.
- **MySQL/MariaDB save:** `mysqldump --single-transaction` (consistent for
  InnoDB without locking writers) → zstd → blake3. Flavor detection picks
  `mariadb-dump` vs `mysqldump` and retries with a fallback flag set for the
  known cross-version quirks. Dump and replay time depend on database size,
  server load, and storage.
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
bash scripts/e2e-mysql.sh # MySQL/MariaDB: verified in-place restore on a disposable database
```

The PostgreSQL demo requires Python 3 and a disposable server. It removes its
test database but retains the snapshot catalog and may leave server-side
clones; it prints the catalog location for inspection.

## Workflow and performance

When rehearsing a migration, the time cost includes preparing snapshots,
remembering where they live, and making the before/after comparison repeatable.
dbsnap gives each checkpoint an ID so the same snapshots can be used for
inspection, rehearsal, and recovery.

| Task | Manual workflow | With dbsnap |
| --- | --- | --- |
| Keep a restore point | Run an engine-specific backup command and record its file or database name. | `dbsnap save before-migration` captures schema and data and records the checkpoint in the project catalog. |
| Review what changed | Compare schema exports, then write queries to match rows by primary key. | `dbsnap diff before-migration after-migration` reviews schema and counts. `diff-rows` can focus on a table, selected columns, and an integer key range. |
| Try the migration on a copy | Prepare a separate database, restore into it, and record what to clean up. | `dbsnap sandbox before-migration` creates a PostgreSQL sandbox. SQLite takes a new output path, and MySQL/MariaDB uses an explicitly selected Docker image. |
| Recover a failed change | Locate the right backup, check the target, and follow the engine's restore procedure. | `dbsnap restore SNAPSHOT_ID` checks the binding and uses the engine's safety snapshot and recovery flow. |

### Row preview benchmark

For a self-comparison on **8,000 rows**, dbsnap v0.4.1 prepares the snapshot
once and validates the preview limits. The filtered case selects **100 rows
and one non-key column**.

| Engine and snapshot | Earlier dbsnap build | v0.4.1 full preview | v0.4.1 filtered preview |
| --- | ---: | ---: | ---: |
| SQLite delta | 2,125 ms | **829 ms** | **68 ms** |
| PostgreSQL 17 clone | 985 ms | **507 ms** | **118 ms** |
| MySQL 8.0 dump in Docker | 45.0 s | **27.9 s** | **19.7 s** |

These are median end-to-end times from local Windows debug builds. The
benchmark compares two dbsnap builds. The manual workflow above compares
steps. For two different snapshots, both still need preparation. MySQL
times include Docker startup, dump replay, and cleanup. Filters reduce
row work while dump replay still runs. See the [benchmark method and
reproduction steps](docs/TESTING.md#row-preview-benchmark).

### macOS benchmark with two different snapshots

A user tested dbsnap v0.4.1 on an **iMac M4 with 16 GB RAM**. The times below
are individual wall-clock observations. The diff compares schema and table
row counts across two snapshots after a change.

| Engine and baseline fixture | Save | Diff | Rollback |
| --- | ---: | ---: | ---: |
| PostgreSQL 16.15 clone in Docker · 176,050 rows | 213 ms | 65 ms | 400 ms |
| MySQL 9.6.0 local · 102,000 rows | 166 ms | 40 ms | 875 ms |
| SQLite WAL · 255,010 rows | 200 ms | 97 ms | 276 ms |

MySQL's 40 ms result is the offline schema/count diff. For a comparison
that reads individual rows, see the Docker row preview benchmark above.
See [fixture details and evidence limits](docs/TESTING.md#macos-field-benchmark).

### Snapshot and restore examples

These results come from local tests and demos on disposable databases.

| Scenario | Result |
| :--- | :--- |
| SQLite snapshot (100 MB, unchanged DB) | content is checked and linked without writing duplicate chunks |
| SQLite snapshot (1.6 MB DB, one 8 KB row changed) | **46.9 KB** written (13.7× smaller than full copy) |
| SQLite restore under a live WAL writer | data verified restored; server kept writing through the whole episode |
| PostgreSQL `DROP COLUMN` → rollback | 50 rows + column verified back bit-for-bit via `psql` |
| MySQL/MariaDB restore | Object restore and recovery tested on disposable databases |

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

PRs welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) and the
[maintainer roadmap](docs/MAINTAINER-ROADMAP.md) for upcoming work. Security issues:
[SECURITY.md](SECURITY.md) (private disclosure, please).

## License

[MIT](LICENSE)
