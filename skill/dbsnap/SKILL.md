---
name: dbsnap
description: Checkpoint, compare, transfer, and restore local development databases with the dbsnap CLI or MCP tools. Use before migrations, schema changes, resets, or bulk edits, and when rehearsing changes in a sandbox or recovering a failed restore.
---

# dbsnap

Use dbsnap for SQLite, PostgreSQL, and MySQL/MariaDB development databases.
Run it from the intended project. Confirm the target and engine with
`dbsnap_status` or `dbsnap status`. A loopback address alone does not make a
database safe to modify. Resolve catalog binding errors before proceeding.

Prefer the tools advertised by the current MCP session. For CLI use, check
`dbsnap --version` and the relevant command's `--help`. If installation is
needed, the published package is `npm i -g @farhan0431/db-snap`. Portable
archives, server row previews, Git output, and PostgreSQL dump sandboxes
require v0.4.1 or later.

## Checkpoint before a change

1. Before migrations, DDL, seed/reset scripts, or bulk edits, call
   `dbsnap_create_snapshot` with a descriptive `tag`, or run
   `dbsnap save before-migration`. Stop the risky operation if save fails.
2. Retain the exact snapshot ID from `structuredContent.data.snapshot_id` in
   MCP, or from the CLI result. Use `dbsnap_list_snapshots` or
   `dbsnap list --json` if a tag is ambiguous.
3. After the change, check the expected schema and application data. Use
   `dbsnap_diff` with `from` and `to`, or `dbsnap diff BEFORE AFTER`, for
   schema and table counts. Equal counts can hide row changes.

CLI `dbsnap exec -- PROGRAM ARGS...` saves before starting the child process.
Set the migration runner's database URL explicitly. The child inherits its
environment and dbsnap's `--db` only selects the checkpoint target. Child
failure leaves a checkpoint for explicit recovery. `diff-live` creates and
retains a snapshot of the current state. PostgreSQL clone capture briefly
fences database connections.

## Choose a preview

- Use `dbsnap_verify` or `dbsnap verify ID` to check artifacts before relying
  on them. PostgreSQL clone verification checks existence only. Use
  `dbsnap_doctor` or `dbsnap doctor` to inspect pending recovery and cleanup.
- Use `dbsnap_diff_rows` with `from`, `to`, and `table`, or run
  `dbsnap diff-rows BEFORE AFTER --table TABLE`. Counts require an explicit
  primary key and matching column layouts. PostgreSQL defaults to `public`
  and accepts `schema.table` for ordinary tables. The default limit is 10,000
  rows per snapshot. Set `limit` in MCP or `--limit` on the CLI up to
  100,000. The encoded value budget is 16 MiB per snapshot.
- Use `--format git --show-values` only when the user wants stored values
  displayed. Keep terminal output private. Git output shows schema changes
  instead of row changes when column layouts differ. MCP returns counts.
- For trusted PostgreSQL dumps, opt in with CLI `--replay-dumps` or MCP
  `replay_dumps: true`. For trusted MySQL/MariaDB dumps, use CLI
  `--docker-image` or MCP `docker_image` with `mysql:8.0` or `mariadb:11`.
  MySQL/MariaDB replay starts temporary Docker servers with networking
  disabled. Keep replay and Docker use within the user's authorized scope.
  Ordinary `diff` stays offline for dump
  comparison and PostgreSQL requires two snapshots of the same kind.

## Rehearse or transfer

| Task | CLI | What to check |
| --- | --- | --- |
| SQLite sandbox | `dbsnap sandbox ID --output experiment.db` | Use a new file. Point the app or migration runner at that file. |
| PostgreSQL sandbox | `dbsnap sandbox ID` | Accepts a clone or trusted custom dump. Dump replay needs matching `pg_restore` and compatible extensions. The sandbox shares the server and may reach resources through routines or extensions. |
| MySQL/MariaDB sandbox | `dbsnap sandbox ID --docker-image mysql:8.0` | Use `mariadb:11` for MariaDB. No published port. Connect inside the container with `docker exec`. |
| Transfer | `dbsnap export ID --output snapshot.dbsnap`, then `dbsnap import snapshot.dbsnap --tag transferred` | Import in the destination project with its own binding. It verifies and registers a new snapshot. PostgreSQL clone export needs matching `pg_dump`. MySQL/MariaDB requires the original database name. |

Keep archives and snapshots private. Use trusted inputs and sanitized
fixtures in shared environments. Checksums detect corruption. A restore is
a separate operation. Retain the created sandbox identifier for explicit
cleanup. PostgreSQL sandbox names are recorded in `.dbsnap/sandboxes.json`.
An interrupted replay can leave temporary resources behind.

## Restore and recovery

Restore replaces current data. Confirm the intended target and use an exact
snapshot ID. Keep the automatic safety snapshot enabled. A bare `rollback`
selects the newest snapshot, which can be an earlier safety copy.

MCP restore, prune, and dump replay require a session started with
`dbsnap mcp --allow-destructive` and per-call approval via MCP elicitation.
Respect the user's existing authorization and the host's approval result.
If elicitation is unavailable, explain the limitation. Use the CLI only
within the user's authorized scope.

- **MySQL/MariaDB.** Stop application connections, writers, and scheduled
  events before restore or recovery. Preflight cannot fence new connections.
  Restore is in place and non-atomic. It requires a pinned safety snapshot
  and trusted SQL. If recovery is pending, preserve the journal and pinned
  snapshot, fix the cause, then use CLI `dbsnap recover` or approved MCP
  restore with the exact reported `recovery_id`.
- **PostgreSQL.** Clones live on the server's data volume. If cutover is
  interrupted, inspect the recovery state and use CLI `dbsnap recover` for
  unambiguous journal recovery. Ambiguous states require inspection.
- **Cleanup.** Keep recovery copies pinned until recovery is verified.
  Prune works on recorded artifacts and retries pending cleanup. Leave
  untracked artifacts and journals intact while diagnosing an error.

For migration rehearsal, target development or staging. Keep production
rollout and production backup and recovery in the existing deployment and
database systems.
