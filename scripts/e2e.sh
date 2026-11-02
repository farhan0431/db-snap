#!/usr/bin/env bash
# End-to-end demo of dbsnap's core promise:
#
#   a "dev server" keeps the SQLite database open (WAL mode) and writes
#   continuously; an AI agent's destructive "migration" drops a column with
#   all its data; dbsnap rolls the database back — while the server is still
#   running — and the server keeps working against the restored state.
#
# Usage: scripts/e2e.sh            (uses target/release/dbsnap[.exe])
#        DBSNAP_BIN=path scripts/e2e.sh

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${DBSNAP_BIN:-$ROOT/target/release/dbsnap}"
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) BIN="$BIN.exe" ;; esac

if [ ! -x "$BIN" ]; then
  echo "dbsnap binary not found at $BIN — run: cargo build --release" >&2
  exit 1
fi

WORK="$(mktemp -d)"
cleanup() {
  kill "$SERVER_PID" 2>/dev/null || true
  # server may still hold the files for a moment — give it a beat, then
  # best-effort remove (temp dir contents are harmless if they linger)
  sleep 1
  rm -rf "$WORK" 2>/dev/null || true
}
trap cleanup EXIT
cd "$WORK"

cat > server.js <<'EOF'
// Simulates a dev server: holds the DB open, writes every 100 ms.
const { DatabaseSync } = require('node:sqlite');
const db = new DatabaseSync(process.argv[2]);
db.exec('PRAGMA journal_mode = WAL');
db.exec('PRAGMA busy_timeout = 5000');
db.exec('CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT)');
let i = 0;
const timer = setInterval(() => {
  try {
    db.prepare('INSERT OR REPLACE INTO kv (k, v) VALUES (?1, ?2)').run('w' + i, 'value-' + i);
    i++;
  } catch { /* table temporarily gone during the destructive phase */ }
}, 100);
setTimeout(() => { clearInterval(timer); db.close(); }, Number(process.argv[3]) * 1000);
EOF

cat > migration.js <<'EOF'
// Simulates an AI agent's destructive "migration": drops the v column
// together with all its data — unfixable with ordinary queries.
const { DatabaseSync } = require('node:sqlite');
const db = new DatabaseSync(process.argv[2]);
db.exec('PRAGMA busy_timeout = 5000');
db.exec('DROP TABLE kv');
db.exec('CREATE TABLE kv (k TEXT PRIMARY KEY)');
db.prepare('INSERT INTO kv (k) VALUES (?)').run('migrated');
db.close();
EOF

cat > assert.js <<'EOF'
// PASS only if the v column and its data came back.
const { DatabaseSync } = require('node:sqlite');
const db = new DatabaseSync(process.argv[2], { readOnly: true });
const rows = db.prepare('SELECT COUNT(*) c FROM kv').get().c;
const sample = db.prepare('SELECT v FROM kv WHERE v IS NOT NULL LIMIT 1').get();
if (rows <= 0) { console.log('FAIL: kv has no rows'); process.exit(1); }
if (!sample) { console.log('FAIL: column v is missing — restore was not physical'); process.exit(1); }
console.log(`PASS: kv restored — ${rows} rows, sample v = ${sample.v}`);
EOF

echo "== starting dev server (holds DB open, WAL mode, writes every 100 ms) =="
node server.js app.db 6 &
SERVER_PID=$!
sleep 1

echo "== agent snapshots before the risky migration =="
"$BIN" save before-migration --db app.db

echo "== destructive migration runs (DROP TABLE + data loss) =="
node migration.js app.db
node -e "
const { DatabaseSync } = require('node:sqlite');
const db = new DatabaseSync('app.db', { readOnly: true });
console.log('after migration:', db.prepare('SELECT COUNT(*) c FROM kv').get().c, 'row, no v column');
"

echo "== rollback while the dev server is STILL RUNNING =="
"$BIN" rollback --db app.db

sleep 1
node assert.js app.db

echo "== server kept writing through the whole episode; newest state survives =="
node -e "
const { DatabaseSync } = require('node:sqlite');
const db = new DatabaseSync('app.db', { readOnly: true });
const rows = db.prepare('SELECT COUNT(*) c FROM kv').get().c;
console.log('rows now (server kept writing after restore):', rows);
if (rows < 2) { console.log('FAIL: server did not continue after restore'); process.exit(1); }
"
echo "== DONE — hot snapshot/restore verified under a live writer =="
wait "$SERVER_PID" 2>/dev/null || true
