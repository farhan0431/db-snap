#!/usr/bin/env python3
"""Real InnoDB table-space exhaustion on a dedicated bounded test server.

Requires DBSNAP_TEST_MYSQL_STORAGE_URL and a server started with
--innodb-file-per-table=OFF --innodb-data-file-path=ibdata1:12M:autoextend:max:128M
Never use a shared server. This fills its bounded InnoDB tablespace, not the
host filesystem. The client password is passed through MYSQL_PWD.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import uuid
from urllib.parse import unquote, urlsplit, urlunsplit


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    parser.add_argument("--exercise-server-restart", action="store_true",
                        help="exercise the explicit Docker fixture expansion/restart recovery path")
    options = parser.parse_args()
    binary = str(Path(options.binary).resolve())
    parsed = urlsplit(os.environ["DBSNAP_TEST_MYSQL_STORAGE_URL"])
    assert parsed.hostname in ("127.0.0.1", "localhost", "::1")
    env = os.environ.copy()
    env["MYSQL_PWD"] = unquote(parsed.password or "")
    base = ["mysql", "--no-defaults", "--protocol=TCP", "-h", parsed.hostname, "-P", str(parsed.port or 3306),
            "-u", unquote(parsed.username or "root"), "-N", "-B"]

    def sql(query, database=None, failure=False):
        result = subprocess.run(base + ([database] if database else []) + ["-e", query],
                                env=env, text=True, capture_output=True, timeout=120)
        if failure:
            return result
        assert result.returncode == 0, result.stderr
        return result.stdout.strip()

    assert sql("SELECT @@GLOBAL.innodb_file_per_table") == "0", "requires a dedicated bounded server"
    setting = sql("SELECT @@GLOBAL.innodb_data_file_path")
    bound = re.search(r"max:(\d+)M", setting, re.I)
    assert bound and int(bound[1]) <= 256, "refusing an unbounded or oversized test server"
    token = uuid.uuid4().hex[:12]
    target, filler = "dbsnap_full_" + token, "dbsnap_fill_" + token
    created = []
    repair_container = None

    def repair_server():
        """Expand only an explicitly named disposable Docker fixture.

        MariaDB can abort when rollback itself runs out of shared tablespace.
        Reuse its datadir with a larger bound, as an operator would do before
        retrying dbsnap recovery. No Docker dependency is added to dbsnap.
        """
        nonlocal repair_container
        container = os.environ.get("DBSNAP_TEST_STORAGE_CONTAINER")
        assert container, "server stopped under storage pressure; expand/restart it before recovery (Docker QA fixture ID not supplied)"
        inspected = subprocess.run(["docker", "inspect", container], capture_output=True, text=True, check=True)
        fixture = json.loads(inspected.stdout)[0]
        image = fixture["Config"]["Image"]
        assert image.startswith(("mysql:", "mariadb:")), "unexpected storage fixture image"
        ports = fixture["HostConfig"]["PortBindings"]["3306/tcp"]
        assert any(int(p["HostPort"]) == parsed.port for p in ports), "fixture port does not match test URL"
        subprocess.run(["docker", "stop", container], capture_output=True, check=True, timeout=60)
        flavor = "maria" if image.startswith("mariadb:") else "mysql"
        repair_container = f"dbsnap-storage-recovery-{flavor}-{token}"
        expanded = int(bound[1]) + 128
        subprocess.run(["docker", "run", "-d", "--name", repair_container,
                        "--volumes-from", container, "-p", f"127.0.0.1:{parsed.port}:3306",
                        image, "--innodb-file-per-table=OFF",
                        f"--innodb-data-file-path=ibdata1:12M:autoextend:max:{expanded}M"],
                       capture_output=True, check=True, timeout=60)
        # Windows adapters address a container directly rather than its port.
        if "DBSNAP_TEST_CONTAINER" in env:
            env["DBSNAP_TEST_CONTAINER"] = repair_container
            os.environ["DBSNAP_TEST_CONTAINER"] = repair_container
        for _ in range(60):
            if sql("SELECT 1", failure=True).returncode == 0:
                print("Server restarted with expanded bounded storage; retrying recovery", flush=True)
                return
            time.sleep(1)
        raise AssertionError("expanded fixture did not recover within 60 seconds")
    try:
        for database in (target, filler):
            sql(f"CREATE DATABASE `{database}`")
            created.append(database)
        sql("CREATE TABLE items(id INT PRIMARY KEY, payload LONGBLOB)", target)
        sql(";".join(f"INSERT INTO items VALUES({i},REPEAT('snapshot',32768))" for i in range(32)), target)
        url = urlunsplit(parsed._replace(path="/" + target))
        with tempfile.TemporaryDirectory(prefix="dbsnap-storage-full-") as root:
            def cli(*args, fail=False):
                result = subprocess.run([binary, *args, "--db", url], cwd=root, capture_output=True,
                                        text=True, timeout=180)
                assert (result.returncode != 0) == fail, result.stderr
                return result
            cli("save", "large")
            sql("TRUNCATE TABLE items; INSERT INTO items VALUES(999,'current')", target)
            sql("CREATE TABLE blocks(id BIGINT AUTO_INCREMENT PRIMARY KEY, payload LONGBLOB)", filler)
            # Fill in coarse then fine allocations. Each failed INSERT is
            # transactional. The server's max tablespace size bounds the test.
            for batch in (32, 1):
                select = " UNION ALL ".join("SELECT REPEAT('f',262144)" for _ in range(batch))
                for _ in range(1024):
                    result = sql("INSERT INTO blocks(payload) " + select, filler, failure=True)
                    if result.returncode:
                        assert "1114" in result.stderr or "full" in result.stderr.lower(), result.stderr
                        break
                else:
                    raise AssertionError("bounded tablespace did not fill")
            failed = cli("restore", "large", fail=True)
            assert any(marker in failed.stderr.lower() for marker in ("1114", "full", "2013", "2003")), failed.stderr
            journal = Path(root) / ".dbsnap" / "mysql-restore.json"
            assert journal.exists(), "expected failure after recovery journal commit"
            recovery = json.loads(journal.read_text())
            metadata = json.loads((journal.parent / "metadata.json").read_text())
            assert any(r["id"] == recovery and r["pinned"] for r in metadata["snapshots"])
            if options.exercise_server_restart or sql("SELECT 1", failure=True).returncode != 0:
                repair_server()
            # Release space before recovery, as an operator would do.
            sql(f"DROP DATABASE `{filler}`")
            created.remove(filler)
            cli("restore", recovery)
            assert sql("SELECT COUNT(*) FROM items", target) == "1"
            assert sql("SELECT payload FROM items WHERE id=999", target) == "current"
            assert not journal.exists()
            print("PASS real InnoDB tablespace-full replay failure retains journal/safety and recovers original data")
    finally:
        try:
            for database in reversed(created):
                sql(f"DROP DATABASE `{database}`")
        finally:
            if repair_container:
                subprocess.run(["docker", "rm", "-f", repair_container], capture_output=True, check=True, timeout=60)


if __name__ == "__main__":
    main()
