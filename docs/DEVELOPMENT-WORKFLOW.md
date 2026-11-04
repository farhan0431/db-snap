# Checkpoints, diagnosis and isolated experiments

These commands are implemented in the current source tree after v0.3.14. They
require a rebuilt/new release binary; installing published v0.3.14 alone does
not provide them yet.

## Diagnose and verify

```sh
dbsnap doctor
dbsnap verify             # all active catalogued snapshots
dbsnap verify SNAPSHOT_ID
```

Both commands return JSON. Doctor reports connectivity, recovery journals,
local free space, pending GC count and untracked files in snapshots/manifests/tmp.
It never deletes untracked files or applies recovery. It does not scan every
unreferenced CAS object or every server database. Missing initialization or a
binding/catalog mismatch produces an error rather than silently rebinding.

Verify checks every referenced chunk and its combined size for delta snapshots,
the hash/decompressed size for compressed snapshots, and hash/archive size for
PostgreSQL dump artifacts. Clone verification checks existence only. Each
result names its verification level and sets `restore_tested: false`. A bad
snapshot makes CLI exit nonzero; other records are still checked and reported.
These checks never replay SQL and do not prove that a restore will succeed.
Successful CLI reports are JSON on stdout; failed operations use the CLI error
channel and nonzero exit status. MCP callers should consume structuredContent
rather than assuming failed CLI stderr is bare JSON.

## Recover

```sh
dbsnap recover
```

On MySQL/MariaDB this reads the recovery ID from the journal and invokes the
same validated restore path as explicit restore. Stop writers/events and fix
space/privileges/connectivity first. Missing/malformed journals fail instead of
guessing an ID. PostgreSQL recovery runs only the existing unambiguous journal
repair; ambiguous states still require inspection. SQLite has no server recovery
journal, so use verify/doctor and select an explicit snapshot.

Recovery is a CLI operation. In MCP, use the existing approved restore tool
with the MySQL recovery snapshot ID; no unrestricted repair/exec tool is exposed.

## Run a risky command with a checkpoint

```sh
dbsnap exec -- npm run db:migrate
dbsnap exec --db ./dev.db -- python migrate.py
```

Exec saves a checkpoint before spawning the provided program directly, without
a shell. Snapshot failure prevents command execution. The JSON result includes
`snapshot_id`, child `exit_code`, output and an explicit recovery command. Child
failure makes dbsnap exit nonzero. There is **no automatic rollback**. Fix the
cause and restore only if that is the intended outcome.

Use explicit interpreters for scripts or shell builtins. On Windows, use e.g.
`dbsnap exec -- cmd /C npm run db:migrate` when the program is a command-file
shim or shell builtin.
Exec runs in the current project directory and inherits the environment. `--db` selects the
checkpoint target; it does not rewrite the child's DATABASE_URL or guarantee
that the child command targets that database. It does not make external commands
transactional or hold an exclusive application/database fence. Output is spooled
to guarded OS temporary files outside `.dbsnap` and returned up to 64 KiB per
stream after masking, with truncation flags;
process termination may leave OS temporary files requiring manual cleanup.

## Compare current state or row changes

```sh
dbsnap diff-live before-migration
dbsnap diff-rows before after --table users --limit 10000
```

Diff-live saves and retains a regular `compare-live-*` snapshot, then uses the
existing snapshot diff. Its JSON identifies the retained checkpoint. This is
not a read-only command: it uses storage and PostgreSQL cloning briefly fences
connections. Two PostgreSQL custom dump snapshots can also be compared offline
with `pg_restore`: archive-wide schema changes and table row counts from
column-list COPY output. Ownership, privileges, row contents and sequence values
are excluded. Mixed clone/archive diff is unsupported; diff-live captures a
clone on PostgreSQL, so comparing it to an archive fails but retains the new
checkpoint. Extraction errors fail explicitly. SQL is never replayed for diff.

Diff-rows compares compatible table layouts by explicit primary key. The default
JSON contains added, removed and updated counts. The limit defaults to 10,000
rows per snapshot (maximum 100,000). Previews exceeding the limit or the 16 MiB
encoded-value budget per snapshot fail with an error.

```sh
dbsnap diff-rows before after --table users --format git --show-values
dbsnap diff-rows before after --table users --column role --key-min 100 --key-max 199
dbsnap diff-rows before after --table public.users --replay-dumps
dbsnap diff-rows before after --table users --docker-image mysql:8.0
```

Git format uses +/- lines and groups changed columns by primary key. The
`--show-values` option prints stored values, so keep that output private. When
column layouts differ, Git format shows the schema changes and skips the row
comparison. MCP
returns counts and accepts `replay_dumps` and `docker_image` for trusted dumps
in an authorized session with per-call approval.
PostgreSQL reads ordinary tables from clones in read-only transactions and uses
temporary databases for dump replay. Specify `schema.table` for non-public
tables. MySQL/MariaDB uses a new network-isolated Docker server for each dump.
Temporary replay resources are removed on normal completion and errors.
Interrupted processes can leave managed resources behind. Server values are
compared using their serialized forms, so a spelling change in a
case-insensitive key may appear as a removal and an addition.

Repeat `--column` to compare only selected columns and the primary key. The
inclusive `--key-min` and `--key-max` bounds require one integer primary key.
SQLite bounds select rows whose stored key is an integer. The row and byte
limits apply after selection. Filters reduce row inspection work. Dump-based
engines still prepare the snapshot before running the filtered query.

For automation, run `dbsnap diff-report BEFORE AFTER --fail-on-drop-table`.
It writes a bounded JSON report with the exact snapshot IDs and table-drop
count. Exit 3 means the policy was violated, and exit 1 means the comparison
failed. The policy supports SQLite snapshots, PostgreSQL clones and standard
MySQL/MariaDB dumps. PostgreSQL archive reports compare COPY inventories and
reject the table-drop policy.

## Transfer a snapshot

```sh
dbsnap export before --output before.dbsnap
dbsnap import before.dbsnap --tag transferred
dbsnap verify transferred
```

Run import in the destination project with its own database binding. Import
registers a new unpinned snapshot with a new local ID. Restore is a separate
operation with its normal safety checks. The versioned archive verifies both
metadata and payload and contains snapshot contents. Keep it private and
accept archives from trusted sources. Hashes detect corruption and provide no
proof of authorship. Export materializes SQLite delta snapshots and converts
PostgreSQL clones to custom dumps. PostgreSQL needs matching native client
tools for clone export and compatible server extensions for replay.
MySQL/MariaDB destinations must use the original database name because the
dump contains database-qualified SQL.

## Sandbox

```sh
dbsnap sandbox before --output ./experiment.db  # SQLite
dbsnap sandbox before                          # PostgreSQL clone or trusted dump
dbsnap sandbox before --docker-image mysql:8.0  # MySQL: separate Docker server
dbsnap sandbox before --docker-image mariadb:11 # MariaDB: separate Docker server
```

SQLite reconstructs verified snapshot content into a new file and runs
`quick_check`; an existing destination is never overwritten. Use a separate
project/directory and database URL when running migrations against that file.
Delete it explicitly after closing sandbox connections when no longer needed.

PostgreSQL creates a separate `__dbsnap_sandbox_*` database from a catalogued
clone or verified custom dump, without fencing or replacing the live target.
Dump replay uses template0 and omits archived owners, grants and tablespaces.
Use trusted dumps and matching PostgreSQL client tools. The result names it; connect
to it explicitly for experiments. A local `sandboxes.json` registry records its
source/target. There is no automatic lifetime or cleanup: close clients and
explicitly drop that managed sandbox after use. A registry write failure can
leave a managed orphan; the error reports its name. Concurrent clients connected
to the source clone may prevent sandbox creation.

Sandbox means independent database contents, not an OS/network/credential
security boundary. Copied foreign-data wrappers, routines or connection settings
can still reference external resources. Use appropriately scoped credentials
and explicitly target the fork before running migrations.

For MySQL/MariaDB, explicit `--docker-image` creates a separate official
`mysql:8.0` or `mariadb:11` server with `network=none`, no published ports, no
host mounts, and no source credentials. The verified trusted dump keeps its
original database identifiers inside that server. The result reports the exact
container ID and cleanup command. Use `docker exec` and the matching client
inside the container for SQL experiments; no host application endpoint is
published. Docker and the chosen official image are required; Docker may pull
the image if absent. Failed initialization/replay removes only the container
created by that call. Successful containers are registered in sandboxes.json
and require explicit cleanup (`docker rm -f -v ID`). Abrupt process termination
can leave a container labelled `dbsnap.sandbox=true` for manual inspection.

Point your application at the sandbox explicitly before running migrations.

### Inspect registered sandboxes

Run `dbsnap sandbox-list` for JSON records with the source snapshot ID and
resource state. `dbsnap doctor` includes the same inventory. A file, database,
or labelled container can be `present`, `missing`, or `unknown` when its
backend cannot be checked. Docker status reflects the current Docker context.
A presence check provides no proof of ownership for deletion.

New SQLite sandbox files are registered. Older SQLite files and resources
left behind before registration require manual inspection. Close sandbox
clients before manual cleanup.

`dbsnap sandbox-remove CONTAINER_ID` stops and removes a Docker sandbox and
its anonymous volumes after checking the full registered container ID and
project ownership label. This applies to newly created MySQL/MariaDB
sandboxes. Earlier Docker sandboxes, SQLite files, and PostgreSQL sandboxes
require explicit manual cleanup.

## Resumable recorded garbage collection

Prune commits retired snapshot records into `metadata.json.pending_gc` while
removing them from the active catalog. It then deletes only their recorded,
unshared artifacts. A later prune retries remaining queue entries. Shared
survivor chunks/files and arbitrary untracked files are not deleted. Failed
chunk deletion preserves the victim manifest so references survive a retry.
Doctor reports the queue count; inspect errors before retrying prune.

The metadata remains version 2 with an optional field. Do not downgrade to a
binary unaware of `pending_gc` while that queue is nonempty; an old binary could
discard the field. Power-loss durability and concurrent malicious filesystem
mutation are still not proven by this retry mechanism.

## MCP results

The server exposes ten tools: the original six plus `dbsnap_verify`,
`dbsnap_doctor`, `dbsnap_diff_live`, and `dbsnap_diff_rows`. New comparisons use
`from` (and `to`, `table`, optional `limit` for row diff). Diagnostic verify takes
an optional `tag_or_id`. All responses retain text content and add a
`structuredContent` object containing `ok`, `message`, optional `operation`,
`data`, and stable broad error categories `OPERATION_FAILED`,
`INVALID_OR_DENIED`, or `APPROVAL_REQUIRED`. Save data includes the exact
`snapshot_id`; machine callers should use it instead of parsing prose.
Operation failures with a valid MySQL journal also include `recovery_id`, a
recovery stage and next-action guidance. Hints do not override the actual
recovery-time fingerprint, integrity, pinning or permission checks.

Tools advertise output schemas and annotations. Annotations are hints, not
permission enforcement. Restore/prune still require a destructive-enabled
session and per-call elicitation. Unsupported clients fail closed. Status/list/
diff are not labelled strictly read-only because their existing PostgreSQL
context can heal an interrupted journal. Arbitrary command execution is CLI-only.
