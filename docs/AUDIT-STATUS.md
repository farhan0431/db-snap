# Audit status for the 2026-11-03 report

This tracks the supplied static audit against the current development branch.
It is not a security certification. Restore and fault-injection tests must use
disposable databases; dbsnap is not a production backup system.

| Finding | Status and remaining work |
| --- | --- |
| DB-001 PostgreSQL interrupted restore | Clone and staged-dump cutover use a journal and preserve the old target. Tested process-exit failpoints; ambiguous states require manual intervention. Power-loss durability remains unproven. |
| DB-002 PostgreSQL source clone consumed | Working clone preserves the catalogued snapshot; repeated restore tested. |
| DB-003 Cross-database catalog rebind | Catalog fingerprint checked before operations and rebinding; regression tested. |
| DB-004 Prune removes before metadata | Metadata is committed before artifact deletion; crash/power-loss and pending-GC retry remain to be tested. |
| DB-005 PostgreSQL fence failure | Error cleanup and a recovery journal are implemented; process-exit tests cover a fence interruption. Network loss and power failure remain untested. |
| DB-006 MCP private-network targets | MCP allows SQLite and loopback only; loopback is not proof a target is safe. |
| DB-007 MCP destructive operations | Restore/prune require session enablement and per-call MCP elicitation; unsupported clients fail closed. Tested allow/deny over stdio. This depends on the host faithfully showing the confirmation. |
| DB-008 SQLite manifest binding | Manifest hash sequence and size are checked against the record before staging; reordered-hash regression tested. |
| DB-009 PostgreSQL clone checksum | JSON/list output identifies its hash as a metadata fingerprint, not a content checksum. No cryptographic content verification exists for clones. |
| DB-010 Artifact path traversal | Metadata artifact paths and obvious symlinks are rejected. Concurrent filesystem races and full OS-level containment remain unresolved. |
| DB-011 PostgreSQL TLS | Non-loopback connections require certificate- and hostname-verified TLS. Windows Docker integration checked valid CA and wrong hostname; wider platform testing remains. |
| DB-012 Dump restore failure | PostgreSQL dump replay uses staging. MySQL/MariaDB restores in place without dropping the database, with mandatory synced/pinned safety artifacts, integrity checks and a journal. Visible-client/event guards run before cleanup. Error/process-exit boundaries, partial SQL replay and real InnoDB storage-full recovery are tested on both engines. Replay remains non-atomic and is not an SQL sandbox; stopped writers/events and trusted dumps are required. Power-loss durability remains unproven. |
| DB-013 SQLite creates missing database | Open-existing behavior implemented and tested. |
| DB-014 Durability | Metadata files, new MySQL safety artifacts and recovery journals are synced; Unix directory sync is used. Cross-platform power-loss durability remains unproven. |
| DB-015 Temporary data privacy | New Unix `.dbsnap` directories are private; existing directory permissions, Windows ACLs, and crash cleanup remain unresolved. Snapshot data may contain sensitive information. |
| DB-016 Existing CAS chunk corruption | Existing chunks are verified before reuse; regression tested. A full verify command is not implemented. |
| DB-017 SQLite WAL size | WAL path now appends `-wal`; more extension-specific tests are desirable. |
| DB-018 CI/E2E and fault injection | Live CLI/MCP, PostgreSQL fault tests, expanded MySQL/MariaDB I/O/process-exit tests, real dump ENOSPC and bounded InnoDB storage-full replay/recovery tests are available. CI includes the new CLI/MCP and two-engine bounded-storage jobs. See TESTING.md and QA-REPORT.md. These changes still need a new hosted CI run; arbitrary network/server/power-loss failures are not fully covered. |
| DB-019 Release gate | Release gates on CI and native/packed-package smoke tests. Local release binary and native Windows packed npm smoke are checked. New workflow jobs and cross-platform release artifacts require hosted execution; no new version was published by this QA pass. |
| DB-020 Documentation claims | README, npm README, install/agent guidance, CLI/MCP descriptions, security/design docs and audit notes updated to describe actual per-engine behavior and diff scope. TESTING.md and QA-REPORT.md distinguish test evidence from remaining guarantees. |
| DB-021 PostgreSQL E2E cleanup | Cleanup uses the configured server and credentials, only drops a test database it created, and is tested on success and failure. The script retains the catalog and warns about remaining server-side snapshot clones. |
| DB-022 Orphan dumps | Normal-error staging cleanup is tested; safety artifact sync/catalog failures remove the newly created uncatalogued safety file. Crash termination may leave staging/orphan artifacts, and a normal save that writes an artifact before catalog failure can still leave an orphan. Such files are not silently deleted. |

Do not tag a release as fully audited or production-ready while the unresolved
items above remain. In particular, staging MySQL dumps safely requires handling
database-qualified SQL without redirecting it to the wrong target.

All MySQL/MariaDB dump fallback options now retain routines, triggers, and
events. If the installed client cannot supply these options, snapshot creation
fails rather than silently omitting those objects.

## MySQL restore follow-up

Restore is enabled with mandatory pinned recovery snapshots and a journal.
This is an in-place restore, not the originally proposed staging/cutover design.
The implementation never drops the target database. See
[MySQL restore and recovery](MYSQL-RESTORE.md) for operating requirements.

Current verification uses disposable MySQL 8.0, MariaDB 11 and PostgreSQL 17
servers, the real Windows CLI/MCP process, actual dump clients and controlled
fault injection. See [QA-REPORT.md](QA-REPORT.md) for recorded outcomes and
[TESTING.md](TESTING.md) for reproduction commands. Power-loss, filesystem races,
new connections after preflight, and untrusted SQL remain outside the guarantees.

## Snapshot diff follow-up

Diff supports SQLite schema/counts, PostgreSQL clone table/view/counts, and
standard MySQL/MariaDB table/counts with offline dump inspection. MySQL diff
verifies compressed hashes and sizes and never executes SQL. PostgreSQL dump
diff, per-row content comparison, and MySQL view/routine/trigger/event diff are
not implemented. Unsupported snapshot types and dump INSERT formats fail with
an explicit error rather than silently replaying into the live target.
