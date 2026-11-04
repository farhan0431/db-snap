# dbsnap

**Database checkpoints for local development and AI coding agents.**

Save a snapshot before a migration or bulk edit, then restore the schema and
data if you need to roll back. Use dbsnap from your terminal or connect its
built-in MCP server to an AI assistant.

Supports **SQLite**, **PostgreSQL**, and **MySQL/MariaDB**. The npm package
bundles prebuilt binaries for Windows x64, macOS Intel and Apple Silicon, and
Linux x64 (glibc and musl) and ARM64 (glibc).

**MySQL/MariaDB restore works on the original database.** Stop application
connections, writers, and scheduled events first. Restore is non-atomic and
requires a pinned safety snapshot. Interrupted restores must recover that
snapshot before another restore; see the recovery steps below.

## Install

Requires Node.js 16 or later.

Portable archives, server row previews, Git output, and PostgreSQL dump
sandboxes require dbsnap v0.4.1 or later.

```bash
npm install -g @farhan0431/db-snap
dbsnap --version
```

Or run without a global install:

```bash
npx -y @farhan0431/db-snap --help
```

## Migration rehearsals and CI

Use dbsnap to capture a restore point, run migrations against a disposable
development or staging database, and review schema and row changes before
rollout. The CLI runs in scripts, and the built-in MCP server gives coding
agents access to checkpoint and inspection tools.

Point your migration runner at the sandbox explicitly. Keep production
rollout and production backups in your existing deployment and recovery
systems. Use sanitized data in shared environments.

## Quick start

Run these commands in the root of the project containing your development
database:

```bash
dbsnap init
dbsnap status
dbsnap save "before migration"

# Run your migration or make changes here.

dbsnap list
dbsnap restore "before migration"  # restores the selected snapshot
```

Target resolution is `--db`, then process `DATABASE_URL`, then project `.env`,
then common SQLite paths. To choose a
database explicitly, use `dbsnap init --db ./prisma/dev.db`. Check the target
with `dbsnap status` before restoring: a restore changes the live database.

Snapshots and metadata live in `.dbsnap/` in your project. The directory is
added to `.gitignore` during initialization.

## Use with an AI assistant

Register dbsnap as an MCP server in a compatible client. For example, add
the following to your project's `.cursor/mcp.json`:

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

Restart your client and ask it to run `dbsnap_status` to confirm the database
before making changes. The server also exposes tools to create, list, restore,
and prune snapshots. It resolves the project from its working directory, so
start it in the project you want to protect. The current source exposes ten tools:

| MCP tool | Purpose |
| --- | --- |
| `dbsnap_status` | Inspect the target, engine, and snapshot storage. |
| `dbsnap_create_snapshot` | Save a snapshot, optionally supplying `tag`. |
| `dbsnap_list_snapshots` | List available restore points. |
| `dbsnap_diff` | Compare supported snapshots using `from` and `to`. |
| `dbsnap_restore_snapshot` | Restore a selected `tag_or_id`, or the newest snapshot. |
| `dbsnap_prune` | Prune with a positive integer `keep`; pinned snapshots survive. |
| `dbsnap_verify` | Check snapshot hashes/sizes/chunks, or clone existence; no SQL replay. |
| `dbsnap_doctor` | Report connectivity, journals, pending GC and untracked files. |
| `dbsnap_diff_live` | Save and retain a live checkpoint, then compare it with `from`. |
| `dbsnap_diff_rows` | Bounded primary-key row-change counts using `from`, `to`, and `table`. |

Results include text plus structured `ok`, `message`, optional typed `data` and
broad error codes. Snapshot creation returns the exact ID in data. Annotations
describe behavior but do not replace session permissions or elicitation.

Restore and prune are disabled by default in MCP. To enable them for a server
session, start it with `dbsnap mcp --allow-destructive`. Each operation requires
approval through an MCP client supporting elicitation; otherwise use the CLI.

To configure an explicitly enabled session, change the server arguments to
`["mcp", "--allow-destructive"]`. Enabling the session does not approve individual
operations. Your MCP client must still present and return the confirmation for
each restore or prune.

`dbsnap init` also writes `.agents/skills/dbsnap/SKILL.md` in the project.
Whether that skill is loaded automatically depends on the AI client; the skill
file and the MCP server are separate integrations.
Initialization preserves an existing skill file instead of overwriting local
customizations. Updating the installed binary does not refresh that existing
file; review it against the repository's current skill when upgrading.

## Commands

| Command | Purpose |
| --- | --- |
| `dbsnap init` | Bind the project to a database. |
| `dbsnap status` | Check the database and snapshot storage. |
| `dbsnap save [tag]` | Create a snapshot; the tag is optional. |
| `dbsnap list` | List snapshot IDs and tags. |
| `dbsnap restore [tag-or-id]` | Restore a snapshot; omit the selector for the latest. |
| `dbsnap rollback` | Restore the latest snapshot. |
| `dbsnap prune --keep 5` | Remove older snapshots and retain the newest five. |
| `dbsnap diff before after` | Compare SQLite, PostgreSQL clone, or MySQL/MariaDB dump snapshot schemas and row counts. |
| `dbsnap pin <tag-or-id>` | Protect a snapshot from pruning; use `unpin` to remove protection. |
| `dbsnap unpin <tag-or-id>` | Remove protection after reviewing the snapshot. |
| `dbsnap prune --all` | Delete all unpinned snapshots. |
| `dbsnap save tag --method dump` | Create a portable PostgreSQL dump snapshot. |
| `dbsnap verify [id-or-tag]` | Check one or all active snapshots; integrity/existence is not a restore test. |
| `dbsnap doctor` | Diagnose connection/recovery and report untracked files without deleting them. |
| `dbsnap recover` | Resume the recovery journal; ambiguous PostgreSQL states require inspection. |
| `dbsnap exec -- npm run db:migrate` | Checkpoint before a child command; retain it on failure, with no automatic rollback. |
| `dbsnap diff-live before` | Capture and retain the current state, then compare. |
| `dbsnap diff-rows before after --table users` | Bounded row-change counts by primary key, default limit 10,000 rows. v0.4.1 adds server preview and Git-style output. |
| `dbsnap sandbox before --output experiment.db` | Fork SQLite into a new file; existing files are never overwritten. |
| `dbsnap sandbox before` | Create an independent PostgreSQL clone, recorded in `sandboxes.json`. |
| `dbsnap sandbox before --docker-image mysql:8.0` | Explicit isolated MySQL server; use `mariadb:11` for MariaDB. Docker required. |
| `dbsnap sandbox-list` | List recorded sandboxes, their source snapshots, and resource state. |
| `dbsnap sandbox-remove CONTAINER_ID` | Remove a newly created Docker sandbox after verifying its project ownership. |
| `dbsnap diff-report before after --fail-on-drop-table` | Bounded JSON for automation. Exit 3 means a table was removed. |

`dbsnap list --json` and `dbsnap status --json` provide machine-readable output.
Pinned snapshots survive `prune`, including `--all`.

Use the recorded ID when undoing a specific change. Duplicate tags may be
ambiguous. `rollback` and `restore` without a selector choose the newest
snapshot, which can be an automatic safety snapshot from a previous restore.

## Snapshot diff

```bash
dbsnap save before
# Make your development changes.
dbsnap save after
dbsnap diff before after
```

Diff compares **snapshots**, not the current live database. SQLite compares
schema objects and row counts. PostgreSQL clone diff compares table/view
definitions and row counts. MySQL/MariaDB diff reads verified standard dumps
offline, including multiline `INSERT` data, and compares table definitions
and row counts. It does not replay SQL into the target.

Equal row counts do not mean equal data. Individual row contents are not
compared. PostgreSQL diff excludes indexes, constraints, routines and triggers;
MySQL/MariaDB diff excludes views, routines, triggers and events. Unsupported
INSERT formats produce an error. Version 0.4.0 and later can compare two
PostgreSQL dump archives offline through `pg_restore`: archive-wide schema
changes and table counts from column-list COPY data, excluding ownership,
privileges, row contents and sequence values. Mixed clone/archive diff is
unsupported.

`diff-rows` additionally supports SQLite added/removed/updated counts without
printing values. It requires explicit non-NULL primary keys and matching column
layouts, refusing previews above `--limit` (maximum 100,000). Server-engine row
diff is not implemented. `diff-live` uses storage and may briefly fence
PostgreSQL connections because it creates a regular retained snapshot.

## Checkpoints, diagnosis and sandbox experiments

`verify` and `doctor` print JSON. Verification failure exits nonzero. Doctor
reports a failed probe in its report and does not automatically repair anything.
`recover` reads the existing MySQL journal's ID or applies unambiguous PostgreSQL
journal recovery. It does not repair a stopped database server or choose an
arbitrary restore point. SQLite has no server recovery journal.

`exec` directly spawns a program without a shell, saving a checkpoint first.
Snapshot failure prevents execution. Child failure retains the checkpoint and
returns its ID, exit code and recovery command; the parent exits nonzero. Output
is returned up to 64 KiB per stream. `--db` selects the checkpoint database, not
the database your external program will use; set its environment deliberately.
Arbitrary command execution is CLI-only, not an MCP tool.

SQLite and PostgreSQL clone sandboxes let experiments target independent data.
Connect your migration/application explicitly to the returned file/database.
Cleanup is manual after closing sandbox connections. MySQL/MariaDB sandboxes
require explicit `--docker-image mysql:8.0` or `mariadb:11`: a separate Docker
server imports the verified dump with its original schema name, network disabled,
no published ports/host mounts/source credentials. Use `docker exec` for SQL
experiments and remove the returned container explicitly afterwards. Failed
imports remove only the newly created container. v0.4.1 also
supports sandbox replay from trusted PostgreSQL custom dumps. See the workflow
guide below.

### Portable archives and Git-style row previews

These commands require v0.4.1 or later.

```sh
dbsnap export before --output before.dbsnap
dbsnap import before.dbsnap --tag transferred
dbsnap diff-rows before after --table users --format git --show-values
```

- **Archive transfer.** Import verifies checksums and registers a new snapshot
  in the destination catalog. Restore is separate. The archive contains
  database contents, so keep it private and accept only trusted archives.
  PostgreSQL clone export requires matching `pg_dump`. MySQL/MariaDB requires
  the original database name at the destination.
- **Row preview.** Use a table with an explicit primary key. JSON and MCP
  return counts. Git output shows schema changes when layouts differ and can
  show stored row values when layouts match and `--show-values` is supplied.
  Treat that output as sensitive. The default limit is 10,000 rows per snapshot
  with a maximum of 100,000 and a 16 MiB encoded value budget per snapshot.
- **Server replay.** PostgreSQL row preview reads clones directly and requires
  `--replay-dumps` for trusted dumps. `dbsnap sandbox before` also accepts a
  trusted PostgreSQL custom dump. It needs matching `pg_restore`, database
  creation permission, and compatible extensions. The sandbox shares the
  PostgreSQL server. MySQL/MariaDB row preview requires an explicit
  `--docker-image mysql:8.0` or `--docker-image mariadb:11` and Docker.
  MCP replay requires session authorization and per-call approval. Abrupt
  termination can leave temporary resources that need cleanup.

Prune persists retired records in `metadata.json.pending_gc` and retries their
recorded artifact cleanup on a later prune. Shared survivor data and arbitrary
untracked files are preserved. Doctor reports pending GC and untracked staging.
Do not downgrade to an older binary unaware of `pending_gc` while it is nonempty.

## Recovering an interrupted MySQL/MariaDB restore

A fresh restore creates, syncs and catalogs a pinned safety snapshot before
cleanup. It preserves the database itself and replaces objects inside it.
`--no-safety-snapshot` is rejected for fresh MySQL/MariaDB restores.

If restore fails after the recovery journal is written:

1. Keep application writers/connections and scheduled events stopped.
2. Fix the reported disk-space, connection or privilege problem. Local free
   space checks do not measure the database server's storage.
3. Read the recovery ID in the error or `.dbsnap/mysql-restore.json`, then run:

   ```bash
   dbsnap restore RECOVERY_ID
   ```

The journal stores the recovery ID as a JSON string. Other restore IDs and
unpinning are refused while recovery is pending. Recovery uses the existing
pinned snapshot; it does not snapshot the partially restored target.
If the journal is malformed or the recovery copy is missing/corrupt, preserve
the files and investigate. Do not delete the journal to bypass recovery.

After success, safety snapshots remain pinned until you review and explicitly
unpin them. Restore remains non-atomic and requires trusted dumps; preflight
checks do not prevent another application from reconnecting during replay.

## Database support

| Database | Snapshot and restore | Preview and sandbox in v0.4.1 | Requirements |
| --- | --- | --- | --- |
| SQLite | Online backup with deduplicated chunks. Restore into the existing database file. | Primary-key row preview and sandbox as a new file. | An existing SQLite file. |
| PostgreSQL | Server-side clone for local iteration or custom dump for transfer. Dump restore replays in staging before cutover. | Read clone rows directly. Replay trusted custom dumps when explicitly requested. Create a sandbox from either kind. | Database creation privileges for server-side operations. Matching-major `pg_dump` and `pg_restore` for dumps. |
| MySQL/MariaDB | Logical dumps with routines, triggers, and events. In-place restore uses a pinned safety copy and journal. | Offline table schema/count diff. Use Docker for row preview and sandbox. | Compatible native clients on `PATH`, transactional tables for consistency, and Docker for replay in a sandbox. |

For PostgreSQL, keep clones on the server and use dumps to transfer snapshots
between machines. For MySQL/MariaDB, start with offline `diff`. Use
`diff-rows --docker-image mysql:8.0` or `--docker-image mariadb:11` when you
need row changes. The destination project must use the original database
name when importing MySQL/MariaDB archives. During in-place restore, stop
writers and scheduled events and preserve the recovery journal on failure.

## Safety

- MCP permits SQLite and loopback database hosts only. The CLI also permits
  private-network targets by default. A local target is not necessarily safe.
- Project binding checks that the current target matches the recorded database.
- Restore checks SQLite chunks and dump artifacts before applying them and, by default,
  creates a safety snapshot first. If that snapshot cannot be created, restore
  stops. Only SQLite/PostgreSQL allow explicit opt-out; fresh MySQL/MariaDB
  restores require a pinned safety snapshot.
- A PostgreSQL server-side clone has a metadata fingerprint, not a content
  checksum. dbsnap checks that the clone exists before restore.
- PostgreSQL restore is not atomic; an interruption can require manual
  recovery. Clone and staged dump restores journal their cutover.
- MySQL restore is in-place and non-atomic. Stop writers/events first. Visible
  target connections and enabled events cause refusal before cleanup; this is
  not a fence against future connections. Keep the pinned snapshot and recovery
  journal if replay fails, including when server storage is full.
- PostgreSQL network connections require verified TLS. Set `DBSNAP_PG_CA_CERT`
  to a trusted CA PEM file if needed; use `DBSNAP_PG_TLS=verify-full` to require
  TLS on loopback connections as well.

dbsnap is intended for development workflows. Keep independent backups for
data you cannot afford to lose.

## Links

- [Repository and full documentation](https://github.com/farhan0431/db-snap)
- [MCP installation guide](https://github.com/farhan0431/db-snap/blob/main/INSTALL.md)
- [Testing and audit coverage](https://github.com/farhan0431/db-snap/blob/main/docs/TESTING.md)
- [MySQL recovery](https://github.com/farhan0431/db-snap/blob/main/docs/MYSQL-RESTORE.md)
- [Development workflow and feature coverage](https://github.com/farhan0431/db-snap/blob/main/docs/DEVELOPMENT-WORKFLOW.md)
- [Report an issue](https://github.com/farhan0431/db-snap/issues)

MIT license.
