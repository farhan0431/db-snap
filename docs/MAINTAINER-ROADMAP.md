# Maintainer roadmap

Use this checklist to track implementation and release verification. The README and development workflow describe available features and their limits.

## v0.4.1 release checklist

- [x] Portable archive export/import, server row previews, and Git-style CLI diff
- [x] Sandboxes from trusted PostgreSQL custom dumps
- [x] Test partial archive rejection and PostgreSQL interruption after replay
- [ ] Complete sandbox lifecycle management. The inventory and `doctor`
  report the source snapshot and whether each registered resource is present.
  Docker sandbox removal checks the exact registered ID and project label.
  SQLite and PostgreSQL still require manual cleanup.
- [ ] Report migration checks in CI. Produce bounded machine-readable results
  and allow explicit failure rules for changes such as dropped tables.
- [ ] Add primary-key range and column filters to row previews. Preserve the
  row and byte limits and keep values private by default.
- [ ] Explore opt-in sanitized fixture exports and expand recovery tests for
  disk errors, network loss, and process interruptions.
- [ ] Repeat hosted CI, six release builds, and npm package tests on the
  completed v0.4.1 branch.

The [previous pre-release run](https://github.com/farhan0431/db-snap/actions/runs/41775572622)
validated the original portability and diff work. The expanded checklist
requires another run.

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
