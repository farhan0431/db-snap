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

## v0.4.2 design proposals

`dbsnap rehearse` would take a snapshot selector and an explicit migration
command. It would create a sandbox, pass the sandbox connection to that
command, and collect a bounded report with the migration exit code and
before/after snapshot IDs. The first version should support engines where
dbsnap can pass a usable sandbox connection. MySQL/MariaDB Docker sandboxes
currently have no host endpoint and need a separate execution design.

`dbsnap deployment-check` would read a rehearsal report and apply explicit
schema-change policies. It should distinguish a policy failure from an
incomplete rehearsal or infrastructure error. Reports should carry IDs,
counts and rule results without raw row values or database credentials.

`dbsnap deployment-preflight` would inspect a development or staging target
for engine/version compatibility, privileges, and pending recovery. The
result should be usable from a terminal, CI, or an agent before a team starts
its rollout. These are planned dbsnap features.
