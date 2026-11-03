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
import tempfile
import threading
import uuid
from urllib.parse import urlsplit, urlunsplit, unquote


def checked(args, **kwargs):
    result = subprocess.run(args, capture_output=True, text=True, timeout=120, **kwargs)
    if result.returncode:
        raise AssertionError(f"command failed ({Path(str(args[0])).name}): {result.stderr}")
    return result.stdout


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
            def sql(query, admin=False):
                database = unquote(parsed.path[1:]) if admin else name
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
            result = subprocess.run([str(binary), *args, "--db", url], cwd=root,
                                    capture_output=True, text=True, timeout=120)
            assert (result.returncode != 0) == error, result.stderr
            return result.stdout if not error else result.stderr

        def rows():
            return int(sql("SELECT COUNT(*) FROM items"))

        try:
            checked([str(binary), "--version"])
            checked([str(binary), "--help"])
            for command in ("init", "save", "restore", "rollback", "list", "status", "diff", "pin", "unpin", "prune", "mcp"):
                checked([str(binary), command, "--help"])
            sql("CREATE TABLE items(id INT PRIMARY KEY, value TEXT); INSERT INTO items VALUES(1,'first');")
            cli("init")
            cli("save", "before")
            sql("INSERT INTO items VALUES(2,'second'); CREATE TABLE extra(id INT);")
            cli("save", "after")
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
                    real_dump = shutil.which("mariadb-dump" if parsed.scheme == "mariadb" else "mysqldump")
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
            catalog = json.loads(cli("list", "--json"))
            assert catalog
            assert json.loads(cli("status", "--json"))["snapshot_count"] == 2
            assert "before" in cli("list")
            assert "snapshots" in cli("status")
            if options.engine == "postgres":
                cli("save", "portable", "--method", "dump")
                cli("diff", "before", "portable", error=True)
                cli("restore", "portable")
                assert rows() == 2
            cli("pin", "before")
            cli("prune", "--keep", "1")
            assert "before" in cli("list")
            cli("unpin", "before")
            cli("restore", "before")
            assert rows() == 1
            cli("rollback")
            assert rows() == 2

            session = Mcp(binary, root, url, False)
            try:
                tools = session.call("tools/list", {})["result"]["tools"]
                assert len(tools) == 6
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
            print(f"PASS {options.engine}: all CLI commands, six MCP tools, approval/denial, JSON, diff, prune and data assertions")
        finally:
            if created:
                sql(f"DROP DATABASE {quoted}", admin=True)


if __name__ == "__main__":
    main()
