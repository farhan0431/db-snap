#!/usr/bin/env bash
# Regression test for custom PostgreSQL connection and cleanup, without a server.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cat > "$WORK/psql" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
python - <<'PY'
import configparser, os
config = configparser.ConfigParser()
config.read(os.environ["PGSERVICEFILE"])
section = config[os.environ["PGSERVICE"]]
assert section["host"] == "127.0.0.1"
assert section["port"] == "45436"
assert section["user"] == "tester"
assert section["password"] == "test password"
assert section["connect_timeout"] == "5"
if os.environ["PGSERVICE"] == "dbsnap_base":
    assert section["dbname"] == "admin"
else:
    assert section["dbname"].startswith("__dbsnap_e2e_")
PY
printf '%s %s\n' "$PGSERVICE" "$*" >> "$TEST_LOG"
case "$*" in *"SELECT COUNT"*) echo '50|1';; esac
SH
cat > "$WORK/dbsnap" <<'SH'
#!/usr/bin/env bash
if [ "${TEST_FAIL_SAVE:-0}" = 1 ] && [ "$1" = save ]; then exit 1; fi
exit 0
SH
chmod +x "$WORK/psql" "$WORK/dbsnap"
cp "$WORK/dbsnap" "$WORK/dbsnap.exe"
export TEST_LOG="$WORK/log"
export DBSNAP_TEST_PG_URL='postgres://tester:test%20password@127.0.0.1:45436/admin?connect_timeout=5'
export DBSNAP_BIN="$WORK/dbsnap" PSQL="$WORK/psql"
bash "$ROOT/scripts/e2e-pg.sh" >/dev/null
grep -q 'dbsnap_base .*CREATE DATABASE' "$TEST_LOG"
grep -q 'dbsnap_base .*DROP DATABASE' "$TEST_LOG"
if grep -q 'test%20password\|test password' "$TEST_LOG"; then
  echo 'Credentials appeared in psql arguments' >&2; exit 1
fi
: > "$TEST_LOG"
if TEST_FAIL_SAVE=1 bash "$ROOT/scripts/e2e-pg.sh" >/dev/null 2>&1; then
  echo 'Expected failed snapshot to abort the demo' >&2; exit 1
fi
grep -q 'dbsnap_base .*DROP DATABASE' "$TEST_LOG"
echo 'PostgreSQL E2E connection and cleanup regression checks passed.'
