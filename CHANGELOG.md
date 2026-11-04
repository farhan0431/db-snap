# Changelog

All notable changes to this project are documented in this file.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.4.3] — 2026-11-04

### Added

- Rename a snapshot with `dbsnap tag` and remove a single unpinned snapshot
  with `dbsnap rm`.
- Record local notes and optional Git branch context on save. Status now
  includes the latest restore point.
- Compare live state with `diff-live --ephemeral` without retaining a restore
  point. Interrupted cleanup of recorded artifacts can be retried with prune.

## [0.4.2] — 2026-11-04

### Fixed
- Bind `init` to its current directory so a nested project can use its own
  database and snapshot catalog.
- Search common Homebrew client directories when the MCP process has a
  limited PATH. Use the same PostgreSQL client for version checking and
  execution.
- Let `diff-rows --show-values` select Git-style output when no format is
  specified. An explicit JSON format still rejects row values.

## [0.4.1] — 2026-11-04

### Added
- Verified portable snapshot archives with `export` and `import`.
- Primary-key row previews across engines, Git-style CLI output, column
  selection, and integer key ranges.
- PostgreSQL dump sandboxes, sandbox inventory and diagnostics, and removal
  of Docker sandboxes after verifying project ownership.
- Bounded JSON migration reports with an optional table-drop policy.

### Changed
- Prepare a snapshot only once for a row self-comparison.

## [0.4.0] — 2026-11-04

### Added
- `verify`, `doctor`, and journal-based `recover`.
- Checkpointed command execution, live comparison, and SQLite row previews.
- Offline PostgreSQL archive comparison and sandbox workflows for all engines.
- Structured results and additional diagnostics and comparison tools in MCP.

### Changed
- Retry cleanup of artifacts from snapshots already retired from the catalog.

## [0.3.14] — 2026-11-03

See the [0.3.x release history](https://github.com/farhan0431/db-snap/compare/v0.3.0...v0.3.14)
for scoped npm packaging, restore safety and recovery, and snapshot diff.

## [0.3.0] — 2026-11-03

### Added
- Agent/repo discoverability for MCP installation: `llms.txt` (LLM-readable
  install summary), `AGENTS.md` (conventions for agents working on the
  repo), `INSTALL.md` (copy-paste MCP configs for Claude Code, Cursor,
  Windsurf, Claude Desktop, and generic clients), and a top-of-README
  install block.

- **MySQL/MariaDB engine** — consistent logical snapshots via
  `mysqldump --single-transaction` (InnoDB-safe, no writer locks), piped
  through zstd with blake3 integrity hashing.
- Flavor detection (`SELECT VERSION()`): MariaDB prefers `mariadb-dump`/
  `mariadb`, MySQL prefers `mysqldump`/`mysql`, with cross-flavor fallbacks.
- Curated dump flag sets per flavor with automatic retry fallback for
  cross-version quirks (`column_statistics`, GTID flags).
- MySQL restore: verify dump hash → capture charset/collation → automatic
  safety dump → drop + recreate with original attributes → replay with
  stop-on-first-error.
- `mysql://` and `mariadb://` URL schemes share the remote-host guard.
- CI services for `mysql:8` and `mariadb:11`; `scripts/e2e-mysql.sh`.

## [0.2.0] — 2026-11-03

### Added
- **PostgreSQL engine** — server-side block copy via
  `CREATE DATABASE __dbsnap_<id> TEMPLATE`, with a ~1 s connection fence
  (fence → clone → unfence) and idempotent healing after crashes.
- `--method dump` for portable `pg_dump -Fc` artifacts (password via
  `PGPASSWORD`, never argv).
- Restore re-creates the database with its original encoding/locale/owner.
- GC drops pruned server-side clones — only `__dbsnap_*` names are ever
  touched.
- Engine-mismatch guard: snapshots cannot cross engines.
- `postgres://` URLs wired into the local-host guard (`--allow-remote` is
  human-only; MCP stays hard-wired local-only).
- CI service for `postgres:16`; `scripts/e2e-pg.sh`.

### Changed
- Engine trait restructured: engines own their full restore flow and persist
  safety-snapshot records immediately (crash-safe).

## [0.1.5] — 2026-11-03

### Added
- **Delta snapshots** (content-addressed storage): databases are chunked
  into 64 KB page-aligned blocks, blake3-hashed; a snapshot is a small
  manifest and only chunks missing from the shared store are written.
  Measured: 1.6 MB database, one 8 KB row changed → 46.9 KB snapshot.
- Restore reassembles from chunks with per-chunk integrity verification.
- Prune is a true GC (shared chunks are never deleted).
- Legacy v0.1 single-file snapshots remain restorable.

## [0.1.0] — 2026-11-03

### Added
- Initial release: SQLite engine (Online Backup API, hot <10 ms), CLI
  (`init/save/restore/rollback/list/prune/status`), MCP stdio server with
  agent-guiding tool descriptions, agent skill auto-installed by `init`.
- Safety model: project fingerprint binding, local-only host guard,
  verify-before-restore, automatic safety snapshots, cross-process lock,
  atomic metadata writes, credential-free storage.
- npm distribution (`db-snap` + per-platform binary packages) and GitHub
  Actions CI/release.
