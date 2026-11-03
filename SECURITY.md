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

All of these are specified in [docs/DESIGN.md](docs/DESIGN.md) §4 (threat
model) and enforced by code plus tests:

1. **Local-only by default (T1).** Only localhost/private-network database
   targets are accepted. Remote targets require the human to pass
   `--allow-remote` on the CLI. The MCP server is hard-wired local-only —
   an AI agent has no path to a remote target.
2. **Project binding (T2).** `.dbsnap/config.json` fingerprints the bound
   database; snapshots cannot be restored into a different database without
   an explicit `--force`.
3. **Verify before restore (T3).** Every snapshot is hashed (blake3) on
   write and re-verified per chunk/dump before a single byte touches the
   live database.
4. **Safety snapshots (T7).** Every restore first preserves the current
   state and persists the record before any destructive step.
5. **Surgical pruning (T13).** Deletions only ever touch artifacts recorded
   in metadata; server-side drops only ever touch databases under the
   `__dbsnap_` prefix.
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
- The masking layer is defense in depth, not a sandbox: an AI agent with
  shell access to your project can read `.env` itself.

## Reporting a vulnerability

Use GitHub's **private security advisory** for this repository
(Security → Report a vulnerability). Do not open a public issue.

Please include: dbsnap version, engine, a minimal reproduction, and the
safety guarantee you believe was broken (T1–T14 above). You can expect an
initial response within 7 days.
