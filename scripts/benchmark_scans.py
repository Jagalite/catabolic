#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Disposable local scan benchmark. Slow I/O is simulated, never a NAS claim."""

import argparse
import json
import os
import tempfile
import time
import tracemalloc
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.scan_execution import Publisher
from catabolic.store import Store


def benchmark(kind, count):
    with tempfile.TemporaryDirectory(prefix="catabolic-benchmark-") as directory:
        root = Path(directory).resolve()
        source = root / "source"
        source.mkdir()
        if kind == "deep":
            fd = os.open(source, os.O_RDONLY | os.O_DIRECTORY)
            for _ in range(count):
                os.mkdir("d", dir_fd=fd)
                child = os.open("d", os.O_RDONLY | os.O_DIRECTORY, dir_fd=fd)
                os.close(fd)
                fd = child
            os.close(fd)
        else:
            for n in range(count):
                parent = source / str(n // 50) if kind == "many-small" else source
                parent.mkdir(exist_ok=True)
                (parent / f"{n}.txt").write_bytes(b"x")
        database = root / "catalog.sqlite3"
        Store.initialize(database)
        publication_seconds = 0
        maximum_buffer = 0
        original = Publisher._flush
        stat = os.stat

        def publish(publisher):
            nonlocal publication_seconds, maximum_buffer
            maximum_buffer = max(maximum_buffer, publisher.bytes)
            started = time.perf_counter()
            try:
                return original(publisher)
            finally:
                publication_seconds += time.perf_counter() - started

        def metadata(path, *args, **kwargs):
            if kind == "simulated-slow" and kwargs.get("dir_fd") is not None:
                time.sleep(0.001)
            return stat(path, *args, **kwargs)

        with Store(database, writable=True) as store:
            app = Application(store)
            app.bind("source", "media", str(source))
            tracemalloc.start()
            started = time.perf_counter()
            with (
                patch.object(Publisher, "_flush", publish),
                patch("catabolic.scan_traversal.os.stat", new=metadata),
            ):
                report = app.scan("media", extended=True)
            elapsed = time.perf_counter() - started
            peak = tracemalloc.get_traced_memory()[1]
            tracemalloc.stop()
        scan = report["scans"][0]
        return dict(
            fixture=kind,
            requested_count=count,
            complete=report["complete"],
            observed=scan["observed"],
            batches=scan["batches"],
            elapsed_seconds=elapsed,
            publication_seconds=publication_seconds,
            traversal_and_orchestration_seconds=elapsed - publication_seconds,
            peak_python_bytes=peak,
            maximum_buffer_bytes=maximum_buffer,
            budgets=scan["budgets"],
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--files", type=int, default=2000)
    parser.add_argument("--depth", type=int, default=80)
    args = parser.parse_args()
    results = [
        benchmark(kind, args.depth if kind == "deep" else args.files)
        for kind in ("deep", "wide", "many-small", "simulated-slow")
    ]
    print(
        json.dumps(
            dict(
                environment="local disposable filesystem; simulated-slow injects 1 ms per metadata stat; tracemalloc enabled",
                results=results,
            ),
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
