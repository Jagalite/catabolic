#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Small synthetic reference timing capture; never a release comparison claim."""

import argparse
import json
import math
import os
import platform
import resource
import socket
import sqlite3
import statistics
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path
from unittest.mock import patch

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from scripts.migration_contracts import REFERENCE, sha, verify_source


def summary(samples):
    values = sorted(samples)
    return dict(
        samples_seconds=samples,
        p50_seconds=statistics.median(values),
        p95_seconds=values[math.ceil(len(values) * 0.95) - 1],
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    source = args.source.resolve()
    inventory = Path("tests/parity/inventory.json").read_bytes()
    verify_source(source, json.loads(inventory))
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=False)
    sys.path[:0] = [str(source), str(source / "src")]
    from catabolic import store as store_module
    from catabolic.access import grant_put, issue, principal_put
    from catabolic.app import Application
    from catabolic.process_runner import CommandFailure, command_output
    from catabolic.processing import Processing
    from catabolic.sql_query import execute_sql
    from catabolic.store import Store

    env = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith("CATABOLIC_")
    }
    env.update(PYTHONPATH=str(source / "src"), PYTHONDONTWRITEBYTECODE="1")
    report = dict(
        reference_commit=REFERENCE,
        inventory_sha256=sha(inventory),
        tool_sha256=sha(Path(__file__).read_bytes()),
        python=sys.version,
        platform=platform.platform(),
        sqlite_version=sqlite3.sqlite_version,
        scope="100 tiny synthetic files; fresh CLI processes with uncontrolled OS caches; loopback HTTP; queued-job and bounded subprocess cancellation. Other discovery benchmarks may contend. Not a cold-filesystem-cache or active-encoder cancellation measurement.",
        measurements={},
    )
    metrics = report["measurements"]

    def measured(call):
        start = time.perf_counter()
        value = call()
        return time.perf_counter() - start, value

    try:
        samples = []
        for _ in range(10):
            seconds, _ = measured(
                lambda: subprocess.run(
                    [sys.executable, "-m", "catabolic", "--version"],
                    cwd=source,
                    env=env,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.PIPE,
                    check=True,
                    timeout=30,
                )
            )
            samples.append(seconds)
        metrics["fresh_process_cli_start"] = summary(samples)
        database = root / "catalog.sqlite3"
        media = root / "media"
        media.mkdir()
        for index in range(100):
            (media / f"sample-{index:03d}.mp4").write_bytes(
                b"synthetic media fixture\n" * 32
            )
        Store.initialize(database)
        holds = []
        acquired = {}
        original_acquire = store_module.acquire_writer_lock
        original_close = os.close

        def acquire(path):
            fd = original_acquire(path)
            acquired[fd] = time.perf_counter()
            return fd

        def close(fd):
            if fd in acquired:
                holds.append(time.perf_counter() - acquired.pop(fd))
            return original_close(fd)

        with (
            patch.object(store_module, "acquire_writer_lock", acquire),
            patch.object(os, "close", close),
        ):
            with Store(database, writable=True) as store:
                app = Application(store)
                app.bind("source", "media", str(media))
                seconds, scan = measured(app.scan)
                if not scan["complete"]:
                    raise RuntimeError("incomplete metrics fixture scan")
                metrics["scan"] = dict(
                    files=100, seconds=seconds, files_per_second=100 / seconds
                )
                files = [
                    r["id"] for r in store.rows("SELECT id FROM files ORDER BY id")
                ]
                if len(files) != 100:
                    raise RuntimeError("unexpected metrics inventory")
                processing = Processing(app)
                admission, cancellation = [], []
                for identifier in files[:10]:
                    seconds, result = measured(
                        lambda identifier=identifier: processing.enqueue(
                            "sniff", file_ids=[identifier]
                        )
                    )
                    if not result["complete"] or len(result["queued"]) != 1:
                        raise RuntimeError("metrics admission failed")
                    admission.append(seconds)
                    seconds, result = measured(
                        lambda identifier=result["queued"][0]: processing.cancel(
                            identifier
                        )
                    )
                    if not result["cancelled"]:
                        raise RuntimeError("metrics cancellation failed")
                    cancellation.append(seconds)
                metrics["job_admission"] = summary(admission)
                metrics["queued_job_cancellation"] = summary(cancellation)
                principal_put(store, "default", "metrics")
                grant = grant_put(
                    store, "metrics", {"actions": ["metadata:read"], "file_ids": files}
                )
                credential = issue(store, "metrics", [grant["grant_id"]])["token"]
                metrics["open_descriptors_store_snapshot"] = len(os.listdir("/dev/fd"))
        metrics["writer_lock_holds_in_instrumented_fixture"] = summary(holds)
        samples = []
        for _ in range(30):
            seconds, result = measured(
                lambda: execute_sql(
                    database, "SELECT count(*) AS count FROM catalog_files"
                )
            )
            if result.get("errors"):
                raise RuntimeError("metrics query failed")
            samples.append(seconds)
        metrics["query"] = summary(samples)

        def cancel_once():
            event = threading.Event()
            requested = []

            def cancel():
                requested.append(time.perf_counter())
                event.set()

            timer = threading.Timer(0.1, cancel)
            try:
                command_output(
                    [sys.executable, "-c", "import time; time.sleep(30)"],
                    cancel=event,
                    on_start=timer.start,
                    timeout=5,
                )
                raise RuntimeError("subprocess did not acknowledge cancellation")
            except CommandFailure as error:
                if error.state != "cancelled" or not requested:
                    raise
                return time.perf_counter() - requested[0]
            finally:
                timer.cancel()

        metrics["subprocess_cancel_to_group_reaped"] = summary(
            [cancel_once() for _ in range(5)]
        )
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        with (root / "server.log").open("w") as log:
            server = subprocess.Popen(
                [
                    sys.executable,
                    "-m",
                    "catabolic",
                    "--db",
                    str(database),
                    "api",
                    "serve",
                    "--port",
                    str(port),
                ],
                cwd=source,
                env=env,
                stdout=log,
                stderr=subprocess.STDOUT,
            )
            try:
                request = urllib.request.Request(
                    f"http://127.0.0.1:{port}/v1/me",
                    headers={"Authorization": "Bearer " + credential},
                )

                def fetch():
                    with urllib.request.urlopen(request, timeout=5) as response:
                        if response.status != 200:
                            raise RuntimeError("metrics HTTP request failed")
                        return response.read()

                deadline = time.monotonic() + 30
                while True:
                    try:
                        fetch()
                        break
                    except OSError:
                        if server.poll() is not None or time.monotonic() >= deadline:
                            raise
                        time.sleep(0.1)
                metrics["idle_http_server_rss_kib"] = int(
                    subprocess.check_output(
                        ["ps", "-o", "rss=", "-p", str(server.pid)], text=True
                    ).strip()
                )
                metrics["loopback_http_me"] = summary(
                    [measured(fetch)[0] for _ in range(30)]
                )
            finally:
                server.terminate()
                try:
                    server.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()
        metrics["retained_fixture_bytes"] = sum(
            path.stat().st_size for path in root.rglob("*") if path.is_file()
        )
        usage = resource.getrusage(resource.RUSAGE_SELF)
        metrics["harness_peak_rss_bytes"] = usage.ru_maxrss * (
            1 if sys.platform == "darwin" else 1024
        )
        metrics["harness_block_io_counts"] = dict(
            input_blocks=usage.ru_inblock,
            output_blocks=usage.ru_oublock,
            note="OS block-operation counters, not application bytes read",
        )
        report["status"] = "passed"
    except Exception as error:
        report.update(status="failed", error=f"{type(error).__name__}: {error}")
    report["unmeasured"] = [
        "cold filesystem cache",
        "application bytes read",
        "temporary disk and descriptor high-water marks",
        "active-worker cancellation latency",
        "observer overhead isolated comparison",
    ]
    (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(report["status"], flush=True)
    return int(report["status"] != "passed")


if __name__ == "__main__":
    raise SystemExit(main())
