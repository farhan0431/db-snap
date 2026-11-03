# AGENTS.md

Instructions for AI coding agents working **on this repository** (db-snap
itself). If you are instead looking to **install db-snap as a tool** for a
user: do NOT clone or build this repo — run `npm i -g db-snap` and register
the MCP server (see [INSTALL.md](INSTALL.md)).

## Build & verify

```bash
cargo build --release          # single binary at target/release/dbsnap(.exe)
cargo fmt --all -- --check     # CI enforces
cargo clippy --all-targets -- -D warnings   # CI enforces
cargo test                     # 29 tests; SQLite always runs
DBSNAP_TEST_PG_URL="postgres://postgres@localhost:5433/postgres" cargo test   # + PostgreSQL tests
DBSNAP_TEST_MYSQL_URL="mysql://root@127.0.0.1:3306/mysql" cargo test          # + MySQL/MariaDB tests
scripts/e2e.sh && scripts/e2e-pg.sh && scripts/e2e-mysql.sh   # live demos (release binary)
```

## Map

- `src/engine/` — one `DatabaseEngine` trait, three engines (`sqlite.rs`
  delta chunks, `postgres.rs` server-side clones, `mysql.rs` dumps).
  New engines implement the trait and register in `engine::make`.
- `src/storage/` — `.dbsnap/` catalog (`metadata.json` v2), content-addressed
  chunk store, zstd+blake3 pipeline.
- `src/ops.rs` — the ONLY command layer; CLI (`main.rs`) and MCP
  (`mcp/handler.rs`) must share it. All output passes `util::mask`.
- `src/config.rs` — URL resolution, fingerprinting, local-host guard.
- `docs/DESIGN.md` — architecture + threat model T1–T15. Read §4 before
  touching anything destructive.

## Ground rules (enforced in review)

1. **Do No Harm** — the safety model in `docs/DESIGN.md` §4 is non-negotiable:
   local-only default, fingerprint binding, verify-before-restore, automatic
   safety snapshots, drops restricted to the `__dbsnap_` prefix, prune only
   touches metadata-recorded artifacts.
2. **Credentials never travel** — passwords via `MYSQL_PWD`/`PGPASSWORD`
   env, never argv; never written to `.dbsnap/`; every new output path must
   go through `util::mask` (§4-T11).
3. `unsafe` is forbidden; clippy `-D warnings` and fmt must pass.
4. New behavior needs a test; changes to the safety model need a regression
   test that would fail without it.
5. npm package versions (`npm/`) must match `Cargo.toml`'s version.
