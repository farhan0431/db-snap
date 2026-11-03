# Installing dbsnap as an MCP server

Copy-paste instructions for every major client. If you are an AI agent
reading this: **install the published package, do not build from source** —
the npm package ships prebuilt Rust binaries for Windows, macOS, and Linux.

## Step 1 — install the CLI (one time)

```bash
npm i -g @farhan0431/db-snap
```

Alternative without a global install: everything below also works with
`npx -y @farhan0431/db-snap` in place of the `dbsnap` command.

Verify:

```bash
dbsnap --version
dbsnap status   # binds to the current project's database
```

`dbsnap` resolves the database from `--db` → `DATABASE_URL` in `.env` →
auto-detect (`prisma/dev.db`, `dev.db`, `*.sqlite`, …). PostgreSQL/MySQL
projects: set `DATABASE_URL`, e.g. `postgres://user@localhost:5432/app_dev`.

## Step 2 — register the MCP server

### Claude Code (one command)

```bash
claude mcp add dbsnap -- dbsnap mcp
```

### Cursor — `.cursor/mcp.json` (project) or `~/.cursor/mcp.json` (global)

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

### Windsurf — `~/.codeium/windsurf/mcp_config.json`

Same JSON as Cursor.

### Claude Desktop — `claude_desktop_config.json`

Same JSON as Cursor (Settings → Developer → Edit Config).

### Any MCP client, no global install

```json
{
  "mcpServers": {
    "dbsnap": {
      "command": "npx",
      "args": ["-y", "@farhan0431/db-snap", "mcp"]
    }
  }
}
```

### Custom database (optional)

```json
{
  "mcpServers": {
    "dbsnap": {
      "command": "dbsnap",
      "args": ["mcp", "--db", "postgres://user@localhost:5432/app_dev"]
    }
  }
}
```

## Step 3 — verify

Restart the client, then check the five tools appear:
`dbsnap_status`, `dbsnap_create_snapshot`, `dbsnap_restore_snapshot`,
`dbsnap_list_snapshots`, `dbsnap_prune`. Call `dbsnap_status` once — it prints which local
database is protected.

> Security note: the MCP server is hard-wired **local-only**. It cannot
> target remote/production databases, and it never exposes database
> credentials in tool results.

## CLI cheat sheet (no MCP needed)

```bash
dbsnap save "before-migration"   # snapshot before risky changes
dbsnap rollback                  # instant undo to the newest snapshot
dbsnap restore "before-migration"
dbsnap list
dbsnap prune --keep 5
```

## Troubleshooting

- **`dbsnap: command not found`** — the npm global bin folder is not on
  `PATH`; reinstall with `npm i -g @farhan0431/db-snap` or use the `npx` variant.
- **No database found** — run `dbsnap init --db <path-or-url>` in the
  project, or set `DATABASE_URL` in `.env`.
- **"this project is bound to …"** — the project is already bound to
  another database; run `dbsnap status` in the right directory.
- **Windows says the binary is blocked** — the binaries are signed-zip-free
  but not Authenticode-signed; unblock via file Properties if SmartScreen
  complains.
