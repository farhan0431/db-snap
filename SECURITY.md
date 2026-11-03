# Security Policy

dbsnap is a tool that deletes and rewrites databases on purpose — its
security posture is the product. This document explains the guarantees, the
boundaries, and how to report problems.

## Supported versions

| Version | Supported |
| ------- | --------- |
| 0.3.x   | ✅        |
| < 0.3   | ❌        |

## The guarantees (and where they live)

The following controls are described in [docs/DESIGN.md](docs/DESIGN.md) §4.
They do not constitute a security audit or a production backup guarantee:

1. **Host guard (T1).** MCP accepts SQLite and loopback database hosts. The
   CLI also accepts private-network targets; other hosts require `--allow-remote`.
   Loopback does not prove a target is non-production or prevent tunneling.
2. **Project binding (T2).** `.dbsnap/config.json` fingerprints the bound
   database; a catalog from another database is refused even with `--force`.
3. **Verify before restore (T3).** SQLite manifests/chunks and dump artifacts
   are checked before restore. PostgreSQL clone fingerprints do not verify
   database contents.
4. **Safety snapshots (T7).** Restore creates a safety snapshot by default,
   unless explicitly disabled for SQLite/PostgreSQL. Fresh MySQL/MariaDB restores
   require a pinned safety snapshot. A snapshot does not make restore atomic.
5. **Prune boundaries (T13).** Prune uses recorded artifacts and validates
   artifact paths; server-side clone drops require the `__dbsnap_` prefix.
   Filesystem path checks are not an OS-level sandbox against concurrent tampering.
6. **Credential hygiene (T10/T11).** Passwords travel to client binaries via
   environment variables, never argv; they are never written to
   `.dbsnap/`; and every output boundary (CLI, MCP tool results, warnings)
   passes through `util::mask`, which scrubs registered secrets and any
   `scheme://user:password@` userinfo from text — including text produced
   by dependencies.

## What dbsnap does NOT promise

- It is **not** a backup product: snapshots live on the same machine (and,
  for PostgreSQL clone mode, inside the same server) as the database. Use
  real backups for disaster recovery.
- Dump-based snapshots (MySQL, `pg_dump`) are logical restores; a
  `--single-transaction` dump is consistent for InnoDB tables, not for
  MyISAM.
- MySQL/MariaDB restore replaces objects inside the original target database;
  it does not drop the database. It is not atomic and requires quiescent writers
  and scheduled events. A mandatory pinned safety snapshot and journal support
  manual recovery. Dumps are trusted executable SQL, not sandboxed input.
  See docs/MYSQL-RESTORE.md; crash/power-loss durability is not proven.
- MySQL checks visible target connections and enabled events before cleanup,
  but these checks do not fence future connections. Administrators must stop
  writers/events. Local free-space checks do not measure database-server disk.
  Tested ENOSPC and process-exit boundaries are listed in docs/TESTING.md.
- PostgreSQL clone restore is not atomic; the source snapshot is preserved.
  A recovery journal handles some interruptions, but ambiguous states require
  manual recovery. Dump replay occurs in staging before the same cutover.
- PostgreSQL network connections require TLS with certificate and hostname
  verification; loopback uses no TLS unless `DBSNAP_PG_TLS=verify-full` is set.
  `DBSNAP_PG_CA_CERT` can supply an additional trusted PEM CA certificate.
- MCP restore and prune are disabled unless the server is started with
  `--allow-destructive`; each call requires approval through MCP elicitation.
  This depends on the client faithfully presenting the request to the user.
- The masking layer is defense in depth, not a sandbox: an AI agent with
  shell access to your project can read `.env` itself.

## Reporting a vulnerability

Use GitHub's **private security advisory** for this repository
(Security → Report a vulnerability). Do not open a public issue.

Please include: dbsnap version, engine, a minimal reproduction, and the
safety guarantee you believe was broken (T1–T14 above). You can expect an
initial response within 7 days.
