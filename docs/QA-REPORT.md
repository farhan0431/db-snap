# Local QA report — 2026-11-03

This report covers the working-tree changes after tag `v0.3.11`, including
snapshot diff and MySQL restore hardening. It is not a statement that a new
package has been published or that the project is fully security-audited.

## Environment and results

Windows native dbsnap binaries were tested against disposable Linux containers:
PostgreSQL 17, MySQL 8.0.46 and MariaDB 11.8.9. SQLite uses the bundled library.
Windows client adapters invoke actual MySQL/MariaDB clients in their containers;
PostgreSQL uses matching-major native client tools. The tests do not use real
user databases. Bounded storage-failure servers were created separately.

| Verification | Result |
| --- | --- |
| SQLite release CLI, all six MCP tools, JSON, diff, pin/unpin/prune, restore/rollback | PASS |
| PostgreSQL release CLI/MCP, clone diff, portable dump save/restore, refusal of dump diff | PASS |
| MySQL release CLI/MCP and offline table/count diff | PASS |
| MariaDB release CLI/MCP and multiline-dump diff | PASS |
| MCP disabled destructive session, explicit decline, explicit acceptance, data preservation | PASS on all four engines |
| SQLite E2E under a live WAL writer | PASS |
| PostgreSQL E2E clone rollback, portable dump, cleanup success/failure regression | PASS |
| MySQL E2E destructive column change and rollback | PASS |
| PostgreSQL TLS valid CA and wrong-hostname refusal | PASS |
| MySQL and MariaDB dump-output ENOSPC through actual `/dev/full` writes | PASS; target and metadata unchanged, staging cleaned |
| MySQL bounded InnoDB tablespace-full during replay, then space release and recovery | PASS |
| MariaDB bounded InnoDB tablespace-full during replay, then space release and recovery | PASS |
| Connected-client refusal and enabled-event refusal with scheduler explicitly ON | PASS on both engines |
| Process-exit and disk-full fault boundaries; partial SQL error and recovery | PASS on both engines |
| Pending recovery refuses a manually unpinned recovery snapshot | PASS on both engines |
| PostgreSQL/SQLite storage, binding, corruption, prune, locking and MCP regressions | PASS |
| Native Windows npm pack → local install → launcher `--version` | PASS |
| npm launcher's six platform-selection tests | PASS |
| Formatting, Clippy with warnings denied, release build, Python/YAML syntax | PASS |

The live regression run passed 25 unit tests and 43 integration test functions,
with PostgreSQL, TLS and MySQL configured. The four MySQL integration functions
also passed against MariaDB. The subsequent safety-sync/post-metadata-commit
regression passed against both engines: an uncommitted safety artifact is
removed, a committed artifact is preserved, and target data stays unchanged.
The final non-server suite passed 25 unit tests and 44 integration functions;
optional live-server cases return early in that run, with live results covered
by the preceding full/targeted runs. This distinction prevents counting skipped
server cases as additional live coverage.

## Restore behavior verified

Before mutation, hash/size validation and safety persistence failures leave the
target unchanged. A newly written recovery artifact is removed only when the
catalog confirms it was not committed. An uncertain/committed catalog preserves
the artifact. New recovery artifacts are synced; the pinned recovery artifact's
hash is checked before cleanup. Journal writes and removals sync directories on
Unix. Visible-client/event checks run before cleanup and again after safety save.

After journal commit, cleanup/replay errors retain the pinned recovery snapshot
and journal. Unrelated restore IDs and unpinning are refused. Actual partial
SQL replay, process exits at defined boundaries, real dump write failure, and
real server storage-full were tested. After fixing the storage condition,
recovery restores the original data. Errors from all dump flag attempts are
retained so a fallback compatibility error cannot hide an earlier write failure.

## Remaining boundaries

- MySQL/MariaDB replay is still in-place and non-atomic. Quiesce writers and the
  scheduler; preflight checks do not fence new connections or prove visibility
  of every other user. Trusted dumps are required; SQL is not sandboxed.
- Real bounded InnoDB exhaustion and `/dev/full` are tested; total host-disk
  exhaustion, power loss and every possible network/server failure are not.
- Process termination can leave temporary/orphan files. They are not blindly
  deleted. Cross-platform durable recovery and filesystem race containment are
  not proven.
- PostgreSQL clone hashes identify metadata, not database-content checksums.
  Ambiguous journal recovery still requires manual inspection.
- Diff compares schemas/counts, not individual row contents. PostgreSQL dump
  diff is unsupported. MySQL diff excludes views/routines/triggers/events;
  PostgreSQL diff excludes indexes/constraints/routines/triggers.
- Local results do not validate other OS binaries, external MCP client UIs,
  all npm platform artifacts, or the newly added hosted CI jobs. Elicitation was
  tested over real stdio using the test client, not through Cursor/Claude UI.

Reproduction commands and test semantics: [TESTING.md](TESTING.md).
Open audit findings: [AUDIT-STATUS.md](AUDIT-STATUS.md).

## Hosted release follow-up

The `v0.3.12` hosted run exposed two QA portability assumptions: MariaDB's
disk-full helper required `mariadb-dump` even when `mysqldump` was a supported
fallback, and the storage test expected the server to remain alive. On hosted
MariaDB 11.8.9, rollback at the shared-tablespace limit aborted the server.
dbsnap retained the recovery journal and pinned snapshot. The helper now uses
the same client-name fallback as the engine. Explicit Docker storage fixtures
can be restarted with expanded bounded storage before the test retries recovery;
the test still requires the original data to be restored. These fixes are
included in the final `v0.3.14` release; earlier tags are not rewritten.
The release also pins native macOS jobs to supported macOS 15 images and
normalizes npm package metadata using explicit UTF-8 encoding.
