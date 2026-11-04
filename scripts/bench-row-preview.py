#!/usr/bin/env python3
"""Compare self-diff latency for two dbsnap binaries on a disposable SQLite fixture."""

import argparse
from contextlib import closing
import json
from pathlib import Path
import random
import sqlite3
import statistics
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, type=Path)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--runs", type=int, default=7)
    options = parser.parse_args()
    if options.runs < 1:
        parser.error("--runs must be positive")
    baseline = options.baseline.resolve(strict=True)
    binary = options.binary.resolve(strict=True)

    with tempfile.TemporaryDirectory(prefix="dbsnap-row-bench-") as directory:
        root = Path(directory)
        database = root / "wide.db"
        with closing(sqlite3.connect(database)) as connection:
            connection.execute("CREATE TABLE items(id INTEGER PRIMARY KEY, label TEXT, payload BLOB)")
            rng = random.Random(132)
            connection.executemany(
                "INSERT INTO items VALUES (?, ?, ?)",
                ((index, "sample", rng.randbytes(700)) for index in range(8000)),
            )
            connection.commit()

        def run(executable, *args):
            result = subprocess.run(
                [str(executable), *args, "--db", str(database)],
                cwd=root, capture_output=True, text=True, check=True, timeout=120,
            )
            return result.stdout

        run(binary, "init")
        run(binary, "save", "bench")
        args = ("diff-rows", "bench", "bench", "--table", "items")
        cases = [
            ("baseline self-diff", baseline, ()),
            ("new self-diff", binary, ()),
            ("new 100-row projected self-diff", binary,
             ("--column", "label", "--key-min", "100", "--key-max", "199")),
        ]
        samples = {name: [] for name, _, _ in cases}
        for _, executable, flags in cases:
            run(executable, *args, *flags)
        for _ in range(options.runs):
            for name, executable, flags in cases:
                started = time.perf_counter()
                result = json.loads(run(executable, *args, *flags))
                samples[name].append((time.perf_counter() - started) * 1000)
                if any(result[key] != 0 for key in ("added", "removed", "updated")):
                    raise RuntimeError("self-diff returned changes")
        for name, elapsed in samples.items():
            print(f"{name}: {statistics.median(elapsed):.1f} ms (median, n={len(elapsed)})")


if __name__ == "__main__":
    main()
