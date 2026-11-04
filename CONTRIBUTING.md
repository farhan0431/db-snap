# Contributing to db-snap

Thanks for helping make database time-travel safer for vibe coders. This
document covers the basics; the architecture lives in
[docs/DESIGN.md](docs/DESIGN.md).

See the [maintainer roadmap](docs/MAINTAINER-ROADMAP.md) for release
checklists and proposed features.

## Development setup

```bash
git clone https://github.com/farhan0431/db-snap
cd db-snap
cargo build            # bundles SQLite, no other system deps
cargo test             # unit/integration tests; live engines need test URLs
```

### Testing against real servers

SQLite tests run everywhere. PostgreSQL/MySQL tests activate when the
matching environment variable points at a disposable local server (unique
test databases are created and dropped automatically):

```bash
export DBSNAP_TEST_PG_URL="postgres://postgres@localhost:5433/postgres"
export DBSNAP_TEST_MYSQL_URL="mysql://root@127.0.0.1:3306/mysql"
cargo test
```

End-to-end scripts (release binary + real destructive migrations):

```bash
cargo build --release
scripts/e2e.sh       # SQLite: hot snapshot/rollback under a live writer
scripts/e2e-pg.sh    # PostgreSQL: clone, DROP COLUMN, rollback
scripts/e2e-mysql.sh # MySQL/MariaDB: dump, DROP COLUMN, rollback
python scripts/test-cli-mcp.py --engine sqlite --binary target/release/dbsnap
python scripts/test-cli-mcp.py --engine postgres --binary target/release/dbsnap
python scripts/test-cli-mcp.py --engine mysql --binary target/release/dbsnap --disk-full
```

Use `.exe` on Windows. See [docs/TESTING.md](docs/TESTING.md) for full CLI/MCP
coverage, safe disposable-server setup, real ENOSPC checks, and fault-injection
boundaries. MySQL restoration remains non-atomic; stop writers and scheduled
events before restore. Do not equate a green test run with production readiness.

## Ground rules

- **Do No Harm first.** Anything that could touch a database must respect
  the safety model in `docs/DESIGN.md` §4 (local-host guard, project
  fingerprint, verify-before-restore, `__dbsnap_*`-only drops, automatic
  safety snapshots). A PR that weakens any of these needs a very good
  argument.
- **Credentials never travel.** Passwords go to clients via environment
  variables, never argv; every output path goes through
  `util::mask` (see §4-T11). If you add an output path, it must be redacted.
- `cargo clippy --all-targets -- -D warnings` and `cargo fmt --all -- --check`
  must pass (CI enforces both).
- New behavior needs a test. Regression tests for the safety model are
  especially welcome.
- Keep `unsafe` out — it is forbidden at the crate level.

## Pull requests

1. Branch from `main`.
2. Make your change with tests.
3. `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`.
4. Open the PR with a short description of the behavior change and the risk
   it touches (engine, storage, MCP, distribution).

## Reporting bugs

Open a GitHub issue with: dbsnap version (`dbsnap --version`), engine +
server version, the command you ran, and the output with any credentials
already masked. For security issues, see [SECURITY.md](SECURITY.md) — do not
open a public issue.
