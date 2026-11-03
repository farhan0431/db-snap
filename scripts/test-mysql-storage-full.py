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
import uuid
from urllib.parse import unquote, urlsplit, urlunsplit


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
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
            assert "1114" in failed.stderr or "full" in failed.stderr.lower(), failed.stderr
            journal = Path(root) / ".dbsnap" / "mysql-restore.json"
            assert journal.exists(), "expected failure after recovery journal commit"
            recovery = json.loads(journal.read_text())
            metadata = json.loads((journal.parent / "metadata.json").read_text())
            assert any(r["id"] == recovery and r["pinned"] for r in metadata["snapshots"])
            # Release space before recovery, as an operator would do.
            sql(f"DROP DATABASE `{filler}`")
            created.remove(filler)
            cli("restore", recovery)
            assert sql("SELECT COUNT(*) FROM items", target) == "1"
            assert sql("SELECT payload FROM items WHERE id=999", target) == "current"
            assert not journal.exists()
            print("PASS real InnoDB tablespace-full replay failure retains journal/safety and recovers original data")
    finally:
        for database in reversed(created):
            sql(f"DROP DATABASE `{database}`")


if __name__ == "__main__":
    main()
