"""Bounded real-file scan/probe measurements; never accepts an existing fixture root."""

import argparse
import json
import subprocess
import time
import tracemalloc
import wave
from pathlib import Path

from catabolic.app import Application
from catabolic.processing import Processing
from catabolic.store import Store


def worker(root, size, baseline):
    source = root / f"files-{size}"
    source.mkdir()
    for index in range(size):
        (source / f"{index:08d}.txt").write_text("synthetic benchmark\n")
    database = root / f"catalog-{size}.sqlite3"
    Store.initialize(database)
    with Store(database, writable=True) as store:
        app = Application(store)
        app.bind("source", "fixture", str(source))
        app.scan()  # Warm filesystem and initialized catalog; comparison is unchanged rescans.
        metrics = {}
        for label in ("baseline", "staged"):
            target = baseline(store) if label == "baseline" else app
            tracemalloc.start()
            started = time.perf_counter()
            result = target.scan()
            elapsed = time.perf_counter() - started
            _, peak = tracemalloc.get_traced_memory()
            tracemalloc.stop()
            assert result["complete"] and result["scans"][0]["observed"] == size
            metrics[label] = {"seconds": elapsed, "python_peak_mib": peak / 1024 / 1024}
        return metrics


def media(root, count):
    source = root / "audio"
    source.mkdir()
    database = root / "audio.sqlite3"
    for index in range(count):
        with wave.open(str(source / f"{index}.wav"), "wb") as audio:
            audio.setnchannels(1)
            audio.setsampwidth(2)
            audio.setframerate(8000)
            audio.writeframes(b"\0\0" * 800)
    Store.initialize(database)
    with Store(database, writable=True) as store:
        app = Application(store)
        app.bind("source", "audio", str(source))
        app.scan()
        processing = Processing(app)
        started = time.perf_counter()
        processing.enqueue("probe", limit=count)
        enqueue = time.perf_counter() - started
        started = time.perf_counter()
        cold = processing.run(workers=2, per_device=2, limit=count)
        first = time.perf_counter() - started
        started = time.perf_counter()
        warm = processing.enqueue("probe", limit=count)
        repeat = processing.run(limit=count)
        unchanged = time.perf_counter() - started
        assert (
            cold["complete"]
            and repeat["processed"] == 0
            and len(warm["cached"]) == count
        )
        return {
            "files": count,
            "enqueue_seconds": enqueue,
            "first_run_seconds": first,
            "unchanged_enqueue_and_run_seconds": unchanged,
            "first_jobs": cold["processed"],
            "unchanged_jobs": repeat["processed"],
            "cached": len(warm["cached"]),
        }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True)
    parser.add_argument("--sizes", nargs="+", type=int, default=[1000, 10000])
    parser.add_argument("--probe-files", type=int, default=50)
    args = parser.parse_args()
    root = Path(args.root).resolve()
    root.mkdir()
    code = subprocess.check_output(
        ["git", "show", "HEAD:src/catabolic/filesystem.py"], text=True
    )
    namespace = {"__package__": "catabolic"}
    exec(compile(code, "baseline-filesystem.py", "exec"), namespace)
    application_code = subprocess.check_output(
        ["git", "show", "HEAD:src/catabolic/app.py"], text=True
    )
    application_namespace = {"__package__": "catabolic"}
    exec(compile(application_code, "baseline-app.py", "exec"), application_namespace)
    application_namespace["walk_files"] = namespace["walk_files"]
    baseline = application_namespace["Application"]
    result = {
        "baseline_commit": subprocess.check_output(
            ["git", "rev-parse", "HEAD"], text=True
        ).strip(),
        "scans": {},
        "scope": "local APFS warm rescans; tracemalloc Python allocations, not total process RSS; one exploratory sample per case",
    }
    for size in args.sizes:
        result["scans"][str(size)] = worker(root, size, baseline)
    result["probe"] = media(root, args.probe_files)
    (root / "report.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
