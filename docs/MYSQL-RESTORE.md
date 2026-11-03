# MySQL / MariaDB restore

Restore is available for snapshots created by dbsnap on the original target.
It verifies the compressed hash and decompressed size, saves and catalogs a
mandatory pinned safety snapshot, writes `mysql-restore.json`, removes target
objects, and replays the dump. The database itself is never dropped, preserving
its identity and database-level grants. Tables, views, routines, triggers and events are restored; objects
added after the snapshot are removed. Cross-database incoming foreign keys
cause refusal before cleanup.

The recovery artifact is synced before cataloguing and its compressed hash and
pinned status are checked before cleanup. Persistence failures remove a new
safety artifact only when the catalog confirms it was not committed; committed
or uncertain catalog state preserves the artifact. Dump retries retain all
client error messages, including write failures from the initial flag set.

Stop application writers and scheduled events before restore and recovery.
dbsnap does not fence MySQL connections or stop the global event scheduler.
Do not resume application traffic until restoration is complete.
Restore refuses visible connections to the target and enabled target events
when the global event scheduler is ON. This check is repeated after the safety
snapshot. It is not a connection fence: another process can connect afterwards,
and a restricted account may not see other users' connections. Scheduled events
in the snapshot can become active during replay; stop the scheduler first when
restoring snapshots that contain events.

```sh
dbsnap save before-migration
dbsnap restore before-migration
```

`--no-safety-snapshot` is rejected for a fresh MySQL restore. Safety snapshots
remain pinned even on success; inspect them and explicitly unpin unwanted ones
before pruning. No new safety snapshot is taken when recovering an interrupted
restore, because that would capture a partial target.

If restore fails, the target may be partially changed. The error identifies the
recovery ID, also stored as a JSON string in `.dbsnap/mysql-restore.json`:

```sh
dbsnap restore RECOVERY_ID
```

Other restore IDs and unpinning are refused until recovery succeeds. Keep the
journal and pinned snapshot. Fix connection, disk-space or privilege problems
before retrying recovery. `status` reports a pending journal. A malformed journal
requires manual inspection of the pinned snapshots; do not delete it blindly.

Local free-space checks concern `.dbsnap/`, not the database server's disk.
If space runs out while making the safety dump, cleanup has not started and the
target is unchanged. If replay or cleanup fails after the journal is written,
fix the disk/connection problem and restore the journal's recovery ID. Recovery
can itself fail if the server still lacks space. The journal and pinned copy
remain available for another attempt. See [TESTING.md](TESTING.md) for real
dump-client `/dev/full` tests and simulated I/O failure boundaries.

This is an in-place, non-atomic restore, not a staging/cutover implementation.
SQL dumps must be trusted: the database-selection compatibility check is not a
SQL sandbox, and a hash proves integrity against the catalog, not authenticity.
Stored code can reference other schemas or have side effects. Do not restore
untrusted or manually edited dumps/catalogs. Database names are not rewritten,
so legacy `--databases` dumps stay on the original target. SQL syntax-error recovery
is tested, but recovery cannot be guaranteed under arbitrary privilege changes,
concurrent writes or server failures. Power-loss durability and nonstandard
storage engines remain limitations.
MariaDB sequence objects are not supported by the cleanup implementation.
