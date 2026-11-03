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
| DB-012 Dump restore failure | PostgreSQL replays into staging before cutover. **MySQL/MariaDB restore is temporarily disabled** because the prior implementation could leave a partial target and dropped an unmanaged database. Safe restore remains unimplemented. |
| DB-013 SQLite creates missing database | Open-existing behavior implemented and tested. |
| DB-014 Durability | File sync and Unix directory sync added for metadata; cross-platform power-loss durability remains unproven. |
| DB-015 Temporary data privacy | New Unix `.dbsnap` directories are private; existing directory permissions, Windows ACLs, and crash cleanup remain unresolved. Snapshot data may contain sensitive information. |
| DB-016 Existing CAS chunk corruption | Existing chunks are verified before reuse; regression tested. A full verify command is not implemented. |
| DB-017 SQLite WAL size | WAL path now appends `-wal`; more extension-specific tests are desirable. |
| DB-018 CI/E2E and fault injection | PostgreSQL interruption tests exist; CI runs integration tests. MySQL fault injection, disk-full tests, and E2E scripts remain outstanding. |
| DB-019 Release gate | Release invokes CI and smoke-tests native binaries and the packed npm package. GitHub execution and cross-target packaging still require verification. |
| DB-020 Documentation claims | Key safety and integrity claims revised. Full documentation consistency review remains. |
| DB-021 PostgreSQL E2E cleanup | Not yet addressed. |
| DB-022 Orphan dumps | PostgreSQL dump writes to staging and cleans normal failure; crashes and other partial artifacts still need safe cleanup. |

Do not tag a release as fully audited or production-ready while the unresolved
items above remain. In particular, staging MySQL dumps safely requires handling
database-qualified SQL without redirecting it to the wrong target.
