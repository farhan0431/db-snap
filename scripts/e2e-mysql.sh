#!/usr/bin/env bash
# End-to-end demo of dbsnap's MySQL/MariaDB engine:
#
#   a consistent --single-transaction dump, a destructive migration that
#   drops a column with all its data, and a verified rollback (drop +
#   recreate with original charset + replay).
#
# Prereq: a local MySQL/MariaDB + client tools in PATH. Defaults to
# mysql://root@localhost:3306 — point DBSNAP_TEST_MYSQL_URL elsewhere
# (credentials in the URL are honored, password via MYSQL_PWD).
#
# Usage: scripts/e2e-mysql.sh

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${DBSNAP_BIN:-$ROOT/target/release/dbsnap}"
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) BIN="$BIN.exe" ;; esac
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL="*"

BASE_URL="${DBSNAP_TEST_MYSQL_URL:-mysql://root@127.0.0.1:3306/mysql}"
MYSQL="${MYSQL:-mysql}"

if [ ! -x "$BIN" ]; then
  echo "dbsnap binary not found at $BIN — run: cargo build --release" >&2
  exit 1
fi

# parse credentials from the base URL (user[:password]@host[:port])
if [[ "$BASE_URL" =~ ^[a-z]+://([^:@/]*)(:([^@/]*))?@([^:/]*)(:([0-9]+))?/ ]]; then
  MYUSER="${BASH_REMATCH[1]:-root}"
  MYPASS="${BASH_REMATCH[3]:-}"
  MYHOST="${BASH_REMATCH[4]:-127.0.0.1}"
  MYPORT="${BASH_REMATCH[6]:-3306}"
else
  MYUSER="root"; MYPASS=""; MYHOST="127.0.0.1"; MYPORT="3306"
fi
export MYSQL_PWD="$MYPASS"
MYSQL_ARGS=(-h "$MYHOST" -P "$MYPORT" -u "$MYUSER")

WORK="$(mktemp -d)"
DBNAME="dbsnap_e2e_$$"
cleanup() {
  "$MYSQL" "${MYSQL_ARGS[@]}" -e "DROP DATABASE IF EXISTS $DBNAME;" >/dev/null 2>&1 || true
  rm -rf "$WORK" 2>/dev/null || true
}
trap cleanup EXIT
cd "$WORK"

URL="${BASE_URL%/*}/$DBNAME"

echo "== creating test database $DBNAME =="
"$MYSQL" "${MYSQL_ARGS[@]}" -e "CREATE DATABASE $DBNAME CHARACTER SET utf8mb4;" >/dev/null
"$MYSQL" "${MYSQL_ARGS[@]}" "$DBNAME" -e "
  CREATE TABLE users (id INT PRIMARY KEY AUTO_INCREMENT, email TEXT);
  INSERT INTO users (email) VALUES ('u1@t.dev'),('u2@t.dev'),('u3@t.dev'),('u4@t.dev'),('u5@t.dev');"

echo "== agent snapshots (consistent logical dump) =="
"$BIN" save before-migration --db "$URL"

echo "== destructive migration: ALTER TABLE users DROP COLUMN email =="
"$MYSQL" "${MYSQL_ARGS[@]}" "$DBNAME" -e "ALTER TABLE users DROP COLUMN email;"

echo "== rollback (verify hash -> charset capture -> safety dump -> drop -> replay) =="
"$BIN" rollback --db "$URL"

echo "== verify: column + data must be back =="
RESULT="$("$MYSQL" "${MYSQL_ARGS[@]}" -N -B "$DBNAME" -e "SELECT CONCAT(COUNT(*), '|', MIN(email)) FROM users;")"
echo "mysql: $RESULT"
[ "$RESULT" = "5|u1@t.dev" ] || { echo "FAIL: expected 5|u1@t.dev"; exit 1; }

echo "== DONE — MySQL/MariaDB snapshot/rollback verified =="
