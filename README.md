# ⚡ dbsnap — Instant Database Time-Machine for AI Agents & Vibe Coders

> **"git commit / git checkout" for your local database.**
> Physical, DDL-proof SQLite snapshots in milliseconds, with a built-in
> **MCP server** so Cursor, Windsurf, and Claude Code can protect and heal
> your database themselves.

```bash
dbsnap save "before-auth-migration"   # commit point — instant
dbsnap rollback                       # oops → back to safety, instantly
```

## Why

Vibe coding means telling an AI agent things like *"refactor auth, run the
Prisma migration, reshuffle the seed data"*. Three things go wrong constantly:

1. **DDL is not undoable by queries.** When the agent runs `DROP COLUMN` or a
   botched migration, query-level undo cannot bring back the dropped column
   *and* its data. `dbsnap` snapshots the physical database file — everything
   comes back, schema and data, bit-for-bit.
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
npm i -g db-snap          # recommended — single Rust binary, no runtime deps
# or run without installing:
npx db-snap list
# or via cargo:
cargo install dbsnap
```

Supported today: **SQLite** (delta snapshots) and **PostgreSQL** (server-side
template clones + portable `pg_dump`). MySQL/MariaDB lands in **v0.3** — see
[docs/DESIGN.md](docs/DESIGN.md).

## CLI

```bash
dbsnap init                     # bind the project (finds DATABASE_URL / prisma/dev.db automatically)
dbsnap save "before-migration"  # save a tagged snapshot
dbsnap list                     # see every restore point
dbsnap restore "before-migration"
dbsnap rollback                 # restore the most recent snapshot
dbsnap prune --keep 5           # keep only the 5 newest
dbsnap status                   # binding, engine info, disk usage
```

`dbsnap` resolves your database in this order: `--db` flag → `DATABASE_URL`
from `.env` → common paths (`prisma/dev.db`, `dev.db`, `*.sqlite`, …).

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

or without a global install: `"command": "npx", "args": ["-y", "db-snap", "mcp"]`.

The agent gets four tools:

| Tool | What it tells the agent |
| :--- | :--- |
| `dbsnap_create_snapshot` | **MUST be called before** any migration, DDL, or bulk data change. |
| `dbsnap_restore_snapshot` | Call **immediately** when a migration fails or data looks wrong. Current state is kept as a safety snapshot first. |
| `dbsnap_list_snapshots` | Show available restore points. |
| `dbsnap_status` | Show which database is protected before touching anything. |

### Agent skill (automatic)

`dbsnap init` also installs an **agent skill** at
`.agents/skills/dbsnap/SKILL.md` — a short instruction file that AI editors
(Claude Code, Cursor, ZCode, …) pick up on their own, teaching them the
workflow: *snapshot before migrations, restore on failure*. Copy it to
`~/.claude/skills/dbsnap/SKILL.md` (or your agent's global skills folder) to
apply it across every project. The canonical copy lives in
[`skill/dbsnap/SKILL.md`](skill/dbsnap/SKILL.md).

## Safety guarantees (the "don't wreck my project" list)

- **Local-only by default.** Only `localhost` / private-network databases can
  be touched; anything else is refused unless you pass `--allow-remote`. The
  MCP server is hard-wired local-only — an agent cannot aim it at production.
- **Project binding.** `.dbsnap/config.json` fingerprints your database;
  restoring a snapshot into a *different* project's database is refused
  (`--force` to override deliberately).
- **Verify before restore.** Every snapshot is blake3-hashed on write and
  re-verified before a single byte touches your live database.
- **Automatic safety snapshot** before every restore — a failed restore always
  leaves a one-command way back.
- **Atomic everything.** Metadata writes are atomic, operations are guarded by
  a cross-process lock, and a killed process leaves no half-state.
- **No credentials stored.** Nothing in `.dbsnap/` ever contains your password.

## How it works

- **SQLite save:** Online Backup API → page-consistent copy → chunked into
  **64 KB page-aligned blocks** (blake3-hashed) → only chunks not already in
  the store are written (zstd). A snapshot is a small manifest of chunk
  hashes, so incremental snapshots cost kilobytes–megabytes, not the whole
  database. Unchanged databases are *linked* with zero I/O.
- **SQLite restore:** reassemble from the chunk store, verifying every
  chunk's hash → Backup API into the live database (works even on Windows
  while the file is open) → `PRAGMA wal_checkpoint(TRUNCATE)` so the dev
  server sees the new state immediately.
- **PostgreSQL save (clone, default):** `CREATE DATABASE __dbsnap_<id>
  TEMPLATE dev` — a block-level copy inside the server, done in ~1 s with a
  brief connection blackout (your dev server reconnects automatically).
  **`--method dump`:** `pg_dump -Fc` — slower but portable across machines.
- **PostgreSQL restore (clone):** fence the target (block new connections),
  clone the current state as an automatic safety snapshot, drop + rename in
  one step, unfence. Restore from a dump re-creates the database with its
  original encoding/locale and replays `pg_restore`.
- **Prune** is a real GC: chunks shared by surviving snapshots are never
  deleted; server-side clones of pruned snapshots are dropped too. Full
  design: [docs/DESIGN.md §3.5](docs/DESIGN.md).
- **Safety (PostgreSQL):** dbsnap only ever creates/drops/renames databases
  under the `__dbsnap_` prefix, always re-enables connections (even after a
  crash mid-operation), and hard-refuses remote hosts unless you pass
  `--allow-remote`.

## Development

```bash
cargo test        # 24 tests incl. live PostgreSQL (set DBSNAP_TEST_PG_URL)
cargo clippy --all-targets -- -D warnings
cargo build --release
scripts/e2e.sh      # SQLite: hot snapshot/rollback while a "dev server" writes
scripts/e2e-pg.sh   # PostgreSQL: clone snapshot, destructive DDL, rollback
```

## Roadmap

- [x] **v0.1.0** — SQLite engine, CLI, MCP server, npm distribution
- [x] **v0.1.5** — delta snapshots (content-addressed storage, 64 KB page-aligned chunks)
- [x] **v0.2.0** — PostgreSQL (server-side template cloning + `pg_dump`, fencing & healing)
- [ ] **v0.3.0** — MySQL/MariaDB (consistent logical dumps)

## License

[MIT](LICENSE)
