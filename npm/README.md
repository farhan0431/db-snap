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

```bash
npm install -g @farhan0431/db-snap
dbsnap --version
```

Or run without a global install:

```bash
npx -y @farhan0431/db-snap --help
```

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
start it in the project you want to protect. The six tools are:

| MCP tool | Purpose |
| --- | --- |
| `dbsnap_status` | Inspect the target, engine, and snapshot storage. |
| `dbsnap_create_snapshot` | Save a snapshot, optionally supplying `tag`. |
| `dbsnap_list_snapshots` | List available restore points. |
| `dbsnap_diff` | Compare supported snapshots using `from` and `to`. |
| `dbsnap_restore_snapshot` | Restore a selected `tag_or_id`, or the newest snapshot. |
| `dbsnap_prune` | Prune with a positive integer `keep`; pinned snapshots survive. |

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
INSERT formats produce an error. PostgreSQL dump archives are not supported.

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

## Database requirements

| Database | Snapshot method | Requirements |
| --- | --- | --- |
| SQLite | Online backup and deduplicated chunks | No separate database client. |
| PostgreSQL | Server-side clone or `pg_dump` | Database creation privileges; PostgreSQL client tools for dump mode. |
| MySQL/MariaDB | Consistent logical dump | Compatible `mysql`/`mysqldump` or MariaDB client tools in `PATH`; transactional tables for consistent dumps. |

PostgreSQL server-side clones are stored on the database server. Use dump mode
when the snapshot needs to survive removal of that server's data volume.
For dump save and restore, install `pg_dump` and `pg_restore` matching the
PostgreSQL server's major version. Mismatched tools are rejected before restore.
The SQLite database file must already exist. MySQL/MariaDB snapshots retain
routines, triggers and events, although diff does not compare those objects.
Snapshot runtime and connection disruption depend on the engine and data size.

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
- [Report an issue](https://github.com/farhan0431/db-snap/issues)

MIT license.
