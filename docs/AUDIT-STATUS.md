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
| DB-012 Dump restore failure | PostgreSQL replays into staging before cutover. MySQL/MariaDB now restores objects in the original target without dropping the database. Mandatory pinned safety snapshots and a recovery journal support retry after failure. Replay is NOT staged or atomic; trusted dumps and stopped writers/events are required. See MYSQL-RESTORE.md. Power-loss recovery remains unproven. |
| DB-013 SQLite creates missing database | Open-existing behavior implemented and tested. |
| DB-014 Durability | File sync and Unix directory sync added for metadata; cross-platform power-loss durability remains unproven. |
| DB-015 Temporary data privacy | New Unix `.dbsnap` directories are private; existing directory permissions, Windows ACLs, and crash cleanup remain unresolved. Snapshot data may contain sensitive information. |
| DB-016 Existing CAS chunk corruption | Existing chunks are verified before reuse; regression tested. A full verify command is not implemented. |
| DB-017 SQLite WAL size | WAL path now appends `-wal`; more extension-specific tests are desirable. |
| DB-018 CI/E2E and fault injection | PostgreSQL interruption tests exist; CI runs integration tests and now includes a PostgreSQL E2E smoke test and cleanup regression. MySQL cleanup-failure and process-exit recovery tests added; disk-full tests remain outstanding. |
| DB-019 Release gate | Release invokes CI and smoke-tests native binaries and the packed npm package. GitHub execution and cross-target packaging still require verification. |
| DB-020 Documentation claims | Key safety and integrity claims revised. Full documentation consistency review remains. |
| DB-021 PostgreSQL E2E cleanup | Cleanup uses the configured server and credentials, only drops a test database it created, and is tested on success and failure. The script retains the catalog and warns about remaining server-side snapshot clones. |
| DB-022 Orphan dumps | PostgreSQL and MySQL dump staging files and partially written output, and SQLite staged backups, are removed on normal errors. Existing dump destinations are not overwritten. Process termination and orphan cleanup after a successful artifact write but failed catalog update remain unresolved. |

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

Local verification used disposable MySQL 8.0 and MariaDB 11 containers, with
Windows test binaries invoking the container client tools through temporary
wrappers. Covered roundtrips, repeated restore, routines/views/triggers/events,
removal of newly added objects, corrupt compressed input, SQL replay failure,
error/process-exit recovery after cleanup, refusal of incoming cross-database
foreign keys, and preservation of a sentinel in another test database.
Release-binary E2E demos passed
on both engines, restoring the dropped column and all five rows. The full local test suite passed
with MySQL configured (21 unit tests, 39 integration test functions); PostgreSQL
live tests and the credential test without a test password were skipped by their
existing environment guards. Formatting, Clippy and the release build passed.
This does not close the remaining durability, concurrent-writer, SQL isolation,
disk-full or release-platform audit items.
