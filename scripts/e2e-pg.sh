#!/usr/bin/env bash
# End-to-end demo of dbsnap's PostgreSQL engine:
#
#   an AI agent snapshots a local PostgreSQL database (server-side template
#   clone), a destructive migration drops a column with all its data, and
#   dbsnap rolls back through a staged clone — checked with psql.
#
# Prereq: a local PostgreSQL. Defaults to postgres://postgres@localhost:5433
# (start your own, e.g. via pg_ctl, or point DBSNAP_TEST_PG_URL elsewhere).
# Requires Python 3 to prepare a temporary libpq service file.
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
DBNAME="__dbsnap_e2e_$$_${RANDOM}"
CREATED=0
SERVICE_FILE="$WORK/pg_service.conf"
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) SERVICE_FILE="$(cygpath -w "$SERVICE_FILE")" ;; esac
unset PGDATABASE
cleanup() {
  if [ "$CREATED" = 1 ]; then
    if ! PGSERVICEFILE="$SERVICE_FILE" PGSERVICE=dbsnap_base "$PSQL" -w -X -v ON_ERROR_STOP=1 \
        -c "DROP DATABASE IF EXISTS \"$DBNAME\";" >/dev/null; then
      echo "WARNING: could not remove test database $DBNAME on the configured server" >&2
    fi
  fi
  rm -f "$SERVICE_FILE" 2>/dev/null || true
  if [ -f "$WORK/.dbsnap/metadata.json" ]; then
    echo "Snapshot catalog retained at $WORK/.dbsnap; server-side clones may remain." >&2
  else
    rm -rf "$WORK" 2>/dev/null || true
  fi
}
trap cleanup EXIT
cd "$WORK"

# Replace only the database path; preserve credentials, host, port, and query.
case "$BASE_URL" in
  postgres://*/*|postgresql://*/*) ;;
  *) echo "DBSNAP_TEST_PG_URL must be a PostgreSQL URL with a database path" >&2; exit 1 ;;
esac
BASE_PATH="${BASE_URL%%\?*}"
QUERY=""
if [[ "$BASE_URL" == *\?* ]]; then QUERY="?${BASE_URL#*\?}"; fi
URL="${BASE_PATH%/*}/$DBNAME$QUERY"

# Keep credentials out of psql's arguments. libpq reads this private service
# file; reject connection options that could redirect cleanup to another DB.
umask 077
DBSNAP_SERVICE_FILE="$SERVICE_FILE" DBSNAP_BASE_URL="$BASE_URL" DBSNAP_E2E_DB="$DBNAME" python - <<'PY'
import os
import re
from urllib.parse import parse_qsl, unquote, urlsplit

url = urlsplit(os.environ["DBSNAP_BASE_URL"])
if not url.hostname or not url.path.strip("/"):
    raise SystemExit("PostgreSQL URL must contain a host and database")
values = {"host": url.hostname, "dbname": unquote(url.path[1:])}
if url.port is not None:
    values["port"] = str(url.port)
if url.username is not None:
    values["user"] = unquote(url.username)
if url.password is not None:
    values["password"] = unquote(url.password)
for key, value in parse_qsl(url.query, keep_blank_values=True):
    if not re.fullmatch(r"[a-z_]+", key) or key in values or key in {"host", "hostaddr", "port", "user", "password", "dbname", "service", "servicefile"}:
        raise SystemExit("unsupported or duplicate PostgreSQL URL option")
    values[key] = value
if any("\n" in value or "\r" in value for value in values.values()):
    raise SystemExit("PostgreSQL connection values cannot contain newlines")
with open(os.environ["DBSNAP_SERVICE_FILE"], "w", encoding="utf-8") as output:
    for name, database in (("dbsnap_base", values["dbname"]), ("dbsnap_test", os.environ["DBSNAP_E2E_DB"])):
        output.write(f"[{name}]\n")
        for key, value in values.items():
            output.write(f"{key}={database if key == 'dbname' else value}\n")
PY

echo "== creating test database $DBNAME =="
PGSERVICEFILE="$SERVICE_FILE" PGSERVICE=dbsnap_base "$PSQL" -w -X -v ON_ERROR_STOP=1 -c "CREATE DATABASE \"$DBNAME\";" >/dev/null
CREATED=1
PGSERVICEFILE="$SERVICE_FILE" PGSERVICE=dbsnap_test "$PSQL" -w -X -v ON_ERROR_STOP=1 -q -c "
  CREATE TABLE users (id serial PRIMARY KEY, email TEXT);
  INSERT INTO users (email) SELECT 'u' || g || '@test.dev' FROM generate_series(1, 50) g;"

echo "== agent snapshots (server-side clone; ~1 s connection blackout) =="
DATABASE_URL="$URL" "$BIN" save before-migration

echo "== destructive migration: ALTER TABLE users DROP COLUMN email =="
PGSERVICEFILE="$SERVICE_FILE" PGSERVICE=dbsnap_test "$PSQL" -w -X -v ON_ERROR_STOP=1 -q -c "ALTER TABLE users DROP COLUMN email;"

echo "== rollback (working clone -> fence -> safety -> journaled rename -> unfence) =="
DATABASE_URL="$URL" "$BIN" rollback

echo "== verify: column + expected test data must be back =="
RESULT="$(PGSERVICEFILE="$SERVICE_FILE" PGSERVICE=dbsnap_test "$PSQL" -w -X -v ON_ERROR_STOP=1 -t -A -c "SELECT COUNT(*) || '|' || COUNT(*) FILTER (WHERE email = 'u1@test.dev') FROM users;")"
echo "psql: $RESULT"
[ "$RESULT" = "50|1" ] || { echo "FAIL: expected 50 rows including u1@test.dev"; exit 1; }

echo "== portable dump method =="
DATABASE_URL="$URL" "$BIN" save dump-check --method dump
DATABASE_URL="$URL" "$BIN" list

echo "== DONE — PostgreSQL hot clone/rollback verified =="
