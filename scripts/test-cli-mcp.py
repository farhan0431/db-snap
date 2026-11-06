#!/usr/bin/env python3
"""Exercise the actual CLI and MCP stdio protocol on disposable databases.

python scripts/test-cli-mcp.py --engine sqlite --binary target/debug/dbsnap
Set DBSNAP_TEST_PG_URL or DBSNAP_TEST_MYSQL_URL for the other engines.
Only a uniquely named database created by this script is dropped.
"""
import argparse
from contextlib import closing
import json
import os
from pathlib import Path
import queue
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from urllib.parse import urlsplit, urlunsplit, unquote


def checked(args, **kwargs):
    result = subprocess.run(args, capture_output=True, text=True, timeout=120, **kwargs)
    if result.returncode:
        raise AssertionError(f"command failed ({Path(str(args[0])).name}): {result.stderr}")
    return result.stdout


def find_dump_client(scheme):
    primary, fallback = ("mariadb-dump", "mysqldump") if scheme == "mariadb" else ("mysqldump", "mariadb-dump")
    return shutil.which(primary) or shutil.which(fallback)


class Mcp:
    def __init__(self, binary, root, url, allow):
        args = [str(binary), "mcp", "--db", url]
        if allow:
            args.append("--allow-destructive")
        self.proc = subprocess.Popen(args, cwd=root, stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.messages = queue.Queue()
        self.serial = 0
        def read():
            for line in self.proc.stdout:
                self.messages.put(json.loads(line))
            self.messages.put(None)
        threading.Thread(target=read, daemon=True).start()
        self.call("initialize", {"protocolVersion": "2025-06-18", "capabilities": {"elicitation": {}},
                                 "clientInfo": {"name": "dbsnap-qa", "version": "1"}})
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def send(self, value):
        self.proc.stdin.write(json.dumps(value) + "\n")
        self.proc.stdin.flush()

    def call(self, method, params, approve=True):
        self.serial += 1
        serial = self.serial
        self.send({"jsonrpc": "2.0", "id": serial, "method": method, "params": params})
        while True:
            message = self.messages.get(timeout=120)
            assert message is not None, "MCP exited before responding"
            if message.get("method") == "elicitation/create":
                self.send({"jsonrpc": "2.0", "id": message["id"], "result": {
                    "action": "accept" if approve else "decline", "content": {"confirm": approve}}})
                continue
            assert message.get("id") == serial, message
            return message

    def tool(self, name, arguments=None, error=False, approve=True):
        reply = self.call("tools/call", {"name": name, "arguments": arguments or {}}, approve)
        assert "error" not in reply, reply
        assert bool(reply["result"].get("isError")) == error, reply
        structured=reply["result"].get("structuredContent")
        assert isinstance(structured, dict) and structured["ok"] == (not error), reply
        assert isinstance(structured["message"], str), reply
        if name == "dbsnap_create_snapshot" and not error:
            assert structured["data"]["snapshot_id"], reply
        return "\n".join(item.get("text", "") for item in reply["result"].get("content", []))

    def close(self):
        self.proc.stdin.close()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
            raise
        assert self.proc.returncode == 0, self.proc.stderr.read()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--engine", choices=["sqlite", "postgres", "mysql"], required=True)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--disk-full", action="store_true",
                        help="test a real dump-client ENOSPC using /dev/full (Unix MySQL/MariaDB)")
    parser.add_argument("--docker-sandbox", action="store_true", help="test explicit MySQL/MariaDB sandbox on a separate network-isolated Docker server")
    options = parser.parse_args()
    binary = Path(options.binary).resolve()
    created = False
    with tempfile.TemporaryDirectory(prefix="dbsnap-cli-mcp-") as directory:
        root = Path(directory)
        if options.engine == "sqlite":
            url = str(root / "dev.db")
            def sql(query, admin=False):
                with closing(sqlite3.connect(url)) as connection:
                    if query.lstrip().upper().startswith("SELECT"):
                        return str(connection.execute(query).fetchone()[0])
                    connection.executescript(query)
                    return ""
        else:
            key = "DBSNAP_TEST_PG_URL" if options.engine == "postgres" else "DBSNAP_TEST_MYSQL_URL"
            parsed = urlsplit(os.environ[key])
            assert parsed.hostname in ("localhost", "127.0.0.1", "::1"), "QA requires loopback"
            name = "dbsnap_qa_" + uuid.uuid4().hex[:16]
            quote = '"' if options.engine == "postgres" else '`'
            quoted = quote + name + quote
            url = urlunsplit(parsed._replace(path="/" + name))
            env = os.environ.copy()
            for key in ("PGHOSTADDR", "PGSERVICE", "PGSERVICEFILE"):
                env.pop(key, None)
            env["PGPASSWORD" if options.engine == "postgres" else "MYSQL_PWD"] = unquote(parsed.password or "")
            def sql(query, admin=False, database_override=None):
                database = database_override or (unquote(parsed.path[1:]) if admin else name)
                if options.engine == "postgres":
                    args = ["psql", "-X", "-w", "-h", parsed.hostname, "-p", str(parsed.port or 5432),
                            "-U", unquote(parsed.username or "postgres"), "-d", database,
                            "-A", "-t", "-v", "ON_ERROR_STOP=1", "-c", query]
                else:
                    args = ["mysql", "--no-defaults", "-h", parsed.hostname, "-P", str(parsed.port or 3306),
                            "-u", unquote(parsed.username or "root"), "-N", "-B", database, "-e", query]
                return checked(args, env=env).strip()
            sql(f"CREATE DATABASE {quoted}", admin=True)
            created = True

        def cli(*args, error=False):
            invocation = [str(binary), "exec", "--db", url, *args[1:]] if args and args[0] == "exec" else [str(binary), *args, "--db", url]
            result = subprocess.run(invocation, cwd=root,
                                    capture_output=True, text=True, timeout=120)
            assert (result.returncode != 0) == error, result.stderr
            return result.stdout if not error else result.stderr

        def rows():
            return int(sql("SELECT COUNT(*) FROM items"))

        def sandbox_state(snapshot_id, state, operation="sandbox-list"):
            report = json.loads(cli(operation))
            assert any(s["snapshot_id"] == snapshot_id and s["state"] == state
                       for s in report["sandboxes"]), report

        def interrupted(failpoint, *args):
            env = dict(os.environ, DBSNAP_FAILPOINT=failpoint)
            result = subprocess.run([str(binary), *args, "--db", url], cwd=root,
                                    env=env, capture_output=True, text=True, timeout=120)
            assert result.returncode == 86, result.stderr
            return result.stderr.strip()

        try:
            checked([str(binary), "--version"])
            checked([str(binary), "--help"])
            for command in ("init", "save", "restore", "rollback", "list", "status", "diff", "diff-live", "diff-rows", "pin", "unpin", "prune", "mcp", "verify", "doctor", "recover", "exec", "sandbox"):
                checked([str(binary), command, "--help"])
            sql("CREATE TABLE items(id INT PRIMARY KEY, value TEXT); INSERT INTO items VALUES(1,'first');")
            cli("init")
            cli("save", "before")
            sql("INSERT INTO items VALUES(2,'second'); CREATE TABLE extra(id INT);")
            cli("save", "after")
            cli("save", "qa-note", "-m", "QA checkpoint")
            latest = json.loads(cli("status", "--json"))["latest"]
            assert latest["tag"] == "qa-note" and latest["message"] == "QA checkpoint"
            assert "qa-note" in cli("status")
            cli("tag", "qa-note", "qa-renamed")
            cli("pin", "qa-renamed")
            cli("rm", "qa-renamed", error=True)
            cli("unpin", "qa-renamed")
            cli("rm", "qa-renamed")
            catalog = json.loads(cli("list", "--json"))
            ephemeral = json.loads(cli("diff-live", "before", "--ephemeral"))
            assert ephemeral["live_snapshot_retained"] is False
            assert "live_snapshot_id" not in ephemeral and "extra" in ephemeral["comparison"]
            assert json.loads(cli("list", "--json")) == catalog
            if options.engine != "mysql":
                def migration(query):
                    if options.engine == "sqlite":
                        return [sys.executable, "-c",
                                "import sqlite3,os; c=sqlite3.connect(os.environ['DBSNAP_SANDBOX_DB']); "
                                f"c.executescript({query!r}); c.close()"]
                    return [sys.executable, "-c",
                            "import subprocess; subprocess.run(['psql','-X','-w','-v','ON_ERROR_STOP=1',"
                            f"'-c',{query!r}],check=True)"]

                def rehearsal(command, expected=0, extra=()):
                    result = subprocess.run(
                        [str(binary), "rehearse", "before", "--db", url, *extra, "--", *command],
                        cwd=root, capture_output=True, text=True, timeout=120)
                    assert result.returncode == expected, result.stderr + result.stdout
                    report = json.loads(result.stdout)
                    # Short QA passwords can also occur in temp paths and are
                    # deliberately masked in output and persisted reports.
                    persisted = [json.loads(path.read_text()) for path in
                                 (root / ".dbsnap" / "rehearsals").glob("*/report.json")]
                    assert report in persisted and report["sandbox_retained"], report
                    assert rows() == 2
                    if options.engine == "sqlite":
                        assert sql("SELECT COUNT(*) FROM sqlite_master WHERE type='table'") == "2"
                    else:
                        assert sql("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='public'") == "2"
                    return report

                report = rehearsal(migration("CREATE TABLE rehearsal_probe(id INT)"))
                assert report["status"] == "passed" and "rehearsal_probe" in report["summary"]
                report = rehearsal(migration("DROP TABLE items"), expected=3,
                                   extra=("--fail-on-drop-table",))
                assert report["status"] == "policy_failed" and report["policy"]["violated"]
                report = rehearsal([sys.executable, "-c", "raise SystemExit(7)"], expected=4)
                assert report["command_exit_code"] == 7 and report["status"] == "command_failed"
                report = rehearsal([sys.executable, "-c", "import time; time.sleep(15)"],
                                   expected=5, extra=("--timeout-seconds", "1"))
                assert report["timed_out"] and report["status"] == "timed_out"
                report = rehearsal(["dbsnap-nonexistent-qa-command"], expected=1)
                assert report["status"] == "infrastructure_failed"
                assert json.loads(cli("list", "--json")) == catalog
            assert json.loads(cli("verify"))["ok"]
            assert json.loads(cli("doctor"))["mutations_performed"] is False
            command_result = json.loads(cli("exec", "--", sys.executable, "-c", "print('checkpoint child')"))
            assert command_result["success"] and command_result["snapshot_id"]
            cli("exec", "--", sys.executable, "-c", "raise SystemExit(7)", error=True)
            if options.engine == "sqlite":
                fork = root / "experiment.db"
                fork_result = json.loads(cli("sandbox", "before", "--output", str(fork)))
                assert fork_result["source_modified"] is False
                sandbox_state(fork_result["snapshot_id"], "present")
                with closing(sqlite3.connect(fork)) as connection:
                    assert connection.execute("SELECT COUNT(*) FROM items").fetchone()[0] == 1
                    connection.execute("DELETE FROM items")
                    connection.commit()
                assert rows() == 2
                fork.unlink()
                sandbox_state(fork_result["snapshot_id"], "missing", "doctor")
            elif options.engine == "postgres":
                sandbox = json.loads(cli("sandbox", "before"))
                assert sandbox["source_modified"] is False
                # CLI output masks credentials even when a short password is
                # also part of a generated identifier (CI uses "dbsnap").
                # Read the exact owned resource from its local registry.
                registry = json.loads((root / ".dbsnap" / "sandboxes.json").read_text())
                fork = registry[-1]["name"]
                assert fork.startswith("__dbsnap_sandbox_") and fork.replace('_','').isalnum()
                try:
                    sandbox_state(sandbox["snapshot_id"], "present")
                    assert sql("SELECT COUNT(*) FROM items", database_override=fork) == "1"
                    sql("DELETE FROM items", database_override=fork)
                    assert rows() == 2, "sandbox writes must not change source"
                finally:
                    sql(f'DROP DATABASE "{fork}"', admin=True)
                sandbox_state(sandbox["snapshot_id"], "missing", "doctor")
            else:
                cli("sandbox", "before", error=True)
                if options.docker_sandbox:
                    image = "mariadb:11" if parsed.scheme == "mariadb" else "mysql:8.0"
                    client = "mariadb" if parsed.scheme == "mariadb" else "mysql"
                    sandbox=json.loads(cli("sandbox","before","--docker-image",image))
                    container=sandbox["container_id"]
                    assert len(container)==64 and all(c in '0123456789abcdef' for c in container)
                    registry_file = root / ".dbsnap" / "sandboxes.json"
                    original_registry = registry_file.read_bytes()
                    removed = False
                    try:
                        sandbox_state(sandbox["snapshot_id"], "present")
                        inspected=json.loads(checked(["docker","inspect",container]))[0]
                        assert inspected["HostConfig"]["NetworkMode"]=="none"
                        assert not inspected["HostConfig"]["PortBindings"]
                        count=checked(["docker","exec",container,client,"--no-defaults","-uroot","-N","-e",f"SELECT COUNT(*) FROM `{name}`.items"])
                        assert count.strip()=="1", count
                        checked(["docker","exec",container,client,"--no-defaults","-uroot","-e",f"DELETE FROM `{name}`.items"])
                        assert rows()==2, "sandbox writes must not change source"
                        cli("sandbox-remove", container[:12], error=True)
                        with tempfile.TemporaryDirectory(prefix="dbsnap-other-owner-") as other:
                            checked([str(binary), "init", "--db", url], cwd=other)
                            (Path(other) / ".dbsnap" / "sandboxes.json").write_bytes(original_registry)
                            denied = subprocess.run([str(binary), "sandbox-remove", container, "--db", url],
                                                    cwd=other, capture_output=True, text=True, timeout=120)
                            assert denied.returncode != 0 and "ownership could not be verified" in denied.stderr
                        sandbox_state(sandbox["snapshot_id"], "present")
                        assert json.loads(cli("sandbox-remove", container))["removed"]
                        removed = True
                        assert json.loads(cli("sandbox-list"))["sandboxes"] == []
                    finally:
                        if not removed:
                            checked(["docker","rm","-f","-v",container])
                    registry_file.write_bytes(original_registry)  # simulate a stale entry
                    sandbox_state(sandbox["snapshot_id"], "missing", "doctor")
            if options.disk_full:
                assert options.engine == "mysql"
                fault_env = os.environ.copy()
                remote_full = fault_env.get("DBSNAP_QA_REMOTE_DEV_FULL") == "1"
                if remote_full:
                    # Windows QA adapter forwards this to a dump client in a
                    # disposable Linux container using --result-file=/dev/full.
                    fault_env["DBSNAP_TEST_DUMP_DISK_FULL"] = "1"
                else:
                    assert Path("/dev/full").exists(), "requires Unix /dev/full or the remote QA adapter"
                    real_dump = find_dump_client(parsed.scheme)
                    assert real_dump, "dump client is required"
                    wrappers = root / "full-client"
                    wrappers.mkdir()
                wrapper = '''#!/usr/bin/env python3
import os, sys
args = sys.argv[1:]
if '--version' not in args:
    clean = []
    source = iter(args)
    for arg in source:
        if arg == '--result-file':
            next(source)
        elif not arg.startswith('--result-file='):
            clean.append(arg)
    args = clean + ['--result-file=/dev/full']
os.execv(os.environ['DBSNAP_QA_REAL_DUMP'], [os.environ['DBSNAP_QA_REAL_DUMP']] + args)
'''
                if not remote_full:
                    for tool in ("mysqldump", "mariadb-dump"):
                        path = wrappers / tool
                        path.write_text(wrapper)
                        path.chmod(0o700)
                    fault_env["PATH"] = str(wrappers) + os.pathsep + fault_env["PATH"]
                    fault_env["DBSNAP_QA_REAL_DUMP"] = real_dump
                before = (root / ".dbsnap" / "metadata.json").read_bytes()
                result = subprocess.run([str(binary), "restore", "before", "--db", url], cwd=root,
                                        env=fault_env, capture_output=True, text=True, timeout=120)
                assert result.returncode != 0, result.stdout
                # Some MySQL clients report "errno 0 on write" on a failed
                # fclose; /dev/full still provides a real kernel ENOSPC sink.
                assert any(marker in result.stderr.lower() for marker in ("errno 28", "no space left", "error 28", "on write")), result.stderr
                assert rows() == 2, "disk full must abort before target cleanup"
                assert not (root / ".dbsnap" / "mysql-restore.json").exists()
                assert before == (root / ".dbsnap" / "metadata.json").read_bytes()
                assert not list((root / ".dbsnap" / "tmp").iterdir())
                print("PASS real /dev/full dump-client failure preserves target, metadata and temporary-file cleanup")
            assert "rows: 1 -> 2" in cli("diff", "before", "after")
            report = json.loads(cli("diff-report", "before", "after", "--fail-on-drop-table"))
            assert report["removed_table_count"] == 0 and not report["policy"]["violated"]
            dropped = subprocess.run(
                [str(binary), "diff-report", "after", "before", "--fail-on-drop-table", "--db", url],
                cwd=root, capture_output=True, text=True, timeout=120,
            )
            assert dropped.returncode == 3, dropped.stderr
            report = json.loads(dropped.stdout)
            assert report["removed_table_count"] == 1 and report["policy"]["violated"]
            assert report["removed_tables"][0].rsplit(".", 1)[-1] == "extra"
            archive = str(root / "before.dbsnap")
            if options.engine == "sqlite":
                partial = root / "partial.dbsnap"
                catalog_file = root / ".dbsnap" / "metadata.json"
                catalog_before = catalog_file.read_bytes()
                fault_env = dict(os.environ, DBSNAP_FAILPOINT="export_disk_full_before_sync")
                failed = subprocess.run(
                    [str(binary), "export", "before", "--output", str(partial), "--db", url],
                    cwd=root, env=fault_env, capture_output=True, text=True, timeout=120,
                )
                assert failed.returncode == 1 and "simulated disk-full" in failed.stderr
                assert not partial.exists() and catalog_file.read_bytes() == catalog_before
                interrupted("export_before_payload", "export", "before", "--output", str(partial))
                assert partial.is_file()
                assert "payload length mismatch" in cli("import", str(partial), error=True)
                assert catalog_file.read_bytes() == catalog_before and rows() == 2
                partial.unlink()
                cli("export", "before", "--output", str(partial))
                assert partial.stat().st_size > 0
            cli("export", "before", "--output", archive)
            cli("import", archive, "--tag", "imported")
            assert json.loads(cli("verify", "imported"))["ok"]
            assert rows() == 2, "import must only register a snapshot"
            catalog = json.loads(cli("list", "--json"))
            assert catalog
            assert json.loads(cli("status", "--json"))["snapshot_count"] >= 2
            assert "before" in cli("list")
            assert "snapshots" in cli("status")
            if options.engine == "postgres":
                registry_path = root / ".dbsnap" / "sandboxes.json"
                registry_before = registry_path.read_bytes()
                marker = interrupted("pg_sandbox_after_replay", "sandbox", "imported")
                assert marker.startswith("interrupted sandbox suffix: "), marker
                suffix = marker.removeprefix("interrupted sandbox suffix: ")
                assert len(suffix) == 6 and all(c in "0123456789abcdef" for c in suffix)
                orphan = "__dbsnap_sandbox_" + suffix
                try:
                    assert registry_path.read_bytes() == registry_before
                    assert rows() == 2
                    assert sql("SELECT COUNT(*) FROM items", database_override=orphan) == "1"
                finally:
                    sql(f'DROP DATABASE "{orphan}"', admin=True)
                sandbox = json.loads(cli("sandbox", "imported"))
                assert sandbox["source_modified"] is False
                registry = json.loads((root / ".dbsnap" / "sandboxes.json").read_text())
                fork = registry[-1]["name"]
                assert fork.startswith("__dbsnap_sandbox_") and fork.replace("_", "").isalnum()
                try:
                    assert sql("SELECT COUNT(*) FROM items", database_override=fork) == "1"
                finally:
                    sql(f'DROP DATABASE "{fork}"', admin=True)
                preview = json.loads(cli("diff-rows", "imported", "after", "--table", "items", "--replay-dumps"))
                assert preview["added"] == 1 and not preview["row_values_disclosed"]
                replay_session = Mcp(binary, root, url, True)
                try:
                    args = {"from": "imported", "to": "after", "table": "items", "replay_dumps": True}
                    replay_session.tool("dbsnap_diff_rows", args, error=True, approve=False)
                    assert json.loads(replay_session.tool("dbsnap_diff_rows", args))["added"] == 1
                finally:
                    replay_session.close()
                cli("save", "portable", "--method", "dump")
                cli("save", "portable-second", "--method", "dump")
                assert "no archive schema or table row-count differences" in cli("diff","portable","portable-second")
                assert "requires PostgreSQL clones" in cli("diff-report", "portable", "portable-second", "--fail-on-drop-table", error=True)
                cli("diff", "before", "portable", error=True)
                cli("restore", "portable")
                assert rows() == 2
                # Hold pg_restore inside an index build, then terminate its
                # backend to exercise cleanup after a real lost connection.
                index_name = "net_idx_" + name
                sql(f"""
                    CREATE FUNCTION hold_sandbox_restore(x integer) RETURNS integer
                    LANGUAGE plpgsql IMMUTABLE AS $$
                    BEGIN
                        IF left(current_database(), 17) = '__dbsnap_sandbox_' THEN
                            PERFORM pg_sleep(25);
                        END IF;
                        RETURN x;
                    END $$;
                    CREATE INDEX {index_name} ON items(hold_sandbox_restore(id));
                """)
                cli("save", "network-fault", "--method", "dump")
                proc = subprocess.Popen([str(binary), "sandbox", "network-fault", "--db", url],
                                        cwd=root, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                terminated = None
                try:
                    for _ in range(20):
                        active = sql(
                            "SELECT pid::text || ':' || datname FROM pg_stat_activity "
                            "WHERE pid <> pg_backend_pid() AND state='active' "
                            "AND left(datname, 17)='__dbsnap_sandbox_' "
                            f"AND query LIKE '%CREATE INDEX%{index_name}%'",
                            admin=True,
                        )
                        if active:
                            pid, terminated = active.split(":", 1)
                            assert pid.isdecimal()
                            assert sql(f"SELECT pg_terminate_backend({int(pid)})", admin=True) == "t"
                            break
                        if proc.poll() is not None:
                            break
                        time.sleep(0.2)
                    _, error = proc.communicate(timeout=90)
                    assert terminated and proc.returncode != 0 and "pg_restore into staging failed" in error, error
                    suffix = terminated.removeprefix("__dbsnap_sandbox_")
                    assert len(suffix) == 6 and all(c in "0123456789abcdef" for c in suffix)
                    assert sql(f"SELECT COUNT(*) FROM pg_database WHERE datname='{terminated}'", admin=True) == "0"
                    assert rows() == 2
                finally:
                    if proc.poll() is None:
                        proc.kill()
                        proc.communicate()
                    sql("DROP FUNCTION hold_sandbox_restore(integer) CASCADE")
            cli("pin", "before")
            cli("prune", "--keep", "1")
            assert "before" in cli("list")
            cli("unpin", "before")
            cli("restore", "before")
            assert rows() == 1
            cli("rollback")
            assert rows() == 2
            if options.engine == "mysql":
                failure_env = os.environ.copy()
                failure_env["DBSNAP_FAILPOINT"] = "mysql_exit_after_journal"
                failure = subprocess.run([str(binary), "restore", "before", "--db", url], cwd=root,
                                         env=failure_env, capture_output=True, text=True, timeout=120)
                assert failure.returncode != 0
                assert json.loads(cli("doctor"))["pending_recovery"]
                cli("recover")
                assert rows() == 2

            session = Mcp(binary, root, url, False)
            try:
                tools = session.call("tools/list", {})["result"]["tools"]
                assert len(tools) == 11
                session.tool("dbsnap_rehearse", {"snapshot": "before", "command": [sys.executable, "-c", "raise SystemExit(9)"]}, error=True)
                verified = json.loads(cli("verify"))
                assert verified["ok"]
                assert json.loads(cli("doctor"))["mutations_performed"] is False
                session.tool("dbsnap_verify")
                session.tool("dbsnap_doctor")
                live = json.loads(session.tool("dbsnap_diff_live", {"from":"before"}))
                assert live["live_snapshot_retained"]
                if options.engine != "mysql":
                    args = {"from": "before", "to": live["live_snapshot_id"], "table": "items"}
                    preview = json.loads(session.tool("dbsnap_diff_rows", args))
                    assert preview["added"] == 1 and not preview["row_values_disclosed"]
                    assert json.loads(cli("diff-rows", "before", live["live_snapshot_id"], "--table", "items"))["added"] == 1
                    scoped = dict(args, columns=["value"], key_min=2, key_max=2, limit=1)
                    preview = json.loads(session.tool("dbsnap_diff_rows", scoped))
                    assert preview["added"] == 1 and preview["removed"] == 0 and preview["updated"] == 0
                elif options.docker_sandbox:
                    session.tool("dbsnap_diff_rows", {"from": "before", "to": live["live_snapshot_id"],
                                                      "table": "items", "docker_image": image}, error=True)
                session.tool("dbsnap_status")
                session.tool("dbsnap_list_snapshots")
                session.tool("dbsnap_create_snapshot", {"tag": "mcp-before"})
                sql("INSERT INTO items VALUES(3,'third')")
                session.tool("dbsnap_create_snapshot", {"tag": "mcp-after"})
                assert "rows: 2 -> 3" in session.tool("dbsnap_diff", {"from": "mcp-before", "to": "mcp-after"})
                session.tool("dbsnap_restore_snapshot", {"tag_or_id": "mcp-before"}, error=True)
                session.tool("dbsnap_prune", {"keep": 1}, error=True)
                assert rows() == 3
            finally:
                session.close()

            session = Mcp(binary, root, url, True)
            try:
                args = {"snapshot": "before", "command": [sys.executable, "-c", "raise SystemExit(9)"]}
                runs = set((root / ".dbsnap" / "rehearsals").glob("*"))
                session.tool("dbsnap_rehearse", args, error=True, approve=False)
                assert set((root / ".dbsnap" / "rehearsals").glob("*")) == runs
                if options.engine != "mysql":
                    args["command"] = migration("CREATE TABLE mcp_rehearsal_probe(id INT)")
                    report = json.loads(session.tool("dbsnap_rehearse", args))
                    assert report["status"] == "passed" and rows() == 3
                    args["command"] = [sys.executable, "-c", "raise SystemExit(9)"]
                    report = json.loads(session.tool("dbsnap_rehearse", args, error=True))
                    assert report["status"] == "command_failed" and report["command_exit_code"] == 9
                else:
                    session.tool("dbsnap_rehearse", args, error=True)
                if options.engine == "mysql" and options.docker_sandbox:
                    args = {"from": "before", "to": live["live_snapshot_id"], "table": "items",
                            "docker_image": image, "columns": ["value"], "key_min": 2,
                            "key_max": 2, "limit": 1}
                    session.tool("dbsnap_diff_rows", args, error=True, approve=False)
                    preview = json.loads(session.tool("dbsnap_diff_rows", args))
                    assert preview["added"] == 1 and not preview["row_values_disclosed"]
                session.tool("dbsnap_restore_snapshot", {"tag_or_id": "mcp-before"}, error=True, approve=False)
                assert rows() == 3
                session.tool("dbsnap_restore_snapshot", {"tag_or_id": "mcp-before"})
                assert rows() == 2
                session.tool("dbsnap_prune", {"keep": 1})
                session.tool("dbsnap_diff", {"from": "missing", "to": "missing"}, error=True)
            finally:
                session.close()

            # Unpin all catalogued records before CLI prune --all.
            metadata = json.loads((root / ".dbsnap" / "metadata.json").read_text())
            for record in metadata["snapshots"]:
                if record["pinned"]:
                    cli("unpin", record["id"])
            cli("prune", "--all")
            assert json.loads(cli("status", "--json"))["snapshot_count"] == 0
            cli("restore", error=True)
            assert rows() == 2
            print(f"PASS {options.engine}: CLI/MCP including verify/doctor, approval/denial, JSON, diff, prune and data assertions")
        finally:
            if created:
                if options.engine == "postgres":
                    # Only this temporary QA project's recorded resources.
                    owned = set()
                    registry = root / ".dbsnap" / "sandboxes.json"
                    if registry.exists():
                        owned.update(entry["name"] for entry in json.loads(registry.read_text())
                                     if entry.get("engine") == "postgres")
                    catalogs = [root / ".dbsnap" / "metadata.json",
                                *(root / ".dbsnap" / "rehearsals").glob("*/.dbsnap/metadata.json")]
                    for catalog_path in catalogs:
                        if not catalog_path.exists():
                            continue
                        catalog = json.loads(catalog_path.read_text())
                        owned.update(s["location"]["name"] for s in catalog["snapshots"]
                                     if s["location"]["kind"] == "pg_database")
                    for resource in owned:
                        assert resource.startswith("__dbsnap_") and resource.replace("_", "").isalnum()
                        sql(f'DROP DATABASE IF EXISTS "{resource}"', admin=True)
                sql(f"DROP DATABASE {quoted}", admin=True)


if __name__ == "__main__":
    main()
