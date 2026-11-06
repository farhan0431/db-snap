# Maintainer roadmap

Use this checklist to track implementation and release verification. The README and development workflow describe available features and their limits.

## v0.4.1 release checklist

- [x] Portable archive export/import, server row previews, and Git-style CLI diff
- [x] Sandboxes from trusted PostgreSQL custom dumps
- [x] Test partial archive rejection and PostgreSQL interruption after replay
- [x] List registered sandbox resources and report their state in `doctor`.
  Remove newly registered Docker sandboxes only after verifying the exact
  container ID and project label. SQLite and PostgreSQL cleanup is manual.
- [x] Produce a bounded JSON migration report and an opt-in table-drop
  policy. The policy covers SQLite, PostgreSQL clones, and standard
  MySQL/MariaDB dumps.
- [x] Filter row previews by column and single-integer primary-key range.
  Keep the row and byte budgets and the counts-only default.
- [x] Document a design for sanitized fixture export and test archive disk
  errors, process interruption, and a lost PostgreSQL replay connection.
- [x] Validate hosted CI, six release builds, and the installed npm tarball.

The [final pre-release run](https://github.com/farhan0431/db-snap/actions/runs/41778356056)
validated the expanded code at `1565dd9`. The sanitization design is
exploratory. Verify ownership for automatic SQLite and PostgreSQL removal
in a future change.

### Sanitized fixture export design

Start with SQLite. Copy a verified snapshot into temporary storage, apply an
explicit per-table and per-column policy, then validate foreign keys, unique
constraints, and the resulting database before packaging it. Keep the source
snapshot unchanged. Omit columns or tables only when the policy accounts for
their dependencies. Never fall back to exporting the original snapshot when
sanitization fails. Cover composite keys and related tables before extending
the design to server dumps.

## v0.4.3 planned scope

- [x] Add `dbsnap tag <snapshot> <new-tag>` and optional `save -m` notes.
  Keep tags unique and preserve compatibility with existing catalogs and
  archives. Limit note length and keep output subject to credential redaction.
- [x] Add `dbsnap rm <snapshot>`. Protect pinned snapshots and reuse the
  catalog lock and pending garbage collection. Preserve SQLite chunks that
  other snapshots still reference.
- [x] Show the latest snapshot in `status` and optionally record its Git
  branch at capture time. Git metadata must be optional and work with Git
  worktrees. Keep the default status cheap.
- [x] Add opt-in `diff-live --ephemeral` with recoverable cleanup on failure
  or interruption. Keep existing `diff-live` checkpoint behavior by default
  and return only references that callers can still use.

- [x] Run live CLI and MCP smoke tests on SQLite and disposable PostgreSQL,
  MySQL, and MariaDB databases.
- [x] Complete hosted CI and package verification for v0.4.3.

## v0.4.4 rehearsal

- [x] Implement retained SQLite/PostgreSQL sandboxes and before/after reports.
- [x] Execute explicit trusted argv with migration timeout and sandbox connection.
- [x] Require MCP session enablement and per-call approval.
- [x] Document command privileges, retained resources and unsupported engines.
- [x] Verify success, policy failure, command failure, timeout and approval denial through CLI/MCP.
- [x] Verify live PostgreSQL and MySQL/MariaDB compatibility.
- [x] Complete hosted CI, six release builds and packed npm verification.

The [pre-release verification](https://github.com/farhan0431/db-snap/actions/runs/42169902179)
passed all 16 jobs at `88e9e15`. Verify installation from the public npm
registry after tag publication.

## Follow-up designs in the v0.4 series

An opt-in status comparison should report what was checked. Schema and row
counts can report differences or no differences detected, but cannot prove
row contents match. SQLite comparison must use a consistent view with WAL
data included. Benchmark the cost before making any latency claims.

`save --amend` requires a defined policy for pinned snapshots and references
to the old ID. Commit the replacement before scheduling old artifact cleanup,
and preserve the old restore point when capture or catalog commit fails.

Extend sandbox removal to SQLite and PostgreSQL after proving ownership of
the current resource. A registered path or database name alone cannot prove
that a resource was not replaced. Keep legacy resources on manual cleanup
when ownership cannot be established.

`dbsnap rehearse` now supports SQLite/PostgreSQL with retained sandboxes
and bounded reports. MySQL/MariaDB Docker sandboxes currently have no host
endpoint and need a separate execution design.
Passing a sandbox URL does not constrain an arbitrary child process. Treat
commands as trusted unless runtime isolation enforces database access. Keep
MCP execution and automatic cleanup as separate design decisions.

`dbsnap deployment-check` would read a rehearsal report and apply explicit
schema-change policies. It should distinguish a policy failure from an
incomplete rehearsal or infrastructure error. Reports should carry IDs,
counts and rule results without raw row values or database credentials.

`dbsnap deployment-preflight` would inspect a development or staging target
for engine/version compatibility, privileges, and pending recovery. The
result should be usable from a terminal, CI, or an agent before a team starts
its rollout. These are planned dbsnap features.
