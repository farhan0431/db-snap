#!/usr/bin/env python3
"""Benchmark self-diff with disposable SQLite, PostgreSQL, or MySQL fixtures."""

import argparse
from contextlib import closing, contextmanager
import json
import os
from pathlib import Path
import random
import sqlite3
import statistics
import subprocess
import tempfile
import time
from urllib.parse import unquote, urlsplit, urlunsplit
import uuid


def checked(args, **kwargs):
    result = subprocess.run(args, capture_output=True, text=True, timeout=180, **kwargs)
    if result.returncode != 0:
        raise RuntimeError(f"{Path(str(args[0])).name} exited {result.returncode}: {result.stderr}")
    return result.stdout


@contextmanager
def fixture(root, engine, run):
    if engine == "sqlite":
        database = root / "wide.db"
        with closing(sqlite3.connect(database)) as connection:
            connection.execute("CREATE TABLE items(id INTEGER PRIMARY KEY, label TEXT, payload BLOB)")
            rng = random.Random(132)
            connection.executemany(
                "INSERT INTO items VALUES (?, ?, ?)",
                ((index, "sample", rng.randbytes(700)) for index in range(8000)),
            )
            connection.commit()
        yield str(database), ()
        return

    key = "DBSNAP_TEST_PG_URL" if engine == "postgres" else "DBSNAP_TEST_MYSQL_URL"
    base = urlsplit(os.environ[key])
    if base.hostname not in ("localhost", "127.0.0.1", "::1"):
        raise ValueError("benchmarks require a disposable loopback database")
    name = "dbsnap_bench_" + uuid.uuid4().hex[:12]
    url = urlunsplit(base._replace(path="/" + name))
    env = os.environ.copy()
    password = unquote(base.password or "")
    username = unquote(base.username or ("postgres" if engine == "postgres" else "root"))
    admin_database = unquote(base.path.removeprefix("/")) or ("postgres" if engine == "postgres" else "mysql")
    if engine == "postgres":
        env["PGPASSWORD"] = password
        for key in ("PGHOSTADDR", "PGSERVICE", "PGSERVICEFILE"):
            env.pop(key, None)

        def sql(statement, database):
            checked(["psql", "-X", "-w", "-h", base.hostname, "-p", str(base.port or 5432),
                     "-U", username, "-d", database, "-v", "ON_ERROR_STOP=1", "-c", statement],
                    env=env)

        seed = (
            "CREATE TABLE items(id integer PRIMARY KEY, label text, payload text); "
            "INSERT INTO items SELECT n, 'sample', repeat(md5(n::text),22) "
            "FROM generate_series(0,7999) AS n;"
        )
        flags = ()
    else:
        env["MYSQL_PWD"] = password

        def sql(statement, database):
            checked(["mysql", "--no-defaults", "-h", base.hostname, "-P", str(base.port or 3306),
                     "-u", username, database, "-e", statement], env=env)

        digits = " UNION ALL ".join(f"SELECT {digit} AS digit" for digit in range(10))
        grid = " CROSS JOIN ".join(f"({digits}) {alias}" for alias in "abcd")
        seed = (
            "CREATE TABLE items(id integer PRIMARY KEY, label text, payload text); "
            "INSERT INTO items SELECT n, 'sample', REPEAT(MD5(n),22) FROM "
            f"(SELECT a.digit + 10*b.digit + 100*c.digit + 1000*d.digit AS n FROM {grid}) seq "
            "WHERE n < 8000;"
        )
        flags = ("--docker-image", "mariadb:11" if base.scheme == "mariadb" else "mysql:8.0")

    sql(f"CREATE DATABASE {name}", admin_database)
    try:
        sql(seed, name)
        yield url, flags
    finally:
        try:
            if (root / ".dbsnap" / "metadata.json").is_file():
                run(url, "prune", "--all")
        finally:
            sql(f"DROP DATABASE {name}", admin_database)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, type=Path)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--engine", choices=("sqlite", "postgres", "mysql"), default="sqlite")
    parser.add_argument("--runs", type=int, default=7)
    options = parser.parse_args()
    if options.runs < 1:
        parser.error("--runs must be positive")
    baseline = options.baseline.resolve(strict=True)
    binary = options.binary.resolve(strict=True)

    with tempfile.TemporaryDirectory(prefix="dbsnap-row-bench-") as directory:
        root = Path(directory)

        def run(executable, database, *args):
            env = dict(os.environ, DATABASE_URL=database)
            return checked([str(executable), *args], cwd=root, env=env)

        with fixture(root, options.engine, lambda db, *args: run(binary, db, *args)) as (db, flags):
            run(binary, db, "init")
            run(binary, db, "save", "bench")
            args = ("diff-rows", "bench", "bench", "--table", "items")
            cases = [
                ("baseline self-diff", baseline, ()),
                ("new self-diff", binary, ()),
                ("new 100-row projected self-diff", binary,
                 ("--column", "label", "--key-min", "100", "--key-max", "199")),
            ]
            samples = {name: [] for name, _, _ in cases}
            if options.engine != "mysql":
                for _, executable, projection in cases:
                    run(executable, db, *args, *flags, *projection)
            for _ in range(options.runs):
                for name, executable, projection in cases:
                    started = time.perf_counter()
                    result = json.loads(run(executable, db, *args, *flags, *projection))
                    elapsed = (time.perf_counter() - started) * 1000
                    samples[name].append(elapsed)
                    if any(result[key] != 0 for key in ("added", "removed", "updated")):
                        raise RuntimeError("self-diff returned changes")
                    if options.engine == "mysql":
                        print(f"{name}: sample {len(samples[name])}, {elapsed:.1f} ms", flush=True)
            for name, elapsed in samples.items():
                print(f"{name}: {statistics.median(elapsed):.1f} ms (median, n={len(elapsed)})")


if __name__ == "__main__":
    main()
