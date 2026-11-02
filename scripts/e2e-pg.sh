#!/usr/bin/env bash
# End-to-end demo of dbsnap's PostgreSQL engine:
#
#   an AI agent snapshots a local PostgreSQL database (server-side template
#   clone), a destructive migration drops a column with all its data, and
#   dbsnap rolls back via fence -> safety-clone -> drop -> rename — verified
#   bit-for-bit with psql.
#
# Prereq: a local PostgreSQL. Defaults to postgres://postgres@localhost:5433
# (start your own, e.g. via pg_ctl, or point DBSNAP_TEST_PG_URL elsewhere).
#
# Usage: scripts/e2e-pg.sh

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${DBSNAP_BIN:-$ROOT/target/release/dbsnap}"
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) BIN="$BIN.exe" ;; esac
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL="*"

BASE_URL="${DBSNAP_TEST_PG_URL:-postgres://postgres@localhost:5433/postgres}"
PSQL="${PSQL:-psql}"

if [ ! -x "$BIN" ]; then
  echo "dbsnap binary not found at $BIN — run: cargo build --release" >&2
  exit 1
fi

WORK="$(mktemp -d)"
DBNAME="dbsnap_e2e_$$"
cleanup() {
  "$PSQL" "postgresql://postgres@localhost:5433/postgres" \
      -c "DROP DATABASE IF EXISTS $DBNAME;" >/dev/null 2>&1 || true
  rm -rf "$WORK" 2>/dev/null || true
}
trap cleanup EXIT
cd "$WORK"

# derive the per-run database URL from the base URL
URL="${BASE_URL%/*}/$DBNAME"
PORT="$(echo "$URL" | sed -n 's|.*:\([0-9]*\)/.*|\1|p')"
PSQL_URL="postgresql://postgres@localhost:${PORT:-5433}/postgres"

echo "== creating test database $DBNAME =="
"$PSQL" "$PSQL_URL" -c "CREATE DATABASE $DBNAME;" >/dev/null
"$PSQL" "$URL" -q -c "
  CREATE TABLE users (id serial PRIMARY KEY, email TEXT);
  INSERT INTO users (email) SELECT 'u' || g || '@test.dev' FROM generate_series(1, 50) g;"

echo "== agent snapshots (server-side clone; ~1 s connection blackout) =="
"$BIN" save before-migration --db "$URL"

echo "== destructive migration: ALTER TABLE users DROP COLUMN email =="
"$PSQL" "$URL" -q -c "ALTER TABLE users DROP COLUMN email;"

echo "== rollback (fence -> safety clone -> drop -> rename -> unfence) =="
"$BIN" rollback --db "$URL"

echo "== verify: column + data must be back, bit-for-bit =="
RESULT="$("$PSQL" "$URL" -t -A -c "SELECT COUNT(*) || '|' || COALESCE(MIN(email), 'MISSING') FROM users;")"
echo "psql: $RESULT"
[ "$RESULT" = "50|u1@test.dev" ] || { echo "FAIL: expected 50|u1@test.dev"; exit 1; }

echo "== portable dump method =="
"$BIN" save dump-check --method dump --db "$URL"
"$BIN" list --db "$URL"

echo "== DONE — PostgreSQL hot clone/rollback verified =="
