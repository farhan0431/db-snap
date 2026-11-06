# Migration rehearsal

`dbsnap rehearse` runs a trusted migration command against a new sandbox
from an existing SQLite or PostgreSQL snapshot. It captures before/after
snapshots and produces a bounded JSON schema/count report. The sandbox,
report and rehearsal snapshots are retained for inspection.

Save the source checkpoint first, then run the rehearsal from the application
directory. For a PostgreSQL SQL migration:

```bash
dbsnap save baseline
dbsnap rehearse baseline --fail-on-drop-table -- psql -X -w -v ON_ERROR_STOP=1 -f migrations/002.sql
```

For an ORM that reads `DATABASE_URL`:

```bash
dbsnap rehearse baseline --timeout-seconds 120 -- npx prisma migrate deploy
```

On Windows, specify `npx.cmd`. Verify that the application's configuration
uses the supplied environment connection. Hardcoded URLs, application
configuration, alternate environment variables, shadow databases and scripts
that override the environment can direct the command elsewhere.

## Connection and execution

- SQLite receives an absolute `file:` URL in `DATABASE_URL` and `DIRECT_URL`.
  `DBSNAP_SANDBOX_DB` contains the raw absolute filename for custom scripts.
- PostgreSQL receives a sandbox URL in `DATABASE_URL` and `DIRECT_URL`, and
  sandbox `PGHOST`, `PGPORT`, `PGUSER`, `PGDATABASE` and `PGPASSWORD`.
  `DBSNAP_SANDBOX_DB` is the database name. The source server credentials
  are reused through environment variables. Their privileges are unchanged.
- The child runs in the caller's application directory. Arguments are passed
  directly without shell expansion. Stdin is closed and stdout/stderr are
  discarded to keep credentials and unbounded child logs out of reports.
- The timeout defaults to 60 seconds and accepts 1–600 seconds. It covers
  the migrator, while snapshot capture, server calls and diff run separately.
  On timeout dbsnap attempts to terminate the process group on Unix or
  process tree on Windows. Descendants that detach may survive. A killed
  dbsnap process cannot guarantee child termination.

This executes a trusted program with the user's filesystem, network and
database permissions. The environment directs cooperative migrators to the
sandbox. It provides no OS isolation or guarantee against a command that
uses another database connection. PostgreSQL dump snapshots also contain
trusted SQL that is replayed on the selected development server.

## Reports and exit codes

Reports are printed as JSON and saved at
`.dbsnap/rehearsals/RUN/report.json`. They contain the source snapshot ID,
sandbox target without credentials, before/after snapshot IDs, migration
exit code, timeout flag, table-drop policy and comparison summary. The
before/after IDs belong to the rehearsal's nested catalog.

| CLI exit | Status |
| --- | --- |
| 0 | `passed` |
| 1 | Infrastructure failure, invalid request or incomplete report |
| 3 | `policy_failed` |
| 4 | `command_failed` |
| 5 | `timed_out` |

An unsuccessful migrator can still produce a diff of its partial changes.
An infrastructure failure after the command preserves its exit code and
timeout flag when available. A report left at `incomplete` indicates an
interrupted run. Reports omit the command and child output, return at most
50 removed-table names and 8192 summary characters, and mask registered
credentials. Schema definitions can contain sensitive literals.

`--fail-on-drop-table` detects removed tables. It does not prove data
integrity, preserve every row value, or establish deployment safety.
PostgreSQL uses clone comparisons covering tables/views and counts, with
the same limitations as `dbsnap diff`. The source catalog's restore points
are preserved by dbsnap. A trusted child can still modify them or source
data using its own permissions.

## Retention and cleanup

Use `sandbox-list` on the source project to find registered rehearsal
sandboxes. Each rehearsal also has a nested `.dbsnap` catalog. SQLite stores
its sandbox file in the run directory. PostgreSQL retains its sandbox and
before/after clone databases on the server.

Close sandbox clients, inspect the nested catalog, and prune its snapshots
using the sandbox connection from that rehearsal directory. Then manually
remove the exact sandbox file or PostgreSQL database after checking its
identity. Source-project prune does not clean nested rehearsal catalogs.
Crash termination between resource creation and registration can leave an
untracked resource for manual inspection. Automatic resource removal awaits
stronger ownership verification.

## MCP

`dbsnap_rehearse` accepts `snapshot`, `command` (an argv array), optional
`timeout_seconds`, and optional `fail_on_drop_table`. It requires
`dbsnap mcp --allow-destructive` and per-call elicitation approval displaying
the command and exact snapshot ID. A denied request or unsupported client
does not run the command. Failed rehearsals return `isError: true` with the
report in `structuredContent.data`.

MySQL/MariaDB rehearsal is unsupported in v0.4.4. Their Docker sandboxes
require a separate migrator connection/execution design.
