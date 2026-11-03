# dbsnap

**Database checkpoints for local development and AI coding agents.**

Save a snapshot before a migration or bulk edit, then restore the schema and
data if you need to roll back. Use dbsnap from your terminal or connect its
built-in MCP server to an AI assistant.

Supports **SQLite**, **PostgreSQL**, and **MySQL/MariaDB**. The npm package
bundles prebuilt binaries for Windows x64, macOS Intel and Apple Silicon, and
Linux x64 (glibc and musl) and ARM64 (glibc).

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

`dbsnap init` detects `DATABASE_URL` or common SQLite paths. To choose a
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
start it in the project you want to protect.

Restore and prune are disabled by default in MCP. To enable them for a server
session, start it with `dbsnap mcp --allow-destructive`. This is a session
setting, not a per-operation approval prompt.

`dbsnap init` also writes `.agents/skills/dbsnap/SKILL.md` in the project.
Whether that skill is loaded automatically depends on the AI client; the skill
file and the MCP server are separate integrations.

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
| `dbsnap diff before after` | Compare SQLite snapshot schemas and table row counts. |
| `dbsnap pin <tag-or-id>` | Protect a snapshot from pruning; use `unpin` to remove protection. |

`dbsnap list --json` and `dbsnap status --json` provide machine-readable
output. SQLite `diff` does not compare individual row contents; PostgreSQL and
MySQL/MariaDB snapshot diffs are not currently supported.
Pinned snapshots survive `prune`, including `--all`.

## Database requirements

| Database | Snapshot method | Requirements |
| --- | --- | --- |
| SQLite | Online backup and deduplicated chunks | No separate database client. |
| PostgreSQL | Server-side clone or `pg_dump` | Database creation privileges; PostgreSQL client tools for dump mode. |
| MySQL/MariaDB | Consistent logical dump | Compatible `mysql`/`mysqldump` or MariaDB client tools in `PATH`; transactional tables for consistent dumps. |

PostgreSQL server-side clones are stored on the database server. Use dump mode
when the snapshot needs to survive removal of that server's data volume.

## Safety

- MCP permits SQLite and loopback database hosts only. The CLI also permits
  private-network targets by default. A local target is not necessarily safe.
- Project binding checks that the current target matches the recorded database.
- Restore checks SQLite chunks and dump artifacts before applying them and, by default,
  creates a safety snapshot first. If that snapshot cannot be created, restore
  stops; the CLI has an explicit option to skip it.
- A PostgreSQL server-side clone has a metadata fingerprint, not a content
  checksum. dbsnap checks that the clone exists before restore.
- PostgreSQL restore is not atomic; an interruption can require manual
  recovery from a safety snapshot. The built-in PostgreSQL driver uses `NoTls`
  and does not yet support certificate-verified TLS.

dbsnap is intended for development workflows. Keep independent backups for
data you cannot afford to lose.

## Links

- [Repository and full documentation](https://github.com/farhan0431/db-snap)
- [MCP installation guide](https://github.com/farhan0431/db-snap/blob/main/INSTALL.md)
- [Report an issue](https://github.com/farhan0431/db-snap/issues)

MIT license.
