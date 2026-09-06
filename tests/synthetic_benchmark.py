"""Repeatable filesystem integration scenarios with optional scale measurements."""

from __future__ import annotations

import argparse
import json
import os
import platform
import resource
import subprocess
import sys
import time
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path
from uuid import uuid4

from catabolic.app import Application
from catabolic.reconcile import Reconciler
from catabolic.store import Store
from tests.synthetic_library import EXCLUSIONS, MARKER, SyntheticLibrary


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def link_snapshot(output: Path, destinations: list[str]) -> dict:
    result = {}
    for destination in destinations:
        path = output / destination
        metadata = path.lstat()
        result[destination] = (metadata.st_ino, metadata.st_mtime_ns, os.readlink(path))
    return result


def crash_worker(database: Path):
    """Exit at the filesystem/SQLite boundary, only for a marked fake fixture."""
    marker = json.loads((database.parent / MARKER).read_text())
    require(database.name == "catalog.sqlite3", "unexpected fixture database name")
    with Store(database, writable=True) as store:
        require(marker["database_id"] == store.database_id, "fixture marker mismatch")

        def terminate_after_create(operation):
            if operation["kind"] == "create":
                os._exit(86)

        Reconciler(Application(store)).apply(after_filesystem=terminate_after_create)
    raise RuntimeError("crash worker had no create operation")


def run_fixture(root: Path, count: int, progress=None) -> dict:
    report = {
        "media_files": count,
        "root": str(root),
        "timings_seconds": {},
        "checks": {},
    }

    def emit(event, **details):
        if progress:
            progress(event, media_files=count, **details)

    def measured(name, callback):
        start = time.perf_counter()
        value = callback()
        elapsed = time.perf_counter() - start
        report["timings_seconds"][name] = round(elapsed, 6)
        emit(name, seconds=round(elapsed, 3))
        return value

    library = measured("generate", lambda: SyntheticLibrary.create(root, count))
    original_hashes = library.hashes()
    destinations = [entry.destination for entry in library.entries]
    with Store(library.database, writable=True) as store:
        app = Application(store)
        reconciler = Reconciler(app)
        scan = measured("scan", lambda: app.scan(exclude=EXCLUSIONS))
        require(scan["complete"], "initial scan must complete")
        files = library.inventory(app)
        require(
            len(files) == count + 4,
            "inventory must contain media and four invalid sources only",
        )
        require(
            all(
                "external-link" not in path
                and "excluded" not in path
                and "must-not-be-scanned" not in path
                for _, path in files
            ),
            "scanner followed a symlink or entered an exclusion",
        )
        report["inventory_files"] = len(files)
        report["checks"]["pagination_and_scan_scope"] = True
        mappings = measured(
            "record_decisions",
            lambda: library.seed_decisions(
                app,
                files,
                lambda completed: emit("decision_progress", completed=completed),
            ),
        )

        db_before = library.database.read_bytes()
        preview = measured("preview", reconciler.preview)
        require(preview["safe"], "preview blocked")
        require(
            Counter(row["kind"] for row in preview["actions"])
            == {"prepare": 1, "create": count},
            "unexpected initial delta",
        )
        require(
            db_before == library.database.read_bytes()
            and not list(library.output.iterdir()),
            "preview mutated state",
        )
        report["checks"]["read_only_preview"] = True

        result = measured("apply", reconciler.apply)
        require(result["healthy"], "initial application unhealthy")
        verify = measured("verify", reconciler.verify)
        require(
            verify["healthy"] and verify["catalogs"][0]["verified_links"] == count,
            "verification count mismatch",
        )
        for entry in library.entries:
            link = library.output / entry.destination
            require(
                link.is_symlink() and not os.path.isabs(os.readlink(link)),
                "expected a relative symlink",
            )
            require(
                link.resolve(strict=True) == library.source(entry),
                "link points to wrong occurrence",
            )
        report["checks"]["all_relative_links_resolve"] = True
        links_before = link_snapshot(library.output, destinations)
        repeat = measured("repeat_sync", reconciler.apply)
        require(
            repeat["healthy"] and not repeat["applied"],
            "repeat sync changed desired state",
        )
        require(
            links_before == link_snapshot(library.output, destinations),
            "repeat sync rewrote links",
        )
        report["checks"]["repeat_preserves_inodes_and_mtimes"] = True

        first = library.entries[0]
        source = library.source(first)
        link = library.output / first.destination

        def missing_and_restored():
            payload = source.read_bytes()
            source.unlink()
            blocked = reconciler.apply()
            require(
                not blocked["safe"] and link.is_symlink(),
                "unconfirmed absence must preserve the link",
            )
            require(
                app.scan(exclude=EXCLUSIONS)["complete"], "missing-source scan failed"
            )
            removed = reconciler.apply()
            require(
                removed["safe"] and not removed["healthy"],
                "confirmed absence should reconcile but remain unhealthy",
            )
            require(
                [row["kind"] for row in removed["applied"]] == ["remove"],
                "missing source affected unrelated links",
            )
            require(not link.is_symlink(), "confirmed missing link remains")
            source.write_bytes(payload)
            require(
                not reconciler.preview()["safe"], "returned source must require rescan"
            )
            require(app.scan(exclude=EXCLUSIONS)["complete"], "restoration scan failed")
            restored = reconciler.apply()
            require(
                restored["healthy"]
                and [row["kind"] for row in restored["applied"]] == ["create"],
                "restoration must create exactly one link",
            )
            unaffected = destinations[1:]
            require(
                {key: links_before[key] for key in unaffected}
                == link_snapshot(library.output, unaffected),
                "source restoration rewrote unrelated links",
            )

        measured("missing_and_restored", missing_and_restored)
        report["checks"]["missing_and_restored"] = True

        def unavailable_root():
            source_root = library.sources[first.location]
            detached = library.root / "detached-source"
            source_root.rename(detached)
            source_root.mkdir()
            before = link_snapshot(library.output, destinations)
            try:
                require(
                    not app.scan(first.location, exclude=EXCLUSIONS)["complete"],
                    "replaced source root was accepted",
                )
                require(
                    not reconciler.apply()["safe"], "replaced root did not block sync"
                )
                require(
                    before == link_snapshot(library.output, destinations),
                    "unavailable source changed catalog",
                )
            finally:
                source_root.rmdir()
                detached.rename(source_root)
            require(
                reconciler.verify()["healthy"],
                "original binding did not recover after root restoration",
            )

        measured("unavailable_root", unavailable_root)
        report["checks"]["unavailable_root_preserves_catalog"] = True

        def collision():
            target = os.readlink(link)
            link.unlink()
            sentinel = b"EXTERNAL FILE: MUST SURVIVE SYNC"
            link.write_bytes(sentinel)
            try:
                result = reconciler.apply()
                require(
                    not result["safe"] and link.read_bytes() == sentinel,
                    "sync overwrote an external file",
                )
            finally:
                link.unlink()
                link.symlink_to(target)
            require(reconciler.verify()["healthy"], "collision restoration failed")

        measured("collision", collision)
        report["checks"]["external_collision_preserved"] = True

        def invalid_sources():
            identity = app.put_item("other", {"synthetic": "invalid"}, {})["id"]
            pending = []
            for index, filename in enumerate(("empty.mkv", "download.mkv.PART")):
                row = files["drive-1", filename]
                pending.append(
                    app.put_mapping(
                        "global", row["id"], identity, f"Invalid/blocked-{index}.mkv"
                    )
                )
            result = reconciler.apply()
            require(
                result["safe"] and not result["healthy"],
                "invalid sources should be blocked individually",
            )
            require(not result["applied"], "invalid source created or removed a link")
            for mapping in pending:
                require(
                    not (library.output / mapping["path"]).is_symlink(),
                    "invalid source was projected",
                )
                app.disable_mapping(mapping["id"])
            require(
                reconciler.verify()["healthy"],
                "invalid-source cleanup left catalog unhealthy",
            )

        measured("invalid_sources", invalid_sources)
        report["checks"]["empty_and_partial_sources_blocked"] = True
        # Add one new output to ensure the crash leaves a created link whose
        # ownership transaction has not committed, then close this writer.
        recovery_mapping = app.put_mapping(
            "global",
            files[first.location, first.relative]["id"],
            mappings[first.destination]["item_id"],
            "Recovery/interrupted.mkv",
        )

    def interrupted_process():
        child = subprocess.run(
            [
                sys.executable,
                "-m",
                "tests.synthetic_benchmark",
                "--crash-worker",
                str(library.database),
            ],
            capture_output=True,
            text=True,
            timeout=60,
        )
        require(child.returncode == 86, f"crash worker failed: {child.stderr}")
        recovery_link = library.output / recovery_mapping["path"]
        require(
            recovery_link.is_symlink(), "worker did not reach the filesystem boundary"
        )
        with Store(library.database, writable=True) as store:
            app = Application(store)
            reconciler = Reconciler(app)
            require(
                len(app.status()["pending_operations"]) == 1,
                "crash did not leave exactly one pending operation",
            )
            require(
                not store.rows(
                    "SELECT path FROM owned_links WHERE path=?",
                    (recovery_mapping["path"],),
                ),
                "ownership unexpectedly committed before crash",
            )
            require(
                not reconciler.preview()["safe"],
                "pending recovery did not block a new sync",
            )
            require(
                len(reconciler.recover()["recovered"]) == 1, "journal recovery failed"
            )
            require(reconciler.verify()["healthy"], "recovered catalog unhealthy")
            app.disable_mapping(recovery_mapping["id"])
            require(reconciler.apply()["healthy"], "recovery test link removal failed")
            require(not app.status()["pending_operations"], "journal was not cleared")

    measured("process_crash_and_recovery", interrupted_process)
    report["checks"]["real_process_exit_and_recovery"] = True
    require(
        library.hashes() == original_hashes, "fake source contents changed unexpectedly"
    )
    report["checks"]["source_content_hashes_match"] = True
    with Store(library.database) as store:
        verified = Reconciler(Application(store)).verify()
    require(
        verified["healthy"] and verified["catalogs"][0]["verified_links"] == count,
        "final catalog count incorrect",
    )
    report["final_healthy_links"] = count
    report["passed"] = True
    report["rates_per_second"] = {
        "scan_inventory_files": round(
            report["inventory_files"] / report["timings_seconds"]["scan"], 1
        ),
        "create_links_including_verification": round(
            count / report["timings_seconds"]["apply"], 1
        ),
    }
    (library.root / "report.json").write_text(json.dumps(report, indent=2))
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="Create fresh fake libraries and measure Catabolic's behavior. Fixtures are retained for inspection."
    )
    parser.add_argument(
        "--sizes",
        nargs="+",
        type=int,
        default=[1000],
        help="mapped-file counts; default: 1000",
    )
    parser.add_argument(
        "--root", type=Path, help="new artifact directory; existing paths are refused"
    )
    parser.add_argument("--crash-worker", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    if args.crash_worker:
        crash_worker(args.crash_worker)
        return 1
    if any(size < 10 for size in args.sizes) or len(set(args.sizes)) != len(args.sizes):
        parser.error("sizes must be distinct integers of at least 10")
    root = (
        args.root
        or Path(".local-tests")
        / f"synthetic-{datetime.now(timezone.utc):%Y%m%dT%H%M%SZ}-{uuid4().hex[:6]}"
    )
    root = root.absolute()
    root.mkdir(parents=True, exist_ok=False)
    report = {
        "root": str(root),
        "python": platform.python_version(),
        "platform": platform.platform(),
        "payload_bytes_per_media_file": 1024,
        "results": [],
        "notes": "Local filesystem metadata benchmark with tiny fake files. Not a media decoding, hashing-throughput, or NAS benchmark. No wall-clock threshold gates the unit tests.",
    }

    def progress(event, **details):
        print(json.dumps({"event": event, **details}), flush=True)

    try:
        for size in args.sizes:
            report["results"].append(
                run_fixture(root / f"files-{size}", size, progress)
            )
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
        report["passed"] = False
    else:
        report["passed"] = True
    memory = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    report["parent_process_peak_rss_mib"] = round(
        memory / (1024**2 if sys.platform == "darwin" else 1024), 2
    )
    (root / "report.json").write_text(json.dumps(report, indent=2))
    progress("complete", passed=report["passed"], report=str(root / "report.json"))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
