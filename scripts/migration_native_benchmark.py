#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Matched read-only 10k-item benchmark; timings include snapshot validation."""

import argparse
import hashlib
import json
import platform
import statistics
import subprocess
import tempfile
import time
from pathlib import Path

from catabolic.graphql_query import execute_graphql
from catabolic.sql_query import execute_sql
from catabolic.store import Store, encode


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="catabolic-read-benchmark-") as folder:
        path = Path(folder) / "catalog.sqlite3"
        Store.initialize(path)
        with Store(path, writable=True) as store, store.transaction() as db:
            db.executemany(
                "INSERT INTO items(id,kind,metadata) VALUES(?,?,?)",
                [
                    (
                        f"item-{i:05d}",
                        "movie",
                        encode({"title": f"Movie {i:05d}", "year": 2000 + i % 25}),
                    )
                    for i in range(10000)
                ],
            )
        before = hashlib.sha256(path.read_bytes()).hexdigest()
        worker = subprocess.Popen(
            [str(args.binary), "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        reports = []
        try:
            for kind, document, oracle in [
                (
                    "query",
                    "SELECT kind,count(*) AS count FROM items GROUP BY kind",
                    execute_sql,
                ),
                (
                    "graphql",
                    "{items(first:100,sort:TITLE){nodes{id title year}pageInfo{hasNextPage endCursor}}}",
                    execute_graphql,
                ),
            ]:
                expected = json.loads(json.dumps(oracle(path, document)))
                worker.stdin.write(
                    json.dumps(["--db", str(path), kind, document]) + "\n"
                )
                worker.stdin.flush()
                warmup = json.loads(worker.stdout.readline())
                assert warmup["status"] == 0 and warmup["result"] == expected, warmup
                samples = {"python": [], "native": []}
                for _ in range(7):
                    start = time.perf_counter()
                    actual = json.loads(json.dumps(oracle(path, document)))
                    samples["python"].append(time.perf_counter() - start)
                    assert actual == expected
                    start = time.perf_counter()
                    worker.stdin.write(
                        json.dumps(["--db", str(path), kind, document]) + "\n"
                    )
                    worker.stdin.flush()
                    reply = json.loads(worker.stdout.readline())
                    samples["native"].append(time.perf_counter() - start)
                    assert reply["status"] == 0 and reply["result"] == expected, reply
                medians = {
                    key: statistics.median(values) for key, values in samples.items()
                }
                reports.append(
                    {
                        "operation": kind,
                        "seconds": samples,
                        "median_seconds": medians,
                        "p95_seconds": {
                            key: max(values) for key, values in samples.items()
                        },
                        "native_over_python": medians["native"] / medians["python"],
                    }
                )
        finally:
            worker.stdin.close()
            worker.wait(timeout=10)
            worker.stdout.close()
            worker.stderr.close()
        assert hashlib.sha256(path.read_bytes()).hexdigest() == before
    args.report.write_text(
        json.dumps(
            {
                "platform": platform.platform(),
                "python": platform.python_version(),
                "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
                "items": 10000,
                "warmup": "One untimed oracle and native request for each operation; OS caches uncontrolled",
                "benchmark_script_sha256": hashlib.sha256(
                    Path(__file__).read_bytes()
                ).hexdigest(),
                "samples": 7,
                "database_unchanged": True,
                "results": reports,
            },
            indent=2,
        )
        + "\n"
    )


if __name__ == "__main__":
    main()
