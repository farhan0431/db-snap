---
name: dbsnap
description: Database time-machine for the local development database (instant physical snapshot / rollback) for SQLite and PostgreSQL. Use when about to run schema migrations, DDL (ALTER TABLE, DROP COLUMN), bulk data changes, reseed scripts, or ORM migrate commands — or when the database is broken/corrupted and must be restored. Covers the dbsnap CLI and the dbsnap_* MCP tools.
---

# dbsnap — database time-machine for this project

dbsnap takes **physical snapshots** of the local development database and
restores them bit-for-bit — like git commit / git checkout for data. Unlike
query-level undo, it works after `DROP COLUMN`, botched migrations, and broken
foreign keys, and it is safe to run **while the dev server is still running**.
Engines: SQLite (delta snapshots, milliseconds) and PostgreSQL (server-side
template clones, ~1 s with a brief connection blackout).

## Golden rules

1. **Snapshot BEFORE any risky change.** Before running migrations, DDL
   (`ALTER`/`DROP`), seed/fresh scripts, or bulk updates, create a snapshot:
   `dbsnap_create_snapshot` (MCP) or `dbsnap save "<what-you-are-about-to-do>"`
   (CLI). Pick a tag that names the coming change, e.g. `before-auth-migration`.
2. **Restore IMMEDIATELY when something breaks.** If a migration fails, the
   schema looks wrong, or data regressed: `dbsnap_restore_snapshot`
   (omitting the argument restores the most recent snapshot) or
   `dbsnap rollback`. The pre-restore state is automatically kept as a safety
   snapshot, so restoring is never a dead end.
3. **Check `dbsnap_status` first** if unsure which database you are touching.

## MCP tools (this session)

| Tool | Arguments | Use |
| :--- | :--- | :--- |
| `dbsnap_status` | — | Show bound database, engine, snapshot count, disk usage. |
| `dbsnap_create_snapshot` | `tag` (required, `[A-Za-z0-9._-]`) | Save a snapshot. Call before migrations/DDL/bulk changes. |
| `dbsnap_restore_snapshot` | `tag_or_id` (optional; omit = latest) | Restore. Immediately after failure; also to undo an experiment. |
| `dbsnap_list_snapshots` | — | List restore points (id, tag, time, size). |

CLI equivalents: `dbsnap save/list/restore/rollback/status/prune` (run in the
project root; `--db <path>` overrides the bound database).

## How the database is resolved

`--db` flag → `DATABASE_URL` from `.env` → auto-detect (`prisma/dev.db`,
`dev.db`, `*.sqlite`, …; PostgreSQL needs `DATABASE_URL` or `--db`, e.g.
`postgres://user@localhost:5432/app_dev`). The `.dbsnap/config.json`
fingerprint binds the project; operating on a **different** database is
refused (deliberate override: `--force`, or re-run `dbsnap init`). The MCP
server is **local-only** — it cannot target remote/production databases.

## Safety model (what you may rely on)

- Restores are verified (blake3 per chunk / per dump) before touching the
  live database; a corrupt snapshot is refused, never applied.
- Every restore first saves the current state as an `auto-before-restore-*`
  snapshot (kept: last 3). Quoted in the output as a one-command way back.
- Snapshots live in `.dbsnap/` (git-ignored); PostgreSQL clones live
  server-side as `__dbsnap_*` databases. Identical consecutive snapshots are
  linked — snapshotting "just in case" costs nothing.
- PostgreSQL note: a clone save/restore blocks NEW connections to the target
  database for about a second; the dev server reconnects on its own.

## Troubleshooting

- *"this project is bound to … but the target database is …"* → wrong
  directory or wrong `--db`. Confirm with `dbsnap_status`; only re-bind
  (`dbsnap init`) if the user asks.
- *"no snapshot matches …"* → run `dbsnap_list_snapshots` and use an exact id
  prefix or tag.
- *"database is locked"* → the dev server is mid-write; wait a moment and
  retry once. If it persists, ask the user to stop the server.
- Snapshot file missing → the catalog is out of sync; report to the user,
  do not delete `.dbsnap/` yourself.

## Example workflows

**Before a migration:**
`dbsnap_create_snapshot {tag: "before-users-migration"}` → run migration →
verify → done (snapshot stays as a restore point).

**After a failed migration:**
`dbsnap_restore_snapshot {}` (latest) → investigate → retry the migration.

**Exploratory data surgery:** snapshot `before-cleanup` → run risky SQL →
unsatisfied? `dbsnap_restore_snapshot {tag_or_id: "before-cleanup"}`.
