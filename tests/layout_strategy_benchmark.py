"""Read-only experiment: change graph loading, retain the actual layout planner.

The batch loader intentionally supports only this fixture's one-hop outgoing
collection selector. This is an experiment, not a general production planner.
"""

import argparse
import hashlib
import inspect
import json
import os
import statistics
import subprocess
import sys
import textwrap
import threading
import time
from pathlib import Path

import catabolic.layouts as runtime
from catabolic.app import Application
from catabolic.layouts import Layouts, validate_layout
from catabolic.store import Store, encode
from tests.scale_benchmark import MARKER, rss_mib


class BatchGraph:
    def __init__(self, store, associations, batch):
        self.store = store
        self.ids = [a["item_id"] for a in associations]
        assert len(set(self.ids)) == len(self.ids), "fixture requires one file per item"
        self.positions = {key: i for i, key in enumerate(self.ids)}
        self.batch = batch
        self.items, self.edges = {}, {}
        self.sql_calls = 0

    @staticmethod
    def item(key, kind, raw):
        metadata = json.loads(raw)
        return {
            "id": key,
            "kind": kind,
            "title": metadata.get("title"),
            "year": metadata.get("year"),
            "metadata": metadata,
        }

    def __getitem__(self, key):
        if key not in self.items:
            start = self.positions[key] // self.batch * self.batch
            ids = self.ids[start : start + self.batch]
            marks = ",".join("?" for _ in ids)
            self.items, self.edges = {}, {}
            for row in self.store.db.execute(
                f"SELECT id,kind,metadata FROM items WHERE id IN ({marks})", ids
            ):
                self.items[row["id"]] = self.item(
                    row["id"], row["kind"], row["metadata"]
                )
            for row in self.store.db.execute(
                "SELECT r.*,i.kind AS target_kind,i.metadata AS target_metadata "
                "FROM item_relationships r JOIN items i ON i.id=r.target_id "
                f"WHERE r.active=1 AND r.kind='custom:grouped_with' AND r.source_id IN ({marks})",
                ids,
            ):
                edge = {
                    k: row[k]
                    for k in (
                        "id",
                        "source_id",
                        "target_id",
                        "kind",
                        "position",
                        "metadata",
                        "active",
                    )
                }
                self.items[row["target_id"]] = self.item(
                    row["target_id"], row["target_kind"], row["target_metadata"]
                )
                self.edges.setdefault(
                    (row["source_id"], "outgoing", row["kind"]), []
                ).append((row["target_id"], edge))
            self.sql_calls += 2
        return self.items[key]

    def get(self, key, default=None):
        return self.edges.get(key, default)


def definition(selected, collision=False):
    value = {
        "version": 1,
        "rules": [
            {
                "name": "grouped",
                "relations": [{"alias": "group", "kind": "custom:grouped_with"}],
                "path": "{group.title}/same.mkv"
                if collision
                else "{group.title}/{item.title}{file.extension}",
            }
        ],
    }
    if selected:
        value["selection"] = {
            "language": "sql",
            "query": "SELECT item_id FROM catalog_items WHERE item_id>=:first AND item_id<:last ORDER BY item_id",
            "params": {
                "first": "item-000000042" if selected == 1 else "item-000000000",
                "last": "item-000000043" if selected == 1 else f"item-{selected:09d}",
            },
        }
    return validate_layout(value)


def baseline_method(path):
    if path is None:
        return Layouts._plan
    namespace = dict(vars(runtime))
    exec(compile(path.read_text(), str(path.resolve()), "exec"), namespace)
    return namespace["Layouts"]._plan


def prototype_method(batch, graphs, method):
    original = textwrap.dedent(inspect.getsource(method))
    start = original.index("    items = {}\n")
    selection = original.index("    selection_report = None\n", start)
    loop = original.index("    for association in associations:\n", selection)
    # Move the real selection and cap checks before graph loading. All rendering,
    # ownership handling, collision checks and desired/change generation stay exact.
    modified = (
        original[:start]
        + original[selection:loop]
        + "    items = edges = load_graph(self.store, associations)\n"
        + original[loop:]
    )

    def load_graph(store, associations):
        graph = BatchGraph(store, associations, batch)
        graphs.append(graph)
        return graph

    namespace = dict(vars(runtime), load_graph=load_graph)
    exec(compile(modified, "<layout-loading-experiment>", "exec"), namespace)
    return namespace["_plan"]


def worker(args):
    def guard():
        while True:
            if rss_mib() > 1536:
                print(
                    json.dumps({"status": "memory_limit", "peak_rss_mib": rss_mib()}),
                    flush=True,
                )
                os._exit(88)
            time.sleep(0.2)

    threading.Thread(target=guard, daemon=True).start()
    marker = json.loads((args.fixture / MARKER).read_text())
    assert not marker["physical"]
    graphs = []

    class Experiment(Layouts):
        def get(self, identifier):
            return {
                "name": identifier,
                "definition": definition(args.selected, args.collision),
            }

    method = baseline_method(args.baseline_file)
    Experiment._plan = method
    if args.batch:
        Experiment._plan = prototype_method(args.batch, graphs, method)
    start = time.perf_counter()
    with Store(args.fixture / "catalog.sqlite3") as store:
        assert store.database_id == marker["database_id"]
        result = Experiment(Application(store))._plan("experiment", "projection", False)
        seconds = time.perf_counter() - start
        peak = rss_mib()
        plan, desired, removals, owned = result
        assert plan["desired_count"] == (args.selected or marker["count"])
        assert plan["safe"] is not args.collision
        assert not removals and not owned
        # Fingerprint the complete plan and mappings, not just displayed samples.
        # Exclude serialization time/memory from the measured planner interval.
        digest = hashlib.sha256(
            encode([plan, desired, removals, sorted(owned)]).encode()
        ).hexdigest()
    print(
        json.dumps(
            {
                "status": "ok",
                "seconds": seconds,
                "peak_rss_mib": peak,
                "digest": digest,
                "desired": len(desired),
                "blockers": len(plan["blockers"]),
                "graph_sql_calls": graphs[0].sql_calls if graphs else None,
            }
        ),
        flush=True,
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument(
        "--fixtures", type=Path, default=Path(".local-tests/high-scale-20260906")
    )
    parser.add_argument("--worker", action="store_true")
    parser.add_argument("--fixture", type=Path)
    parser.add_argument("--selected", type=int, default=0)
    parser.add_argument("--batch", type=int, default=0)
    parser.add_argument("--collision", action="store_true")
    parser.add_argument(
        "--baseline-file",
        type=Path,
        help="Saved pre-optimization layouts.py for reproducing the prototype comparison",
    )
    parser.add_argument(
        "--production",
        action="store_true",
        help="Measure the current planner against a retained report",
    )
    parser.add_argument(
        "--reference", type=Path, help="Prior report.json with complete-plan hashes"
    )
    args = parser.parse_args()
    if args.production and (args.baseline_file or not args.reference):
        parser.error("--production requires --reference and cannot use --baseline-file")
    if not args.worker and not args.production and not args.baseline_file:
        parser.error(
            "provide --baseline-file for the historical experiment, or --production --reference for the current planner"
        )
    if args.worker:
        worker(args)
        return
    args.root.mkdir(parents=True, exist_ok=False)
    reference = (
        json.loads(args.reference.read_text())["results"] if args.reference else []
    )
    report = {
        "mode": "production" if args.production else "prototype",
        "reference": str(args.reference) if args.reference else None,
        "baseline_file": str(args.baseline_file) if args.baseline_file else None,
        "results": [],
        "planner_sha256": hashlib.sha256(
            inspect.getsource(baseline_method(args.baseline_file)).encode()
        ).hexdigest(),
        "notes": [
            "Read-only retained fixtures; no runtime changes or added indexes.",
            "Production/candidates three times; historical baseline and collision cases once.",
            "Fresh process per sample; shared OS caches, no cache flushing.",
            "Seconds include Store open/validation and full plan generation; exclude signature serialization and interpreter startup.",
            "Only graph loading is batched; selection, desired mappings, and conflict checking retain existing in-memory behavior.",
            "Workloads exercise one-hop outgoing custom:grouped_with; production behavior also requires the application test suite.",
            "Projection catalog is initially empty; populated-catalog reconciliation not measured.",
        ],
    }
    for size, selected, collision in [
        (1000000, 1, False),
        (1000000, 10000, False),
        (100000, 0, False),
        (10000, 1000, True),
    ]:
        expected = None
        if args.production:
            hashes = {
                row["digest"]
                for row in reference
                if row["size"] == size
                and row["selected"] == (selected or size)
                and row["collision"] == collision
                and row["status"] == "ok"
            }
            if len(hashes) != 1:
                raise ValueError(
                    "reference must contain one consistent plan hash per workload"
                )
            expected = hashes.pop()
        for batch in [0] if args.production else [0, 1, 100, 1000, 10000]:
            trials = 1 if collision or (batch == 0 and not args.production) else 3
            times = []
            for trial in range(trials):
                print(
                    json.dumps({"start": [size, selected, batch, trial, collision]}),
                    flush=True,
                )
                command = [
                    sys.executable,
                    "-m",
                    "tests.layout_strategy_benchmark",
                    "--worker",
                    "--root",
                    str(args.root),
                    "--fixture",
                    str(args.fixtures / f"database-{size}"),
                    "--selected",
                    str(selected),
                    "--batch",
                    str(batch),
                ]
                if args.baseline_file:
                    command.extend(["--baseline-file", str(args.baseline_file)])
                if collision:
                    command.append("--collision")
                try:
                    child = subprocess.run(
                        command, capture_output=True, text=True, timeout=120
                    )
                    value = json.loads(child.stdout.strip().splitlines()[-1])
                    if child.returncode and value.get("status") != "memory_limit":
                        raise RuntimeError(child.stderr[-2000:])
                except Exception as exc:
                    value = {"status": "failed", "error": str(exc)}
                    if "child" in locals():
                        value["stderr"] = child.stderr[-2000:]
                value.update(
                    size=size,
                    selected=selected or size,
                    batch=batch,
                    trial=trial,
                    collision=collision,
                )
                if value["status"] == "ok":
                    expected = expected or value["digest"]
                    value["matches_baseline"] = value["digest"] == expected
                    if not value["matches_baseline"]:
                        value["status"] = "failed"
                    times.append(value["seconds"])
                report["results"].append(value)
                (args.root / "report.json").write_text(
                    json.dumps(report, indent=2) + "\n"
                )
                print(json.dumps(value), flush=True)
                if value["status"] != "ok":
                    raise SystemExit(1)
            print(
                json.dumps(
                    {
                        "median": statistics.median(times),
                        "size": size,
                        "selected": selected,
                        "batch": batch,
                    }
                ),
                flush=True,
            )


if __name__ == "__main__":
    main()
