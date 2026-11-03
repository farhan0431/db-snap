# Changelog

All notable changes to this project are documented in this file.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- Agent/repo discoverability for MCP installation: `llms.txt` (LLM-readable
  install summary), `AGENTS.md` (conventions for agents working on the
  repo), `INSTALL.md` (copy-paste MCP configs for Claude Code, Cursor,
  Windsurf, Claude Desktop, and generic clients), and a top-of-README
  install block.

## [0.3.0] — 2026-11-03

### Added
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
