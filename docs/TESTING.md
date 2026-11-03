# Verification and regression coverage

Tests operate on disposable databases. CLI and MCP verification uses actual
processes; the stdio tests exchange JSON-RPC, respond to elicitation requests,
and assert database row counts rather than only checking exit codes.

## Run locally

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
node npm/test/launcher.test.js
cargo build --release
python scripts/test-cli-mcp.py --engine sqlite --binary target/release/dbsnap
```

On Windows use `target/release/dbsnap.exe`. For live servers, set
`DBSNAP_TEST_PG_URL` and/or `DBSNAP_TEST_MYSQL_URL` to a disposable loopback
server before `cargo test`. PostgreSQL requires matching-major `pg_dump`,
`pg_restore` and `psql`; MySQL/MariaDB requires the client tools on PATH.
Tests create their own unique databases. Do not point them at a production
server. The CLI/MCP script drops only the database that it successfully created.

```sh
python scripts/test-cli-mcp.py --engine postgres --binary target/release/dbsnap
python scripts/test-cli-mcp.py --engine mysql --binary target/release/dbsnap
# Unix: make the actual dump client write to the kernel ENOSPC sink.
python scripts/test-cli-mcp.py --engine mysql --binary target/release/dbsnap --disk-full
bash scripts/e2e.sh
bash scripts/e2e-pg.sh
bash scripts/e2e-mysql.sh
bash scripts/test-e2e-pg.sh
```

For a **separate dedicated** MySQL/MariaDB server with
`--innodb-file-per-table=OFF --innodb-data-file-path=ibdata1:12M:autoextend:max:128M`,
set `DBSNAP_TEST_MYSQL_STORAGE_URL` and run:

```sh
python scripts/test-mysql-storage-full.py --binary target/release/dbsnap
```

This script checks the bounded-server configuration before filling InnoDB,
requires replay to fail with full storage after journal commit, releases the
filler database, restores the pinned recovery snapshot and verifies the original
data. It must never run against a shared server. Its bounded tablespace avoids
exhausting the host filesystem. CI has a separate two-engine job for this test.

The script exercises help/version, init, save, list/status (text and JSON), diff,
pin/unpin, prune --keep/--all, restore, rollback, and MCP session startup. It calls
all six MCP tools, verifies destructive-session denial, explicit declined and
accepted confirmations, checks data after restore, and verifies that missing
snapshots fail. PostgreSQL additionally exercises portable dump save/restore
and the explicit refusal of dump diff. Binary/launcher platform selection is
tested separately by `npm/test/launcher.test.js`.

CI runs the SQLite CLI/MCP script on Linux/macOS/Windows and the PostgreSQL,
MySQL and MariaDB scripts on their service jobs. The MySQL/MariaDB jobs include
the `/dev/full` check. Adding these steps locally does not mean a new hosted CI
run has already completed.

## MySQL/MariaDB restore faults

`mysql_dump_roundtrip_and_failure_recovery` tests:

- Hash corruption before mutation and refusal of missing mandatory safety.
- Preservation of a sentinel in another database; refusal of incoming foreign keys.
- Repeated restore of tables/data, views, routines, functions, triggers and events.
- Removal of objects added after the snapshot.
- Injected OS disk-full errors before staging, safety creation, safety metadata
  commit, journal creation, replay, and after replay.
- Process exit after journal creation, cleanup, before replay and after replay.
- A real SQL syntax error after successful partial replay, followed by recovery.
- Pending-journal refusal of unrelated restores and unpinning; recovery to the
  original row count using the pinned snapshot.

Additional regressions check that a failed safety-file sync removes the new
uncatalogued artifact without changing the target, and that pending recovery
refuses an unpinned recovery snapshot.

`mysql_restore_refuses_active_clients_and_events` checks a live sleeping client
and, when the scheduler is ON, enabled scheduled events.

The disk-full fault boundaries return platform-native OS errors in child
processes. They are deterministic simulations, not full-volume tests.
`--disk-full` separately runs the real dump client with `/dev/full` as its
result file, asserting restore failure before target cleanup, unchanged
metadata/data, and staging cleanup. Some MySQL clients print `errno 0 on write`
instead of errno 28 for this failure. MariaDB fallback errors must not hide the
earlier write failure. Windows local QA can use a test adapter that forwards
the result-file to `/dev/full` inside a disposable Linux container; this is
selected explicitly with `DBSNAP_QA_REMOTE_DEV_FULL=1` and is not a product mode.

## Limits of this evidence

Passing tests do not prove power-loss durability, all possible disk failures,
OS ACL isolation, SQL sandboxing or race-free filesystem containment. The real
`/dev/full` test covers dump output. The separate bounded-tablespace test covers
actual InnoDB storage-full during replay and recovery after releasing space.
Neither test exhausts an entire physical host disk. Phase injections cover the recovery protocol but
do not model every server crash or network interruption. MySQL replay remains
in-place and non-atomic, requires trusted dumps, and requires stopped writers
and scheduled events. PostgreSQL clone hashes are metadata fingerprints, not
content checksums. TLS tests require their separate `DBSNAP_TEST_PG_TLS_*`
configuration; absent optional server URLs cause those tests to return early.

See [AUDIT-STATUS.md](AUDIT-STATUS.md) for findings that remain open and
[MYSQL-RESTORE.md](MYSQL-RESTORE.md) for recovery instructions.
Recorded local results are in [QA-REPORT.md](QA-REPORT.md).
