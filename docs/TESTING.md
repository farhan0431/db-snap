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
MariaDB can abort if rollback itself exhausts the shared tablespace. For an
explicit disposable Docker fixture, set `DBSNAP_TEST_STORAGE_CONTAINER` to its
container ID/name. The test verifies the image and published port, restarts the
same datadir with a larger bounded tablespace, then still requires successful
dbsnap recovery and the original data. `--exercise-server-restart` tests this
path even if the server stayed alive. This fixture repair is test orchestration;
Normal restore does not restart Docker or repair crashed database servers.
The separate explicit `sandbox --docker-image` command creates an independent
container; it never restarts the source database server.

The script exercises help/version, init, save, list/status (text and JSON), diff,
pin/unpin, prune --keep/--all, restore, rollback, and MCP session startup. It calls
all ten current MCP tools, verifies destructive-session denial, explicit declined and
accepted confirmations, checks data after restore, and verifies that missing
snapshots fail. PostgreSQL additionally exercises portable dump save/restore
and archive-to-archive offline diff (mixed clone/archive is explicitly refused). Binary/launcher platform selection is
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

## Source additions after v0.3.14

The CLI/MCP process script now exercises verify, doctor, exec success/failure,
retained live comparison, SQLite primary-key row counts, independent SQLite and
PostgreSQL clone sandboxes, and MySQL journal recover. It validates structured
MCP results and exact save IDs as well as human text. PostgreSQL's generated
sandbox is explicitly dropped by the test; user snapshots are not sandbox targets.

Additional Rust regressions verify corrupt-artifact refusal without modifying
the live database, sandbox overwrite refusal and independence, row update/add/
delete counts without values, preview limits, and interruption between prune
metadata commit and artifact cleanup. The next prune must empty the recorded GC
queue and retain an unrecorded sentinel file. `pending_gc` reserves retired IDs.
These additions require a newly built binary; published v0.3.14 is only used for
the separate installation smoke check. See DEVELOPMENT-WORKFLOW.md for limits.

PostgreSQL archive regressions extract schema and column-list COPY data offline,
compare identical/changed archives, reject corrupt input, preserve live row
counts and remove staging files on failure. A Windows-specific GC regression
holds an open chunk handle without delete sharing, checks that prune retains
the manifest and pending queue, then releases the handle and retries cleanup.

MySQL/MariaDB process jobs also use `--docker-sandbox`: import into a separate
container, assert network `none` and no published ports, modify its rows, check
that source rows are unchanged, and explicitly remove that exact container.
`DBSNAP_TEST_DOCKER_SANDBOX_IMAGE=mysql:8.0` (or `mariadb:11`) enables the
failed-replay cleanup regression. Run it separately from other sandbox tests:
it compares the labelled container inventory before and after the operation.
The command creates Docker resources only when the image option is explicit.

## v0.4.1 release validation

The release workflow also runs on `feature/snapshot-portability-and-diff`
for pre-release verification. It runs CI, builds six target binaries, packs
and installs the npm tarball, verifies the installed version, and runs the
SQLite CLI/MCP process suite against the installed launcher. Publication
requires a version tag.

The [pre-release run for commit 8a84ecf](https://github.com/farhan0431/db-snap/actions/runs/41775572622)
passed all nine CI jobs, all six release builds, and the CLI/MCP process
suite from the installed npm tarball. A local Windows tarball smoke test
also reported `dbsnap 0.4.1`.

The process suite interrupts export after writing the archive header. Import
must reject the partial archive and preserve the catalog and live data. The
PostgreSQL suite exits after dump replay and before sandbox registration. It
checks the source rows and registry, verifies the isolated copy, and drops
only the sandbox named by that test process. A real interruption can leave
an incomplete export file or an unregistered sandbox. Inspect the resource
before removing it and retry the command after cleanup.

The [final pre-release run for commit 1565dd9](https://github.com/farhan0431/db-snap/actions/runs/41778356056)
passed all nine CI jobs, six release builds, and the installed npm tarball
test. Its process tests include range and column filters, table-drop policy
failures, refusal to delete another project's Docker sandbox, an injected
archive disk-full error, and a terminated PostgreSQL replay connection.

## Row preview benchmark

Windows debug CLI, 8,000 rows per snapshot. Each measured command compares a
snapshot with itself and verifies zero changes. The baseline is a build
before the row preview optimizations. The filtered case selects 100 rows and
one non-key column. Medians include process startup and snapshot preparation.

| Engine and path | Baseline full | Updated full | Updated filtered | Runs per case |
| --- | ---: | ---: | ---: | ---: |
| SQLite delta snapshot | 2,125.4 ms | 828.7 ms | 68.2 ms | 7 |
| PostgreSQL 17 clone | 984.6 ms | 507.3 ms | 117.5 ms | 7 |
| MySQL 8.0 Docker dump | 44,975.5 ms | 27,934.9 ms | 19,696.6 ms | 3 |

The updated self-comparison prepares a snapshot once. A comparison between
two different snapshots still prepares both. MySQL times include creating
an isolated Docker server, replaying the dump, and removing the container.
Filters reduce the query work but do not avoid replay. The MySQL sample is
small and container startup varies. MariaDB has functional coverage in CI.

To reproduce, run `scripts/bench-row-preview.py --baseline OLD_BINARY
--binary NEW_BINARY --engine sqlite` with Python. Select `postgres` or
`mysql` and set the matching disposable loopback test URL for server
benchmarks. The MySQL benchmark requires Docker and the matching client
tools. Use `--runs 3` for the slower replay case.
