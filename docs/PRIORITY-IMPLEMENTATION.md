# Priority implementation follow-up — 2026-11-04

Published npm v0.3.14 and its successful GitHub publish step were confirmed
before starting this source-tree implementation. The commands below are not
part of that already published release.

The source version is now 0.4.0 across Cargo and all npm package manifests.
Publication remains pending the new hosted CI and a subsequent release tag.

| Priority | Implementation | Deliberate boundaries |
| --- | --- | --- |
| Publication | npm registry confirms v0.3.14 | New source work still requires a new release |
| Verify | CLI verify and MCP dbsnap_verify for active snapshots | Clones: existence only; no replay or restore-success assertion |
| Doctor/recover | JSON connection/journal/untracked/pending-GC diagnosis; CLI journal recovery | No server restart/repair, no guessed IDs; ambiguous PG states remain manual |
| GC/orphans | Persisted pending_gc for retired records; prune retries; doctor reports untracked files | No blind orphan deletion; CAS orphan/server inventory is not comprehensive |
| Checkpoint execution | CLI exec saves first and returns exact ID, child status/output/recovery command | No automatic restore, transaction, environment retargeting or shell invocation; command not exposed via MCP |
| Agent responses | outputSchema/annotations plus structuredContent on all tool/approval paths; save returns snapshot_id | Error codes are broad categories, not a complete engine-specific taxonomy |
| Change preview | Retained live comparison; bounded SQLite primary-key row counts; offline PostgreSQL archive schema/COPY-count diff | Server-engine row-content diff and mixed clone/archive comparison remain unsupported |
| Sandbox | New SQLite file, separately registered PostgreSQL clone, or explicit network-isolated MySQL/MariaDB Docker server | PG dump sandbox/same-server MySQL rewriting unsupported; Docker option requires explicit image; successful resources need manual cleanup |

The command contract, examples and privacy/retention behavior are in
[DEVELOPMENT-WORKFLOW.md](DEVELOPMENT-WORKFLOW.md). README, npm README and the
installed agent skill describe source-vs-published availability explicitly.

Audit checks added include corrupt snapshot verification without target changes,
checkpoint preservation on command failure, independent SQLite sandbox data,
refusal to overwrite a sandbox output, primary-key update/add/delete counts
without values, preview limits/identifier quoting, and prune interruption after
metadata commit followed by retry without touching an unrecorded sentinel file.

MCP grows from six to ten advertised tools. Exec, sandbox and journal recover
are CLI operations; existing approved MCP restore remains the route for a
MySQL recovery ID. Destructive MCP session/per-call confirmation checks remain
in place. Diagnostic annotations do not replace authorization.

Successful save responses carry the exact snapshot ID in structured data.
When an operation fails with an existing valid MySQL journal, structured data
also contains `recovery_id`, recovery stage and the next action; malformed
journals receive a manual-inspection hint. Diagnostics never run repair silently.

This implementation does not close power-loss guarantees, filesystem race/ACL
containment, MySQL writer fencing, untrusted SQL isolation or clone content
checksums. Do not downgrade catalogs with pending_gc to a binary unaware of
that field. Sandbox registry writes can leave a managed server orphan on
failure; retain the reported name and clean it up explicitly after inspection.

## Recorded verification

- npm public v0.3.14 was installed into an isolated prefix and exercised with
  init/save/list/status/restore on temporary SQLite data; version output matched.
- Existing live regression run: 25 unit tests and 47 integration functions with
  PostgreSQL, TLS and MySQL configured passed. Five MySQL integration functions
  also passed on MariaDB, including structured recovery hints.
- Added PostgreSQL archive diff passed against PostgreSQL 17 with matching
  native tools, changed/identical archives, COPY data and corrupt artifacts.
- Windows file-lock GC retry passed after a real deletion refusal; releasing
  the chunk handle allowed retry to remove only the recorded artifacts.
- Final non-server suite passed 26 unit tests and 50 Windows integration
  functions. Optional server cases return early there; the preceding live and
  targeted results provide the server evidence, not those skipped invocations.
- CLI/MCP process scripts cover the source commands and ten advertised tools,
  structured results, approval/denial, checkpoint execution, independent
  sandboxes, live comparison, row preview, diagnosis and journal recovery.
- MySQL 8.0 and MariaDB 11 CLI/MCP scripts passed with `--docker-sandbox`,
  including isolated-network/port checks, replay, independent row mutation and
  explicit cleanup. Failed sandbox replay cleanup passed separately on both
  MySQL 8.0 and MariaDB 11, preserving the source and leaving no new container.
- The first non-server suite attempt hit Windows sandbox temporary-file rename
  denial; the same suite passed with normal filesystem access. A concurrent
  sandbox inventory assertion was rerun separately and passed for MySQL.

Formatting, Clippy, skill validation, Python/YAML syntax, npm launcher checks,
and the final Windows release build passed. The full default suite and SQLite
CLI/MCP process script passed again after moving exec spool files outside the
catalog. These local results do not substitute
for a new hosted multi-platform CI/release run of the uncommitted source tree.

Raw checkpoint command output is spooled in guarded OS temporary files outside
the catalog and masked before returning it; normal completion removes the files.

The first hosted v0.4.0 CI run exposed a PostgreSQL process-test assumption:
its dummy password `dbsnap` is also part of generated sandbox identifiers, so
credential masking changes the displayed name. Cleanup now reads the exact
owned name from the local sandbox registry and checks isolated row mutation
before dropping that database. The complete PostgreSQL CLI/MCP script passed
locally with that same dummy password after this change.
