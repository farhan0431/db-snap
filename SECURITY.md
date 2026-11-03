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
   database; snapshots cannot be restored into a different database without
   an explicit `--force`.
3. **Verify before restore (T3).** SQLite manifests/chunks and dump artifacts
   are checked before restore. PostgreSQL clone fingerprints do not verify
   database contents.
4. **Safety snapshots (T7).** Restore creates a safety snapshot by default,
   unless explicitly disabled on the CLI. A successful snapshot does not make
   PostgreSQL or MySQL restore atomic.
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
- PostgreSQL clone restore is not atomic and may consume the source snapshot.
  Interrupted operations can require manual recovery. PostgreSQL connections
  use `NoTls`; certificate-verified TLS is not implemented.
- MCP restore and prune are disabled unless the server is started with
  `--allow-destructive`. There is no per-operation approval mechanism.
- The masking layer is defense in depth, not a sandbox: an AI agent with
  shell access to your project can read `.env` itself.

## Reporting a vulnerability

Use GitHub's **private security advisory** for this repository
(Security → Report a vulnerability). Do not open a public issue.

Please include: dbsnap version, engine, a minimal reproduction, and the
safety guarantee you believe was broken (T1–T14 above). You can expect an
initial response within 7 days.
