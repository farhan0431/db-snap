---
name: dbsnap
description: Save, inspect, compare, and restore development database snapshots with the dbsnap CLI or MCP tools. Use before migrations, DDL, seeds, or bulk edits, and when recovering a failed database change. Supports SQLite, PostgreSQL, and MySQL/MariaDB.
---

# dbsnap — development database checkpoints

Use an installed `dbsnap` binary or tools advertised by the current MCP session.
If installation is needed, use `npm i -g @farhan0431/db-snap`; tool users do not
need to build the repository. Run commands in the intended project directory.
Never assume that a loopback database is disposable.

## Before changing data

1. Check `dbsnap_status` or `dbsnap status` to establish the target and engine.
   Resolve binding mismatches first. `--force` cannot reuse another database's
   catalog; do not edit fingerprints to bypass this check.
2. Save before migrations, DDL, seed/reset scripts, or bulk changes. Use a
   descriptive tag such as `before-users-migration`. If save fails, stop the
   risky operation and fix the snapshot failure first.
3. Keep the returned snapshot ID. After the change, verify the expected schema
   and application data. Snapshot diff adds schema/count evidence, not proof
   that every row is correct.

## Tools and commands

| MCP tool | Arguments | CLI equivalent |
| --- | --- | --- |
| `dbsnap_status` | none | `dbsnap status` |
| `dbsnap_create_snapshot` | optional `tag` | `dbsnap save "before-users-migration"` |
| `dbsnap_list_snapshots` | none | `dbsnap list` |
| `dbsnap_diff` | `from`, `to` (IDs or tags) | `dbsnap diff before after` |
| `dbsnap_restore_snapshot` | optional `tag_or_id` | `dbsnap restore SNAPSHOT_ID` |
| `dbsnap_prune` | positive integer `keep` | `dbsnap prune --keep 5` |
| `dbsnap_verify` | optional `tag_or_id` | `dbsnap verify [SNAPSHOT_ID]` |
| `dbsnap_doctor` | none | `dbsnap doctor` |
| `dbsnap_diff_live` | `from` | `dbsnap diff-live before` |
| `dbsnap_diff_rows` | `from`, `to`, `table`, optional `limit` | `dbsnap diff-rows before after --table users` |

These additional tools/commands require a newer binary than published v0.3.14.
Inspect advertised tools and `--help`; do not assume support from this skill alone.
Results include structured `ok`/`message`, optional `data`, and broad failure codes.
Snapshot creation exposes its exact `snapshot_id` in data; retain that ID.

Tags allow letters, digits, spaces, `.`, `_`, and `-`, up to 64 characters.
Duplicate tags can be ambiguous; list snapshots and use an unambiguous ID.
CLI also supports `init`, `rollback`, `pin`, `unpin`, `list --json`, and
`status --json`. Pinning protects a snapshot from prune, including `prune --all`.
Do not prune or unpin recovery copies as an incidental cleanup step.

Use `verify` for artifact integrity, never as proof of successful restore.
PostgreSQL clone verification is existence-only. Use `doctor` to inspect
connectivity, pending journals, pending GC and untracked files without deleting
them. CLI `recover` follows an existing MySQL/PostgreSQL journal; it does not
repair the server or resolve ambiguous PostgreSQL states automatically. In MCP,
use the approved restore tool with the exact MySQL recovery ID.

CLI `exec -- PROGRAM ARGS...` saves a checkpoint before direct execution and
never automatically restores when a command fails. It inherits the environment;
`--db` does not retarget the external program. Do not expose arbitrary commands
as an MCP substitute. Review child exit code and the retained checkpoint ID.

`diff-live` intentionally creates and retains a regular live snapshot before
comparison, with the engine's usual storage/disruption cost. `diff-rows` compares
SQLite primary-key added/removed/updated counts only; it returns no values,
requires matching layouts, and fails above its row limit. Server row diff is
unsupported. Do not infer equal data from an ordinary schema/count diff.

For isolation, CLI `sandbox ID --output NEW_FILE` creates an independent SQLite
file; `sandbox ID` creates a PostgreSQL clone and records it in `sandboxes.json`.
Target experiments explicitly at that result and arrange manual cleanup.
MySQL/MariaDB additionally supports explicit CLI `sandbox ID --docker-image
mysql:8.0` or `mariadb:11`. This requires Docker, starts a separate server with
network disabled and no host mounts/ports/source credentials, and returns its
container ID. Use docker exec for SQL experiments; explicitly remove the
successful container after use. Do not silently pull/start containers outside
the user's authorized scope. PostgreSQL dump sandbox and same-server MySQL SQL
rewriting remain unsupported.

Prune retries only recorded `pending_gc` artifacts after interruptions. Arbitrary
untracked files remain for inspection. Do not downgrade an active catalog to an
older binary that would discard the optional pending-GC metadata field.

## Restore and authorization

Restore replaces current data. Establish the intended target and snapshot and
respect the user's existing authorization; clarify only when that scope is
missing or unclear. Prefer the recorded ID for the operation being undone.
Omitting the selector, or using `rollback`, chooses the newest snapshot, which
may be an automatic safety snapshot from an earlier restore.

MCP restore/prune require `dbsnap mcp --allow-destructive` and per-call approval
through a client supporting MCP elicitation. Existing conversational permission
does not bypass this protocol check. If unavailable, explain the limitation and
use the CLI only within the user's authorization. Do not silently enable
destructive access in the client's configuration.

Keep safety snapshots enabled. A successful safety save does not guarantee
atomic restoration. If restore fails, report the error and recovery ID; resolve
its cause before retrying. Do not repeatedly restore against a partial target
or delete journals, catalog entries, or snapshot files to clear the error.

## Engine-specific behavior

| Engine | Save/restore behavior | Diff scope |
| --- | --- | --- |
| SQLite | Existing file; bundled Online Backup API and verified delta chunks | Tables, indexes, views, triggers, and table row counts |
| PostgreSQL | Default server-side clone; CLI `save --method dump` uses matching-major clients | Clone table/view/counts; source after v0.3.14 also compares two dump archives offline |
| MySQL/MariaDB | Logical dump; InnoDB for consistent transactional snapshots; restore to original target | Standard dbsnap dump table definitions and row counts, read offline |

Diff never compares individual row contents. PostgreSQL diff excludes indexes,
constraints, routines and triggers; MySQL diff excludes views, routines, triggers
and events. Equal counts do not imply equal data. Report unsupported formats or
snapshot types rather than replaying dumps into the live database to inspect them.
Source archive diff uses pg_restore without a database connection, reporting
archive-wide schema changes and column-list COPY row counts. Ownership,
privileges, row contents and sequence values are excluded. Mixed clone/archive
comparisons fail explicitly. PostgreSQL live capture uses a clone, so diff-live
from a dump is unsupported and retains the live checkpoint for inspection.

PostgreSQL cloning briefly fences and disconnects target connections; duration
depends on data and server load. Clones disappear with the server's data volume.
Clone fingerprints are metadata fingerprints, not content checksums. Interrupted
cutover can require manual inspection of `postgres-restore.json`; do not drop
preserved databases or consume catalogued clones manually.

### MySQL/MariaDB recovery

- Stop application connections, writers and scheduled events before restore or
  recovery. Visible connections/enabled events cause refusal, but preflight does
  not fence new connections. Coordinate server-wide scheduler changes or killing
  connections within the user's authorization; do not change them silently.
- Restore is in-place and non-atomic. It preserves the database itself, replaces
  its objects, and requires a pinned safety snapshot. Fresh restore rejects
  `--no-safety-snapshot`. Dumps must be trusted; they are executable SQL.
- If `.dbsnap/mysql-restore.json` exists, read its recovery ID and restore that
  exact ID. Resolve disk, connection or privilege failures first. Other restore
  IDs and unpinning are refused while recovery is pending. Recovery reuses the
  existing pinned copy instead of saving the partially restored state.
- A full server disk can also prevent recovery. Free space through an authorized
  operation before retrying; do not delete the recovery artifact or journal.
  A malformed journal or missing/corrupt recovery copy requires investigation.
- Safety copies remain pinned after success. Review them before explicitly
  unpinning; they are not part of automatic unpinned-snapshot retention.

## Target resolution and storage

Resolution is `--db` → process `DATABASE_URL` → project `.env` → SQLite discovery.
Server databases need a URL. MCP accepts SQLite and loopback hosts only; the CLI
also accepts private-network hosts by default. PostgreSQL non-loopback connections
require verified TLS; use `DBSNAP_PG_TLS=verify-full` for loopback TLS and
`DBSNAP_PG_CA_CERT` for an additional trusted PEM CA.

`.dbsnap/` contains the catalog and file artifacts and is added to `.gitignore`.
PostgreSQL clones reside on the server. Connection credentials are excluded from
catalog/config output, but snapshots can contain sensitive application data.
Do not commit or share them casually. Keep independent backups for valuable data;
dbsnap does not provide disaster recovery or proven power-loss durability.
