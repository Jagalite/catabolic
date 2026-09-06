# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Retained high-cardinality fixtures; each measurement runs in a fresh process.

Fixture seeding uses SQL batches, deliberately bypassing per-item CLI import cost.
No production indexes, query limits, journaling settings, or runtime code change.
"""

import argparse
import hashlib
import json
import os
import platform
import resource
import shutil
import statistics
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone
from pathlib import Path
from uuid import uuid4

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.graphql_query import execute_graphql
from catabolic.layouts import PRESETS, Layouts
from catabolic.manifest import Manifest
from catabolic.migration import load_migrations, validate_database
from catabolic.reconcile import Reconciler
from catabolic.sql_query import execute_sql
from catabolic.store import Store, encode

MARKER = ".catabolic-scale-fixture.json"
PAYLOAD = b"CATABOLIC SYNTHETIC SCALE MEDIA\n" * 32
DATABASE_ACTIONS = (
    "indexed_association",
    "metadata_sql",
    "title_search",
    "graphql_nested",
    "graphql_filtered",
    "layout_select_one",
    "layout_full",
    "manifest",
    "manual_mapping",
)


def emit(value):
    print(json.dumps(value), flush=True)


def rss_mib():
    return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / (
        1024**2 if sys.platform == "darwin" else 1024
    )


def source_path(index):
    return f"group-{index // 1000:05d}/file-{index:09d}.mkv"


def file_id(index):
    return f"file-{index:09d}"


def item_id(index):
    return f"item-{index:09d}"


def source_fingerprint(root):
    digest = hashlib.sha256()
    for path in sorted(root.glob("*/*.mkv")):
        st = path.stat()
        digest.update(
            encode(
                [
                    str(path.relative_to(root)),
                    st.st_ino,
                    st.st_mtime_ns,
                    st.st_size,
                    hashlib.sha256(path.read_bytes()).hexdigest(),
                ]
            ).encode()
        )
    return digest.hexdigest()


def link_fingerprint(root, paths):
    digest = hashlib.sha256()
    for path in paths:
        link = root / path
        st = link.lstat()
        digest.update(
            encode([path, st.st_ino, st.st_mtime_ns, os.readlink(link)]).encode()
        )
    return digest.hexdigest()


def seed(root, count, physical):
    root.mkdir()
    path = root / "catalog.sqlite3"
    for directory in ("source", "output"):
        (root / directory).mkdir()
    Store.initialize(path)
    if physical:
        for index in range(count):
            target = root / "source" / source_path(index)
            target.parent.mkdir(exist_ok=True)
            target.write_bytes(PAYLOAD)
    with Store(path, writable=True) as store:
        app = Application(store)
        app.bind("source", "media", str(root / "source"))
        app.bind("output", "global", str(root / "output"))
        with store.transaction() as db:
            db.execute("INSERT INTO catalogs VALUES ('projection')")
        scan_seconds = None
        if physical:
            start = time.perf_counter()
            assert app.scan()["complete"]
            scan_seconds = time.perf_counter() - start
        else:
            with store.transaction() as db:
                db.execute(
                    "INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('synthetic','default','media',1,?,'[]')",
                    (count,),
                )
        start = time.perf_counter()
        for offset in range(0, count, 5000):
            stop = min(offset + 5000, count)
            if shutil.disk_usage(root).free < 5 * 1024**3:
                raise RuntimeError(
                    "fixture disk reserve reached; stopped before exhausting storage"
                )
            with store.transaction() as db:
                if not physical:
                    db.executemany(
                        "INSERT INTO files VALUES (?,?,?)",
                        (
                            (file_id(i), "media", source_path(i))
                            for i in range(offset, stop)
                        ),
                    )
                    db.executemany(
                        "INSERT INTO observations VALUES (?,?,?,?,?,?,?,?)",
                        (
                            (
                                "default",
                                file_id(i),
                                len(PAYLOAD),
                                1700000000000000000,
                                0,
                                i + 1,
                                "present",
                                "synthetic",
                            )
                            for i in range(offset, stop)
                        ),
                    )
                groups = range(offset // 100, (stop + 99) // 100)
                db.executemany(
                    "INSERT INTO items VALUES (?,?,?)",
                    (
                        (
                            f"group-{i:09d}",
                            "collection",
                            encode({"title": f"Collection {i}"}),
                        )
                        for i in groups
                    ),
                )
                db.executemany(
                    "INSERT INTO items VALUES (?,?,?)",
                    (
                        (
                            item_id(i),
                            ("movie", "book_edition", "track", "photo", "document")[
                                i % 5
                            ],
                            encode(
                                {
                                    "title": f"Benchmark {i:09d}",
                                    "year": 1980 + i % 46,
                                    "language": "en",
                                    "tags": ["synthetic", "scale"],
                                    "description": "Deterministic metadata fixture; no real media.",
                                }
                            ),
                        )
                        for i in range(offset, stop)
                    ),
                )
                db.executemany(
                    "INSERT INTO identities VALUES (?,?,?)",
                    (("synthetic", str(i), item_id(i)) for i in range(offset, stop)),
                )
                if physical:
                    actual = {
                        row["path"]: row["id"]
                        for row in store.rows(
                            "SELECT id,path FROM files WHERE path>=? AND path<=?",
                            (source_path(offset), source_path(stop - 1)),
                        )
                    }
                else:
                    actual = {source_path(i): file_id(i) for i in range(offset, stop)}
                db.executemany(
                    "INSERT INTO item_files(id,file_id,item_id,role,metadata,origin,active) VALUES (?,?,?,'primary',?,'explicit',1)",
                    (
                        (
                            f"association-{i:09d}",
                            actual[source_path(i)],
                            item_id(i),
                            '{"format":"synthetic"}',
                        )
                        for i in range(offset, stop)
                    ),
                )
                db.executemany(
                    "INSERT INTO item_relationships(id,source_id,target_id,kind,metadata,active) VALUES (?,?,?,'custom:grouped_with','{}',1)",
                    (
                        (f"relation-{i:09d}", item_id(i), f"group-{i // 100:09d}")
                        for i in range(offset, stop)
                    ),
                )
                db.executemany(
                    "INSERT INTO mappings VALUES (?,'global',?,?,?,1)",
                    (
                        (
                            app.mapping_id(
                                "global",
                                actual[source_path(i)],
                                item_id(i),
                                "Library/" + source_path(i),
                            ),
                            actual[source_path(i)],
                            item_id(i),
                            "Library/" + source_path(i),
                        )
                        for i in range(offset, stop)
                    ),
                )
        seed_seconds = time.perf_counter() - start
        layouts = Layouts(app)
        layouts.put("flat", PRESETS["flat"])
        layouts.put(
            "one",
            {
                **PRESETS["flat"],
                "selection": {
                    "language": "sql",
                    "query": "SELECT item_id FROM catalog_items WHERE item_id=:id",
                    "params": {"id": item_id(42)},
                },
            },
        )
        start = time.perf_counter()
        validate_database(store.db, load_migrations(), full=True)
        validation_seconds = time.perf_counter() - start
        marker = {
            "database_id": store.database_id,
            "count": count,
            "physical": physical,
            "source_fingerprint": source_fingerprint(root / "source")
            if physical
            else None,
        }
    (root / MARKER).write_text(encode(marker))
    return {
        "count": count,
        "physical": physical,
        "database_bytes": path.stat().st_size,
        "scan_seconds": scan_seconds,
        "bulk_fixture_seed_seconds": seed_seconds,
        "integrity_validation_seconds": validation_seconds,
    }


def operate(root, action):
    marker = json.loads((root / MARKER).read_text())
    path = root / "catalog.sqlite3"
    writable = action in (
        "manual_mapping",
        "sync",
        "repeat_sync",
        "change_one_percent",
        "crash",
        "recover",
        "rescan",
    )
    with Store(path, writable=writable, for_recovery=action == "recover") as store:
        assert store.database_id == marker["database_id"]
        app = Application(store)
        if action == "indexed_association":
            identifier = store.rows(
                "SELECT file_id FROM item_files WHERE id=?", ("association-000000042",)
            )[0]["file_id"]
            result = app.queries.associations(file=identifier)
            assert len(result["associations"]) == 1
            return {"rows": 1}
        if action == "metadata_sql":
            result = execute_sql(
                path, "SELECT count(*) FROM catalog_items WHERE year>=2020"
            )
            assert result["complete"]
            return {"matched": result["rows"][0][0]}
        if action == "title_search":
            result = app.queries.items(search="Benchmark 000000042", limit=100)
            assert len(result["items"]) == 1
            return {"rows": 1}
        if action.startswith("graphql"):
            query = (
                '{ items(kind: "movie", first: 100) { nodes { id associations(first: 2) { nodes { role file { id path status } } } } pageInfo { hasNextPage } } }'
                if action == "graphql_nested"
                else "{ items(year: 2024, first: 100) { nodes { id title } pageInfo { hasNextPage } } }"
            )
            result = execute_graphql(path, query)
            if result.get("errors"):
                raise CatabolicError(result["errors"][0]["message"])
            return {
                "rows": len(result["data"]["items"]["nodes"]),
                "records": result["extensions"]["records"],
            }
        if action.startswith("layout"):
            result = Layouts(app).run(
                "one" if action == "layout_select_one" else "flat",
                "projection",
                limit=1,
            )
            assert result["safe"]
            assert result["desired_count"] == (
                1 if action == "layout_select_one" else marker["count"]
            )
            return {
                key: result[key]
                for key in ("desired_count", "change_count", "details_truncated")
            }
        if action == "manifest":
            document = Manifest(app).build()
            assert document["content"]["counts"]["entries"] == marker["count"]
            return {
                "entries": document["content"]["counts"]["entries"],
                "compact_bytes": len(encode(document).encode()),
            }
        if action == "manual_mapping":
            identifier = store.rows(
                "SELECT file_id FROM item_files WHERE id=?", ("association-000000042",)
            )[0]["file_id"]
            mapping = app.put_mapping(
                "global", identifier, item_id(42), "Extra/one.mkv"
            )
            with store.transaction() as db:
                db.execute("DELETE FROM mappings WHERE id=?", (mapping["id"],))
            return {"inserted_and_removed_fixture_mapping": True}
        assert marker["physical"], (
            "filesystem operations require real synthetic sources"
        )
        reconciler = Reconciler(app)
        output = root / "output"
        if action == "rescan":
            result = app.scan()
            assert (
                result["complete"] and result["scans"][0]["observed"] == marker["count"]
            )
            return {"scanned": marker["count"]}
        if action == "sync":
            result = reconciler.apply()
            assert result["healthy"], result
            assert len(result["applied"]) == marker["count"] + 1
            return {
                "applied": len(result["applied"]),
                "verified_links": result["verification"]["catalogs"][0][
                    "verified_links"
                ],
            }
        if action == "repeat_sync":
            paths = [
                row["path"]
                for row in store.rows(
                    "SELECT path FROM mappings WHERE active=1 AND catalog='global' ORDER BY path"
                )
            ]
            before = link_fingerprint(output, paths)
            start = time.perf_counter()
            result = reconciler.apply()
            seconds = time.perf_counter() - start
            assert result["healthy"] and not result["applied"]
            assert before == link_fingerprint(output, paths)
            return {
                "sync_seconds": seconds,
                "unchanged_inodes": True,
                "links": len(paths),
            }
        if action == "change_one_percent":
            survivors = [
                row["path"]
                for row in store.rows(
                    "SELECT path FROM mappings WHERE active=1 AND catalog='global' AND substr(item_id,-2)!='00' ORDER BY path"
                )
            ]
            before = link_fingerprint(output, survivors)
            with store.transaction() as db:
                removed = db.execute(
                    "UPDATE mappings SET active=0 WHERE catalog='global' AND substr(item_id,-2)='00'"
                ).rowcount
            start = time.perf_counter()
            result = reconciler.apply()
            seconds = time.perf_counter() - start
            assert result["healthy"] and len(result["applied"]) == removed
            assert before == link_fingerprint(output, survivors)
            assert source_fingerprint(root / "source") == marker["source_fingerprint"]
            return {
                "sync_seconds": seconds,
                "removed": removed,
                "surviving_links": len(survivors),
                "unchanged_surviving_inodes": True,
                "sources_unchanged": True,
            }
        if action == "crash":
            identifier = store.rows(
                "SELECT file_id FROM item_files WHERE id=?", ("association-000000042",)
            )[0]["file_id"]
            app.put_mapping("global", identifier, item_id(42), "Recovery/extra.mkv")

            def crash(operation):
                if operation["kind"] == "create":
                    os._exit(86)

            reconciler.apply(after_filesystem=crash)
            raise AssertionError("crash boundary not reached")
        if action == "recover":
            assert store.rows("SELECT count(*) AS n FROM journal")[0]["n"] == 1
            result = reconciler.recover()
            assert len(result["recovered"]) == 1
            assert reconciler.verify()["healthy"]
            assert source_fingerprint(root / "source") == marker["source_fingerprint"]
            return {"recovered": 1, "healthy": True, "sources_unchanged": True}
        raise ValueError(action)


def worker(args):
    def memory_guard():
        while True:
            if rss_mib() > args.memory_mib:
                emit(
                    {
                        "status": "memory_limit",
                        "peak_rss_mib": round(rss_mib(), 2),
                        "limit_mib": args.memory_mib,
                    }
                )
                os._exit(88)
            time.sleep(0.2)

    threading.Thread(target=memory_guard, daemon=True).start()
    start = time.perf_counter()
    try:
        result = (
            seed(args.root, args.count, args.physical)
            if args.worker == "seed"
            else operate(args.root, args.worker)
        )
        record = {"status": "ok", "result": result}
    except CatabolicError as exc:
        record = {"status": "guarded", "error": str(exc)}
    except Exception as exc:
        record = {"status": "failed", "error": f"{type(exc).__name__}: {exc}"}
    record.update(
        seconds=round(time.perf_counter() - start, 4), peak_rss_mib=round(rss_mib(), 2)
    )
    emit(record)
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path)
    parser.add_argument(
        "--sizes", type=int, nargs="+", default=[10000, 100000, 1000000]
    )
    parser.add_argument(
        "--filesystem-sizes", type=int, nargs="*", default=[10000, 100000]
    )
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--memory-mib", type=int, default=1536)
    parser.add_argument("--worker")
    parser.add_argument("--count", type=int)
    parser.add_argument("--physical", action="store_true")
    args = parser.parse_args(argv)
    if args.worker:
        return worker(args)
    if (
        any(n < 100 or n % 100 for n in [*args.sizes, *args.filesystem_sizes])
        or args.repeats < 1
    ):
        parser.error(
            "sizes must be positive multiples of 100; repeats must be positive"
        )
    root = (
        args.root
        or Path(".local-tests")
        / f"high-scale-{datetime.now(timezone.utc):%Y%m%dT%H%M%SZ}-{uuid4().hex[:6]}"
    ).absolute()
    root.mkdir(parents=True, exist_ok=False)
    root = root.resolve()
    report = {
        "root": str(root),
        "python": platform.python_version(),
        "platform": platform.platform(),
        "sizes": args.sizes,
        "filesystem_sizes": args.filesystem_sizes,
        "memory_limit_mib": args.memory_mib,
        "results": [],
        "notes": [
            "Database-only rows describe synthetic absent media; no filesystem-health claims for those fixtures.",
            "Fixtures use batched SQL seeding, not the public ingestion workflow.",
            "Fresh process per operation; shared OS caches are not cleared. No NAS measurements.",
            "Timings include process-local database open and validation; process startup excluded from worker seconds.",
            "Filesystem repeat/change phases report sync_seconds separately from safety fingerprint checks.",
            "No runtime limits, indexes, or durability settings changed.",
        ],
    }

    def save():
        (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")

    def run(fixture, count, action, physical=False, timeout=90):
        command = [
            sys.executable,
            "-m",
            "tests.scale_benchmark",
            "--worker",
            action,
            "--root",
            str(fixture),
            "--count",
            str(count),
            "--memory-mib",
            str(args.memory_mib),
        ]
        if physical:
            command.append("--physical")
        emit({"event": "start", "count": count, "physical": physical, "action": action})
        started = time.perf_counter()
        child = subprocess.Popen(
            command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
        )
        try:
            out, err = child.communicate(timeout=timeout)
            try:
                result = json.loads(out.strip().splitlines()[-1])
            except (ValueError, IndexError):
                result = {
                    "status": "injected_crash"
                    if child.returncode == 86 and action == "crash"
                    else "failed",
                    "error": err[-2000:],
                }
        except subprocess.TimeoutExpired:
            child.kill()
            child.communicate()
            result = {"status": "timeout", "timeout_seconds": timeout}
        result.update(
            count=count,
            physical=physical,
            action=action,
            wall_seconds=round(time.perf_counter() - started, 4),
            fixture=str(fixture),
        )
        report["results"].append(result)
        save()
        emit(result)
        return result

    try:
        for physical, sizes in ((False, args.sizes), (True, args.filesystem_sizes)):
            for count in sizes:
                fixture = root / f"{'filesystem' if physical else 'database'}-{count}"
                if run(fixture, count, "seed", physical, timeout=600)["status"] != "ok":
                    continue
                if physical:
                    if run(fixture, count, "sync", True, timeout=600)["status"] != "ok":
                        continue
                    for action in (
                        "rescan",
                        "repeat_sync",
                        "change_one_percent",
                        "crash",
                        "recover",
                    ):
                        outcome = run(fixture, count, action, True, timeout=600)
                        if outcome["status"] not in ("ok", "injected_crash"):
                            break
                else:
                    for action in DATABASE_ACTIONS:
                        trials = []
                        for _ in range(
                            args.repeats if action in DATABASE_ACTIONS[:5] else 1
                        ):
                            trial = run(fixture, count, action)
                            trials.append(trial)
                            if trial["status"] != "ok":
                                break
                        if len(trials) > 1 and all(t["status"] == "ok" for t in trials):
                            emit(
                                {
                                    "event": "median",
                                    "count": count,
                                    "action": action,
                                    "median_seconds": statistics.median(
                                        t["seconds"] for t in trials
                                    ),
                                }
                            )
    finally:
        save()
    emit({"event": "complete", "report": str(root / "report.json")})
    return int(any(r["status"] == "failed" for r in report["results"]))


if __name__ == "__main__":
    raise SystemExit(main())
