# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Run the messy-collection reference journey in a NEW disposable directory.

This is a scripted reference, never an agent judgment score. No live media or
consumer servers are used. --python must have Catabolic installed.
"""

import argparse
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

try:
    from .experience_evaluator import digest, evaluate, snapshot
except ImportError:
    from experience_evaluator import digest, evaluate, snapshot

CORPUS = Path(__file__).resolve().parents[1] / "tests/fixtures/experience/cases.json"

# Test-only child instrumentation; no fault switches in the shipped CLI.
CRASH = """
import os, sys
from catabolic.cli import main
if sys.argv[1] == 'journal':
    from catabolic.reconcile import Reconciler
    original = Reconciler._execute
    def execute(self, operation, **kwargs):
        def stop(operation):
            os._exit(77)
        return original(self, operation, after_filesystem=stop)
    Reconciler._execute = execute
else:
    import catabolic.catalog_refresh
    def finish(*args, **kwargs):
        os._exit(77)
    catabolic.catalog_refresh.finish = finish
raise SystemExit(main(sys.argv[2:]))
"""


def write(path, data):
    path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n")


class Journey:
    def __init__(self, root, python):
        self.root = root
        self.python = python
        self.key = json.loads(CORPUS.read_text())
        self.checkpoints = {}
        self.review = {"run_type": "reference", "cases": {}, "interventions": []}
        self.env = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith("CATABOLIC_") and k != "PYTHONPATH"
        }
        self.commands = 0

    def cli(self, *args, expected=0, crash=None):
        command = [self.python]
        command += ["-c", CRASH, crash] if crash else ["-m", "catabolic"]
        command += ["--db", str(self.root / "catalog.sqlite3"), "--json", *args]
        result = subprocess.run(
            command,
            cwd=self.root,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.commands += 1
        with (self.root / "transcript.jsonl").open("a") as stream:
            stream.write(
                json.dumps(
                    {
                        "argv": command,
                        "exit": result.returncode,
                        "stdout": result.stdout,
                        "stderr": result.stderr,
                    }
                )
                + "\n"
            )
        if result.returncode != expected:
            raise AssertionError(
                f"{args}: {result.returncode}: {result.stdout[-1500:]} {result.stderr[-1500:]}"
            )
        return (
            json.loads(result.stdout or result.stderr)
            if not crash
            else result.returncode
        )

    def encode(self, path, small=False):
        subprocess.run(
            [
                "ffmpeg",
                "-v",
                "error",
                "-nostdin",
                "-f",
                "lavfi",
                "-i",
                f"testsrc2=size={'160x90' if small else '640x360'}:rate=24",
                "-t",
                "2",
                "-c:v",
                "libx264",
                "-threads",
                "1",
                "-preset",
                "ultrafast",
                "-crf",
                "32" if small else "16",
                str(path),
            ],
            check=True,
            capture_output=True,
            timeout=60,
        )

    def fixtures(self):
        for name in ("a", "b", "originals", "mobile", "generated"):
            (self.root / name).mkdir()
        large, small = self.root / "sample.mkv", self.root / "sample.mp4"
        self.encode(large)
        self.encode(small, True)
        for case in self.key["cases"]:
            path = self.root / case["location"] / case["path"]
            if case.get("damaged"):
                path.write_bytes(b"")
            elif case.get("role") == "subtitle":
                path.write_text("1\n00:00:00,100 --> 00:00:01,000\nRiver notes.\n")
            elif case.get("duplicate_of"):
                original = next(
                    c for c in self.key["cases"] if c["id"] == case["duplicate_of"]
                )
                shutil.copyfile(
                    self.root / original["location"] / original["path"], path
                )
            else:
                # Only the designated duplicate shares bytes. Distinct fictional
                # releases need distinct containers, even though clips are tiny.
                subprocess.run(
                    [
                        "ffmpeg",
                        "-v",
                        "error",
                        "-nostdin",
                        "-i",
                        str(small if case.get("small") else large),
                        "-c",
                        "copy",
                        "-metadata",
                        f"comment=synthetic-occurrence-{case['id']}",
                        str(path),
                    ],
                    check=True,
                    capture_output=True,
                    timeout=60,
                )
        self.hashes = {
            str(p.relative_to(self.root)): digest(p)
            for name in ("a", "b")
            for p in (self.root / name).iterdir()
        }
        write(
            self.root / "evidence.json",
            {
                "cases": self.key["cases"],
                "notice": "Fictional reference evidence, not a held-out agent trial.",
            },
        )

    def inventory(self):
        rows, cursor = [], None
        while True:
            args = ["files", "--limit", "4"]
            if cursor:
                args += ["--cursor", cursor]
            page = self.cli(*args)
            rows.extend(page["files"])
            cursor = page.get("next_cursor")
            if not cursor:
                break
        return {(r["location"], r["path"]): r["id"] for r in rows}

    def identify(self, case, files):
        if case["identity"] is None:
            self.review["cases"][case["id"]] = {
                "state": "deferred",
                "reason": case["evidence"],
                "alternatives": case.get("alternatives", []),
            }
            for candidate in case.get("alternatives", []):
                path = self.root / "candidate.json"
                write(
                    path,
                    {
                        "item": {
                            "kind": "movie",
                            "identities": {self.key["namespace"]: candidate},
                            "metadata": {"title": Path(case["path"]).stem},
                        }
                    },
                )
                self.cli(
                    "proposal",
                    "put",
                    "--file-id",
                    files[(case["location"], case["path"])],
                    "--file",
                    str(path),
                    "--source",
                    "reference:ambiguous",
                    "--evidence",
                    json.dumps({"reason": case["evidence"]}),
                )
            return None
        payload = {
            "item": {
                "kind": case.get("kind", "movie"),
                "identities": {self.key["namespace"]: case["identity"]},
                "metadata": {"title": case["title"], "year": case["year"]},
            },
            "role": case.get("role", "primary"),
        }
        path = self.root / "proposal.json"
        write(path, payload)
        proposal = self.cli(
            "proposal",
            "put",
            "--file-id",
            files[(case["location"], case["path"])],
            "--file",
            str(path),
            "--source",
            "reference",
            "--evidence",
            json.dumps(
                {
                    "reason": case.get(
                        "evidence", "Reviewed fictional transfer ledger"
                    ),
                    "case": case["id"],
                }
            ),
        )
        self.cli("proposal", "show", proposal["id"])
        accepted = self.cli(
            "proposal", "accept", proposal["id"], "--actor", "reference"
        )
        self.review["cases"][case["id"]] = {
            "state": "accepted",
            "identity": case["identity"],
        }
        return accepted["result"]["item_id"]

    def checkpoint(self, name):
        self.checkpoints[name] = snapshot(self.root)
        write(self.root / "checkpoints.json", self.checkpoints)

    def maintenance(self, name):
        result = self.cli("maintenance", "--catalog", "originals")
        if not result["complete"]:
            raise AssertionError("maintenance incomplete")
        sync = next(stage for stage in result["stages"] if stage["stage"] == "sync")
        if sync["applied_count"] != 0:
            raise AssertionError("unchanged maintenance rewrote outputs")
        self.checkpoint(name)

    def exercise(self):
        self.fixtures()
        self.cli("init")
        for name in ("a", "b"):
            self.cli("location", "bind", name, "--root", str(self.root / name))
        self.cli("scan")
        files = self.inventory()
        if len(files) != len(self.key["cases"]):
            raise AssertionError("inventory omitted cases")
        items = {}
        for case in self.key["cases"]:
            item = self.identify(case, files)
            if item:
                items[case["id"]] = item
        self.cli(
            "item",
            "put",
            "--id",
            items["misleading"],
            "--kind",
            "movie",
            "--metadata",
            json.dumps(
                {
                    "title": "Quiet Harbour",
                    "year": 2010,
                    "review_note": "Filename corrected against the transfer ledger",
                }
            ),
        )
        self.cli(
            "item",
            "note",
            items["misleading"],
            "--actor",
            "reference:seeded-correction",
            "--text",
            "Retain the corrected title during repeat maintenance.",
        )
        for identity, kind, title in [
            ("series", "series", "Field Notes"),
            ("season", "season", "Season 1"),
        ]:
            self.cli(
                "item",
                "put",
                "--id",
                identity,
                "--kind",
                kind,
                "--metadata",
                json.dumps({"title": title}),
            )
        self.cli(
            "relationship",
            "put",
            "--source",
            "season",
            "--target",
            "series",
            "--kind",
            "part_of",
            "--position",
            "1",
        )
        self.cli(
            "relationship",
            "put",
            "--source",
            items["episode"],
            "--target",
            "season",
            "--kind",
            "part_of",
            "--position",
            "2",
        )
        damaged = next(c for c in self.key["cases"] if c["id"] == "damaged")
        self.cli(
            "process",
            "enqueue",
            "probe",
            "--file-id",
            files[(damaged["location"], damaged["path"])],
        )
        self.cli("process", "run", expected=3)
        for catalog in ("originals", "mobile"):
            self.cli("catalog", "bind", catalog, "--root", str(self.root / catalog))
        policy = self.root / "copies.json"
        write(
            policy,
            {
                "prefer": [
                    {"field": "size", "order": "desc"},
                    {"field": "location", "values": ["a", "b"]},
                ]
            },
        )
        self.cli("copies", "put", "--catalog", "originals", "--file", str(policy))
        self.cli("layout", "put", "native", "--preset", "catabolic")
        self.cli("layout", "preview", "native", "--catalog", "originals")
        self.cli("layout", "apply", "native", "--catalog", "originals")
        self.cli("sync", "--catalog", "originals", "--dry-run")
        code = self.cli("sync", "--catalog", "originals", crash="journal", expected=77)
        before = snapshot(self.root)["pending"]
        self.cli("recover", "--catalog", "originals")
        self.cli("sync", "--catalog", "originals")
        self.cli("verify", "--catalog", "originals")
        self.checkpoints["recovery"] = {
            "journal": {
                "interrupted_exit": code,
                "pending_before": before,
                "verified": snapshot(self.root)["pending"] == 0,
            }
        }
        self.cli(
            "artifact", "bind", "generated", "--root", str(self.root / "generated")
        )
        definition = self.cli(
            "rendition", "define", "mobile", "--definition", '{"purpose":"transcode"}'
        )
        self.cli(
            "rendition",
            "policy",
            "--catalog",
            "mobile",
            "--definition",
            json.dumps({"definition_id": definition["id"]}),
        )
        self.cli("layout", "apply", "native", "--catalog", "mobile")
        self.cli("catalog-refresh", "enable", "--catalog", "mobile")
        if any(p.is_symlink() for p in (self.root / "mobile").rglob("*")):
            raise AssertionError("mobile catalog published before generation")
        recipe = self.cli(
            "artifact",
            "recipe",
            "mobile",
            "--preset",
            "h264-720p",
            "--output-definition",
            definition["id"],
            "--options",
            '{"crf":38,"audio_stream":null}',
        )
        source = next(c for c in self.key["cases"] if c["id"] == "clear")
        self.cli(
            "artifact",
            "enqueue",
            "--file-id",
            files[(source["location"], source["path"])],
            "--item-id",
            items["clear"],
            "--recipe",
            recipe["id"],
            "--location",
            "generated",
        )
        code = self.cli(
            "artifact", "run", "--limit", "1", crash="completion", expected=77
        )
        before = snapshot(self.root)
        self.cli("catalog-refresh", "run", "--force")
        self.cli("verify", "--catalog", "mobile")
        after = snapshot(self.root)
        self.checkpoints["recovery"]["completion"] = {
            "interrupted_exit": code,
            "pending_before": before["refresh"],
            "verified": before["completed"] == after["completed"] == 1
            and after["refresh"] == 0,
        }
        # A second result exercises ordinary producer-driven refresh, without
        # running the retry worker or manual layout/sync after completion.
        second = next(c for c in self.key["cases"] if c["id"] == "same-title")
        self.cli(
            "artifact",
            "enqueue",
            "--file-id",
            files[(second["location"], second["path"])],
            "--item-id",
            items["same-title"],
            "--recipe",
            recipe["id"],
            "--location",
            "generated",
        )
        result = self.cli("artifact", "run", "--limit", "1")
        if not result.get("catalog_refresh", {}).get("complete"):
            raise AssertionError("producer did not automatically refresh links")
        generated = [p for p in (self.root / "generated").rglob("*.mp4") if p.is_file()]
        if (
            len(generated) != 2
            or max(p.stat().st_size for p in generated)
            >= (self.root / source["location"] / source["path"]).stat().st_size
        ):
            raise AssertionError("rendition is not one smaller file")
        expected = [
            f"{c['location']}/{c['path']}"
            for c in self.key["cases"]
            if c["identity"] is not None and c["id"] not in ("duplicate", "small-copy")
        ]
        self.key["expected_targets"] = {
            "originals": expected,
            "mobile": [str(p.relative_to(self.root)) for p in generated],
        }
        self.checkpoint("initial")
        self.maintenance("repeat1")
        self.maintenance("repeat2")
        detached = self.root / "offline-a"
        (self.root / "a").rename(detached)
        (self.root / "a").mkdir()
        try:
            self.cli("scan", "a", expected=3)
            self.cli("sync", "--catalog", "originals", expected=3)
        finally:
            (self.root / "a").rmdir()
            detached.rename(self.root / "a")
        self.cli("scan", "a")
        self.cli("sync", "--catalog", "originals")
        self.checkpoints["outage_recovered"] = (
            snapshot(self.root) == self.checkpoints["repeat2"]
        )
        late = self.key.pop("late_case")
        target = self.root / late["location"] / late["path"]
        shutil.copyfile(self.root / "sample.mkv", target)
        self.hashes[str(target.relative_to(self.root))] = digest(target)
        self.cli("scan")
        self.identify(late, self.inventory())
        self.key["cases"].append(late)
        self.key["expected_targets"]["originals"].append(
            str(target.relative_to(self.root))
        )
        self.cli("maintenance", "--catalog", "originals")
        # Inventory changes conservatively enqueue enabled catalog automation.
        # Drain the independently maintained mobile projection before asserting
        # that the whole fixture has no pending work.
        self.cli("catalog-refresh", "run", "--force")
        self.checkpoint("late")
        self.maintenance("late_repeat1")
        self.maintenance("late_repeat2")
        self.checkpoints["source_preserved"] = all(
            digest(self.root / path) == value for path, value in self.hashes.items()
        )
        write(self.root / "source-hashes.json", self.hashes)
        return evaluate(self.key, snapshot(self.root), self.review, self.checkpoints)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python", default=sys.executable)
    parser.add_argument("--root", help="new directory; existing paths refused")
    args = parser.parse_args()
    root = (
        Path(args.root).absolute()
        if args.root
        else Path(tempfile.mkdtemp(prefix="catabolic-experience-")) / "run"
    )
    root.mkdir()
    journey = Journey(root.resolve(), str(Path(args.python).absolute()))
    report = {
        "passed": False,
        "run_type": "reference",
        "agent_trials": "not_run",
        "root": str(root),
    }
    started = time.monotonic()
    try:
        report.update(journey.exercise())
    except Exception as exc:
        report["error"] = f"{type(exc).__name__}: {exc}"
    finally:
        report.update(
            seconds=time.monotonic() - started,
            commands=journey.commands,
            corpus_sha256=digest(CORPUS),
            harness_sha256=digest(Path(__file__)),
            evaluator_sha256=digest(
                Path(__file__).with_name("experience_evaluator.py")
            ),
            platform=platform.platform(),
            agent_isolation="not_verified_for_full_runner",
        )
        try:
            provenance = subprocess.run(
                [
                    journey.python,
                    "-c",
                    "import importlib.metadata as m,json,sys; d=m.distribution('catabolic'); "
                    "print(json.dumps({'python':sys.version,'package_version':d.version,"
                    "'installation':json.loads(d.read_text('direct_url.json') or '{}')}))",
                ],
                cwd=root,
                env=journey.env,
                check=True,
                capture_output=True,
                text=True,
                timeout=15,
            )
            report["installation"] = json.loads(provenance.stdout)
            report["ffmpeg"] = subprocess.run(
                ["ffmpeg", "-version"],
                check=True,
                capture_output=True,
                text=True,
                timeout=15,
            ).stdout.splitlines()[0]
        except (OSError, ValueError, subprocess.SubprocessError) as exc:
            report["provenance_error"] = str(exc)
            report["passed"] = False
        write(root / "answer-key.json", journey.key)
        write(root / "review.json", journey.review)
        write(root / "checkpoints.json", journey.checkpoints)
        write(root / "report.json", report)
        print(json.dumps(report, indent=2))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
