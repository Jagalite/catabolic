#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Separate installed 24-row Artifacts/Processing gate; fixtures only."""

import argparse
import functools
import hashlib
import importlib.util
import inspect
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import wave
from pathlib import Path

PREFIX = "python:catabolic."
METHODS = {
    "artifacts.Artifacts": (
        "",
        "__init__",
        "bind",
        "recipe",
        "default_output",
        "get_recipe",
        "recipes",
        "list",
        "get",
        "enqueue",
        "recover",
        "run",
    ),
    "processing.Processing": (
        "",
        "__init__",
        "enqueue",
        "list",
        "get",
        "attempts",
        "cancel",
        "retry",
        "run",
        "facts",
        "duplicates",
        "search",
    ),
}
CONTRACTS = {
    PREFIX + owner + ("." + name if name else ""): {
        "tested": "Persistent owner lifecycle and paired selected method contracts",
        "gap": "Selected contracts only; exhaustive source/lease races, every invalid JSON shape and all byte/count boundaries remain unqualified",
    }
    for owner, names in METHODS.items()
    for name in names
}
CONTRACTS[PREFIX + "artifacts.Artifacts.enqueue"]["gap"] = (
    "JSON capabilities, metadata/full cache choice, lineage and package validation are adapted; exhaustive historical derivative/package combinations and source races remain unqualified"
)
CONTRACTS[PREFIX + "processing.Processing.run"]["gap"] = (
    "Worker/device/group/retry/requested-job adapters are exercised by separate native fixture tests; every scheduling race and large concurrent catalog remains unqualified"
)

_DETAILS = {
    "artifacts.Artifacts": (
        "Native owner identity and ten public methods",
        "Foreign legacy Store objects and arbitrary non-JSON internal objects are unsupported",
    ),
    "artifacts.Artifacts.__init__": (
        "Application, Store and profile retained",
        "No legacy Python connection sharing",
    ),
    "artifacts.Artifacts.bind": (
        "Generated binding, idempotence and returned mapping",
        "Full ownership-marker failure and root-relocation race matrix remains unqualified",
    ),
    "artifacts.Artifacts.recipe": (
        "Defaults, immutable revision reuse, real digest and nested rollback",
        "All render presets and option boundary combinations remain unqualified",
    ),
    "artifacts.Artifacts.default_output": (
        "Builtin audio-flac definition and native immutable owner",
        "Other preset roles are not separately exercised here",
    ),
    "artifacts.Artifacts.get_recipe": (
        "Decoded saved recipe and missing-ID error",
        "Analysis-kind rejection and every corrupt serialized row remain unqualified",
    ),
    "artifacts.Artifacts.recipes": (
        "Default/limited page, invalid limit and same-owner transaction visibility",
        "Every scalar cursor and result-byte boundary remains unqualified",
    ),
    "artifacts.Artifacts.list": (
        "Empty/filter pages and installed published artifact",
        "Multi-page artifact ordering and every byte boundary remain unqualified",
    ),
    "artifacts.Artifacts.get": (
        "Missing artifact and decoded real published artifact",
        "Exhaustive malformed stored JSON and cross-profile cases remain unqualified",
    ),
    "artifacts.Artifacts.enqueue": (
        "Injected capabilities, metadata/full cache checks, ignored ordinary lineage, package rejection, transaction rollback and real transform admission",
        "All accepted historical derivative/package combinations and source races remain unqualified; discovery/full-cache paths requiring detach reject active transactions",
    ),
    "artifacts.Artifacts.recover": (
        "No-op recovery after real completed publication",
        "Each interrupted publication phase and recovery cancellation boundary remains unqualified by this facade suite",
    ),
    "artifacts.Artifacts.run": (
        "Explicit empty scope, real FLAC publication, refresh suppression and custom signal child reaping",
        "Every failure phase, preset and catalog-refresh target remains unqualified; execution rejects active caller transactions",
    ),
    "processing.Processing": (
        "Native owner identity and ten public methods",
        "Foreign legacy Store objects are unsupported",
    ),
    "processing.Processing.__init__": (
        "Application, Store and profile retained",
        "No legacy Python connection sharing",
    ),
    "processing.Processing.enqueue": (
        "Explicit IDs, source page/cursor, cache reuse, invalid configuration, nested rollback and detached probe discovery",
        "Nonempty read-only admission is explicitly refused even if a legacy call could reuse cache; all recipe/backend combinations remain unqualified",
    ),
    "processing.Processing.list": (
        "Empty/filter pages and real queued/running result decoding",
        "Large result omission and every page/byte boundary remain unqualified",
    ),
    "processing.Processing.get": (
        "Missing-ID error and complete hash/text/render job shapes",
        "All terminal-state result shapes and malformed rows remain unqualified",
    ),
    "processing.Processing.attempts": (
        "Missing-job precedence and serialized result attempt pages",
        "Exhaustive attempt cursor/result-omission boundaries remain unqualified",
    ),
    "processing.Processing.cancel": (
        "Unknown, queued and render job cancellation",
        "Live publication-intent cancellation races remain unqualified by this facade suite",
    ),
    "processing.Processing.retry": (
        "Missing rejection and cancelled-job requeue",
        "Every stale claim and failed/timeout transition is not separately paired here",
    ),
    "processing.Processing.run": (
        "Defaults, exact IDs/empty scope, worker/group arguments, run validation, real tools, custom signal and detached writer availability",
        "Scheduler Rust tests separately cover retries/reuse/throttles/leases; every concurrent interleaving remains unqualified; run rejects active transactions",
    ),
    "processing.Processing.facts": (
        "Empty and decoded current hash/text facts",
        "All invalidation and source-change combinations remain unqualified",
    ),
    "processing.Processing.duplicates": (
        "Empty and actual equal-SHA256 duplicate groups",
        "Multiple digest/algorithm/size page boundaries remain unqualified",
    ),
    "processing.Processing.search": (
        "Empty, invalid, Unicode casefolded and paginated current text hits",
        "Every Unicode scalar/version and maximum-byte result combination remains unqualified",
    ),
}
for _name, (_tested, _gap) in _DETAILS.items():
    CONTRACTS[PREFIX + _name] = {"tested": _tested, "gap": _gap}


def sibling(name):
    spec = importlib.util.spec_from_file_location(
        name, Path(__file__).with_name(name + ".py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def api(engine):
    if engine == "reference":
        from catabolic.app import Application
        from catabolic.artifacts import Artifacts
        from catabolic.domain import CatabolicError
        from catabolic.processing import Processing
        from catabolic.store import Store, encode
    else:
        from catabolic_native import (
            Application,
            Artifacts,
            CatabolicError,
            Processing,
            Store,
            encode,
        )
    return Application, Store, Artifacts, Processing, CatabolicError, encode


def trace():
    from catabolic_native import artifacts, processing

    calls = {}
    for module, cls in (
        (artifacts, artifacts.Artifacts),
        (processing, processing.Processing),
    ):
        for name, fn in vars(cls).copy().items():
            if inspect.isfunction(fn) and (
                not name.startswith("_") or name == "__init__"
            ):
                key = (
                    PREFIX
                    + module.__name__.split(".")[-1]
                    + "."
                    + cls.__name__
                    + "."
                    + name
                )

                def wrap(fn, key):
                    @functools.wraps(fn)
                    def counted(*args, **kwargs):
                        calls[key] = calls.get(key, 0) + 1
                        return fn(*args, **kwargs)

                    return counted

                setattr(cls, name, wrap(fn, key))
    return calls


def seed_files(root):
    source = root / "source"
    source.mkdir(exist_ok=True)
    (root / "generated").mkdir(exist_ok=True)
    if not (source / "a.txt").exists():
        (source / "a.txt").write_text("Hello Straße\nHELLO world\n", encoding="utf-8")
        (source / "b.txt").write_bytes((source / "a.txt").read_bytes())
        with wave.open(str(source / "tone.wav"), "wb") as out:
            out.setnchannels(1)
            out.setsampwidth(2)
            out.setframerate(8000)
            out.writeframes(b"\0\0" * 1600)


def seed(app, root):
    seed_files(root)
    app.bind("source", "media", str(root / "source"))
    app.put_item("movie", {}, {"title": "Media fixture"}, "item")
    with app.store.transaction() as db:
        db.execute(
            "INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','media',1,3,'[]')"
        )
        for file, path in (("a", "a.txt"), ("b", "b.txt"), ("tone", "tone.wav")):
            st = (root / "source" / path).stat()
            db.execute(
                "INSERT INTO files(id,location,path) VALUES (?,'media',?)", (file, path)
            )
            db.execute(
                "INSERT INTO observations(profile,file_id,size,mtime_ns,device,inode,status,scan_id) VALUES ('default',?,?,?,?,?,'present','scan')",
                (file, st.st_size, st.st_mtime_ns, st.st_dev, st.st_ino),
            )
    app.media.associate("tone", "item", role="primary")


def scenario(root, engine):
    Application, Store, Artifacts, Processing, Error, encode = api(engine)
    result, ids = {}, {}
    with Store(root / (engine + ".db"), writable=True) as store:
        app = Application(store)
        seed(app, root)
        artifacts, processing = Artifacts(app), Processing(app)
        result["owners"] = [
            artifacts.app is app,
            artifacts.store is store,
            artifacts.profile,
            processing.app is app,
            processing.store is store,
            processing.profile,
        ]

        def save(name, value):
            if isinstance(value, dict) and "cache_key" in value:
                operation_key = (
                    recipe["digest"]
                    if value["operation"] == "render"
                    else value["operation"]
                )
                assert (
                    value["cache_key"]
                    == hashlib.sha256(
                        encode(
                            [value["snapshot"], operation_key, value["options"]]
                        ).encode()
                    ).hexdigest()
                )
            result[name] = value
            return value

        def label(value, name):
            ids[value] = name
            return value

        def error(name, action):
            try:
                action()
            except Error as exc:
                result[name] = str(exc)
            else:
                raise AssertionError(name + " did not fail")

        save("empty_artifacts", artifacts.list())
        save("empty_jobs", processing.list())
        save("empty_facts", processing.facts("missing"))
        save("empty_duplicates", processing.duplicates())
        save("empty_search", processing.search("hello"))
        for name, action in (
            ("bad_search", lambda: processing.search("___")),
            ("bad_job", lambda: processing.get("missing")),
            ("bad_artifact", lambda: artifacts.get("missing")),
            ("bad_recipe", lambda: artifacts.get_recipe("missing")),
            ("bad_attempts", lambda: processing.attempts("missing", limit=0)),
            ("bad_workers", lambda: processing.run(workers=True)),
            ("bad_per_device", lambda: processing.run(workers=2, per_device=3)),
            ("bad_retry", lambda: processing.run(retry_transient=11)),
            ("bad_retry_delay", lambda: processing.run(retry_delay=0)),
            ("bad_groups", lambda: processing.run(storage_groups={"media": ""})),
            ("bad_scope", lambda: processing.run(job_ids="x")),
            ("tuple_scope", lambda: processing.run(job_ids=())),
            ("tuple_artifact_scope", lambda: artifacts.run(job_ids=())),
            ("tuple_file_ids", lambda: processing.enqueue("hash", file_ids=())),
            ("bad_artifact_scope", lambda: artifacts.run(job_ids="x")),
            ("bad_location", lambda: processing.enqueue("hash", location="missing")),
            (
                "bad_options",
                lambda: processing.enqueue("hash", options={"timeout": True}),
            ),
            ("bad_ids", lambda: processing.enqueue("hash", file_ids="x")),
            ("bad_limit", lambda: artifacts.recipes(limit=0)),
        ):
            error(name, action)
        save("cancel_missing", processing.cancel("missing"))
        error("retry_missing", lambda: processing.retry("missing"))
        output = artifacts.default_output("audio-flac")
        label(output["id"], "output")
        save("default_output", output)
        recipe = artifacts.recipe("audio", "audio-flac", {"reserve_bytes": 0})
        label(recipe["id"], "recipe")
        label(recipe["digest"], "recipe-digest")
        assert (
            recipe["digest"]
            == hashlib.sha256(encode(recipe["definition"]).encode()).hexdigest()
        )
        save("recipe", recipe)
        save(
            "recipe_reused",
            artifacts.recipe("audio", "audio-flac", {"reserve_bytes": 0}) == recipe,
        )
        save("get_recipe", artifacts.get_recipe(recipe["id"]))
        save("recipes", artifacts.recipes(limit=1))
        save("recipe_cursor_number", artifacts.recipes(after=0))
        save("recipe_cursor_null", artifacts.recipes(after=None))
        save("binding", dict(artifacts.bind("generated", root / "generated")))
        save("binding_reused", dict(artifacts.bind("generated", root / "generated")))
        capabilities = {
            "presets": [{"name": "audio-flac", "available": True}],
            "tools": {"fixture": "unexecuted"},
        }
        queued = artifacts.enqueue(
            "tone",
            recipe["id"],
            "generated",
            "item",
            _capabilities=capabilities,
            _full_cache_check=False,
            _source_lineage_mode="current_source_revision",
        )
        label(queued["job_id"], "render-job")
        save("artifact_enqueue", queued)
        save(
            "artifact_cached",
            artifacts.enqueue(
                "tone", recipe["id"], "generated", "item", _capabilities=capabilities
            ),
        )
        save("artifact_scope_empty", artifacts.run(job_ids=[], _refresh=False))
        save("recover_empty", artifacts.recover())
        save("render_job", processing.get(queued["job_id"]))
        save("cancel_render", processing.cancel(queued["job_id"]))
        save("retry_render", processing.retry(queued["job_id"]))
        save("cancel_render_again", processing.cancel(queued["job_id"]))
        for operation, files in (("hash", ["a", "b"]), ("text", ["a"])):
            q = processing.enqueue(operation, file_ids=files)
            for n, value in enumerate(q["queued"]):
                label(value, operation + "-" + str(n))
            save(operation + "_enqueue", q)
            save(operation + "_cache", processing.enqueue(operation, file_ids=files))
            save(operation + "_scope_empty", processing.run(job_ids=[], _refresh=False))
            save(
                operation + "_run",
                processing.run(
                    workers=2,
                    per_device=1,
                    storage_groups={"media": "fixture"},
                    job_ids=q["queued"],
                    _refresh=False,
                ),
            )
            save(operation + "_job", processing.get(q["queued"][0]))
            save(operation + "_attempts", processing.attempts(q["queued"][0]))
        save("facts", processing.facts("a"))
        save("duplicates", processing.duplicates(limit=1))
        save("duplicates_cursor_null", processing.duplicates(after=None))
        save("duplicates_cursor_number", processing.duplicates(after=7))
        save("search_unicode", processing.search("STRASSE"))
        first = save("search_page", processing.search("hello", limit=1))
        save(
            "search_after",
            processing.search("hello", limit=1, after=first["next_after"]),
        )
        q = processing.enqueue("sniff", location="media", limit=1)
        for value in q["queued"]:
            label(value, "sniff")
        save("enqueue_page", q)
        save("list_filter", processing.list(state="queued", file_id="a"))
        save(
            "list_cursor_number", processing.list(state="queued", file_id="a", after=0)
        )
        save(
            "enqueue_cursor_number",
            processing.enqueue("sniff", limit=1, after=999999999),
        )
        save("cancel_sniff", processing.cancel(q["queued"][0]))
        save("retry_sniff", processing.retry(q["queued"][0]))
        save("final_run", processing.run(limit=1, _refresh=False))
        save("final_noop", processing.run(job_ids=[], _refresh=False))
        save("artifact_filter", artifacts.list(state="ready", file_id="tone"))
        error(
            "package_rejected",
            lambda: artifacts.enqueue(
                "tone",
                recipe["id"],
                "generated",
                "item",
                _capabilities=capabilities,
                _component_package={},
            ),
        )
        error(
            "capability_rejected",
            lambda: artifacts.enqueue(
                "tone",
                recipe["id"],
                "generated",
                "item",
                _capabilities={
                    "presets": [
                        {
                            "name": "audio-flac",
                            "available": False,
                            "missing": ["fixture"],
                        }
                    ],
                    "tools": {},
                },
            ),
        )

    def clean(value):
        if isinstance(value, dict):
            return {
                k: clean(v)
                for k, v in value.items()
                if k
                not in (
                    "created_at",
                    "finished_at",
                    "started_at",
                    "retry_after",
                    "cache_key",
                )
            }
        if isinstance(value, list):
            return [clean(v) for v in value]
        if isinstance(value, str):
            if value in ids:
                return ids[value]
            # Attempts intentionally retain legacy serialized result strings.
            return value
        return value

    return clean(result)


class InstalledProcessing(unittest.TestCase):
    def setUp(self):
        self.Application, self.Store, self.Artifacts, self.Processing, self.Error, _ = (
            api("native")
        )
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.path = self.root / "catalog.db"
        self.Store.initialize(self.path)
        self.store = self.Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        self.app = self.Application(self.store)
        seed(self.app, self.root)
        self.artifacts, self.processing = (
            self.Artifacts(self.app),
            self.Processing(self.app),
        )

    def test_nested_transactions_and_recipe_admission(self):
        with self.store.transaction():
            first = self.artifacts.recipe("outer", "audio-flac")
            try:
                with self.store.transaction():
                    inner = self.artifacts.recipe("inner", "audio-flac")
                    self.processing.enqueue("hash", file_ids=["a"])
                    raise ValueError("rollback")
            except ValueError:
                pass
            self.assertEqual(self.artifacts.get_recipe(first["id"]), first)
            with self.assertRaises(self.Error):
                self.artifacts.get_recipe(inner["id"])
            self.assertEqual(self.processing.list()["jobs"], [])
        self.assertEqual(self.artifacts.recipes()["recipes"], [first])

    def test_injected_capabilities_enqueue_rolls_back_and_poisoned_owner_refuses(self):
        self.artifacts.bind("generated", self.root / "generated")
        recipe = self.artifacts.recipe("flac", "audio-flac", {"reserve_bytes": 0})
        cap = {"presets": [{"name": "audio-flac", "available": True}], "tools": {}}
        try:
            with self.store.transaction():
                job = self.artifacts.enqueue(
                    "tone",
                    recipe["id"],
                    "generated",
                    "item",
                    _capabilities=cap,
                    _full_cache_check=False,
                )
                self.assertFalse(job["reused"])
                raise ValueError("rollback fixture")
        except ValueError:
            pass
        self.assertEqual(self.processing.list()["jobs"], [])
        with self.assertRaisesRegex(self.Error, "aborted"):
            with self.store.transaction():
                try:
                    self.store.db.execute(
                        "INSERT OR ROLLBACK INTO profiles VALUES ('default')"
                    )
                except self.Error:
                    pass
                with self.assertRaisesRegex(self.Error, "aborted"):
                    self.artifacts.recipes()
                with self.assertRaisesRegex(self.Error, "aborted"):
                    self.processing.run(job_ids=[])

    def test_outer_transaction_runtime_refusal(self):
        with self.store.transaction():
            self.processing.enqueue("hash", file_ids=["a"])
            with self.assertRaisesRegex(self.Error, "outside an active transaction"):
                self.processing.run()
            with self.assertRaises(self.Error):
                self.processing.enqueue("probe", file_ids=["tone"])
            self.assertEqual(len(self.processing.list()["jobs"]), 1)

    def test_snapshot_readonly_closed_detached_and_threads(self):
        reader = self.Store(self.path)
        self.addCleanup(reader.close)
        read = self.Processing(self.Application(reader))
        self.assertEqual(read.list()["jobs"], [])
        self.processing.enqueue("hash", file_ids=["a"])
        self.assertEqual(read.list()["jobs"], [])
        with self.assertRaises(self.Error):
            read.enqueue("hash", file_ids=["b"])
        reader.close()
        with self.store.transaction():
            errors = []
            thread = threading.Thread(
                target=lambda: self._catch(errors, self.processing.list)
            )
            thread.start()
            thread.join()
            self.assertEqual(len(errors), 1)
        self.store._owner.detach()
        with self.assertRaises(self.Error):
            self.processing.list()
        self.store._owner.reconnect()
        self.store.close()
        with self.assertRaises(self.Error):
            self.artifacts.recipes()

    @staticmethod
    def _catch(errors, action):
        try:
            action()
        except BaseException as exc:
            errors.append(exc)

    def test_analysis_real_tools_and_text(self):
        for op, file in (
            ("probe", "tone"),
            ("decode", "tone"),
            ("hash", "a"),
            ("text", "a"),
        ):
            queued = self.processing.enqueue(op, file_ids=[file])["queued"]
            report = self.processing.run(job_ids=queued, _refresh=False)
            self.assertEqual(report["counts"], {"complete": 1}, (op, report))
            self.assertEqual(self.processing.get(queued[0])["state"], "complete")
        self.assertTrue(self.processing.search("strasse")["hits"][0]["current"])

    def test_artifact_real_transform_and_both_cache_modes(self):
        self.artifacts.bind("generated", self.root / "generated")
        recipe = self.artifacts.recipe("flac", "audio-flac", {"reserve_bytes": 0})
        job = self.artifacts.enqueue("tone", recipe["id"], "generated", "item")
        result = self.artifacts.run(job_ids=[job["job_id"]], _refresh=False)
        self.assertTrue(result["complete"], result)
        self.assertEqual(len(result["completed"]), 1)
        artifact = self.artifacts.get(result["completed"][0]["artifact_id"])
        self.assertEqual(artifact["state"], "ready")
        self.assertTrue((self.root / "generated" / artifact["path"]).is_file())
        for full in (False, True):
            cached = self.artifacts.enqueue(
                "tone", recipe["id"], "generated", "item", _full_cache_check=full
            )
            self.assertTrue(cached["reused"])
            self.assertEqual(cached["artifact_id"], artifact["id"])
        self.assertEqual(self.artifacts.recover()["recovered"], [])

    def test_cancel_retry_exact_scope(self):
        queued = self.processing.enqueue("hash", file_ids=["a", "b"])["queued"]
        self.processing.cancel(queued[0])
        self.processing.retry(queued[0])
        self.assertEqual(
            self.processing.run(job_ids=[], _refresh=False)["processed"], 0
        )
        self.assertEqual(
            self.processing.run(job_ids=[queued[0]], _refresh=False)["processed"], 1
        )
        self.assertEqual(self.processing.get(queued[1])["state"], "queued")
        self.assertEqual(
            self.processing.run(job_ids=[queued[0]], _refresh=False)["processed"], 0
        )

    def _signal_tool(self, mode, custom):
        tools = self.root / "tools"
        tools.mkdir()
        marker = self.root / "tool-pid"
        probe = tools / ("ffmpeg" if mode == "artifact" else "ffprobe")
        probe.write_text(
            "#!/bin/sh\n"
            + (
                'if [ "$1" = \'-version\' ]; then exec /usr/bin/ffprobe "$@"; fi\n'
                if mode == "run"
                else 'if [ "$1" = -version ] || [ "$2" = -encoders ] || [ "$2" = -muxers ] || [ "$2" = -filters ]; then exec /usr/bin/ffmpeg "$@"; fi\n'
                if mode == "artifact"
                else ""
            )
            + f"echo $$ > '{marker}'\nexec /bin/sleep 30\n"
        )
        probe.chmod(0o700)
        old_path = os.environ["PATH"]
        os.environ["PATH"] = str(tools) + ":" + old_path
        self.addCleanup(os.environ.__setitem__, "PATH", old_path)
        if mode == "run":
            queued = self.processing.enqueue("probe", file_ids=["tone"])["queued"]

            def action():
                return self.processing.run(job_ids=queued, _refresh=False)
        elif mode == "artifact":
            self.artifacts.bind("generated", self.root / "generated")
            recipe = self.artifacts.recipe("flac", "audio-flac", {"reserve_bytes": 0})
            queued = self.artifacts.enqueue("tone", recipe["id"], "generated", "item")[
                "job_id"
            ]

            def action():
                return self.artifacts.run(job_ids=[queued], _refresh=False)
        else:

            def action():
                return self.processing.enqueue("probe", file_ids=["tone"])

        class SignalException(Exception):
            pass

        number = signal.SIGUSR1 if custom else signal.SIGINT
        previous = signal.getsignal(number)
        if custom:

            def handler(*unused):
                raise SignalException("exact fixture signal")

            signal.signal(number, handler)
        self.addCleanup(signal.signal, number, previous)
        errors, writer = [], []

        def interrupt():
            deadline = time.monotonic() + 10
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            if not marker.exists():
                errors.append("tool never started")
                return
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                try:
                    with self.Store(self.path, writable=True):
                        writer.append(True)
                    break
                except self.Error:
                    time.sleep(0.005)
            if not writer:
                errors.append("writer was never available during tool wait")
            os.kill(os.getpid(), number)

        thread = threading.Thread(target=interrupt)
        thread.start()
        began = time.monotonic()
        with self.assertRaises(SignalException if custom else KeyboardInterrupt):
            action()
        thread.join()
        self.assertLess(time.monotonic() - began, 10)
        self.assertEqual(errors, [])
        self.assertEqual(writer, [True])
        with self.assertRaises(ProcessLookupError):
            os.kill(int(marker.read_text()), 0)
        self.assertEqual(self.store.db.execute("SELECT 1").fetchone()[0], 1)
        if mode == "run":
            self.assertFalse(
                self.store.rows(
                    "SELECT * FROM execution_claims WHERE lease_until>unixepoch()"
                )
            )

    def test_enqueue_sigint_reaps_tool_and_releases_writer(self):
        self._signal_tool("enqueue", False)

    def test_processing_custom_signal_reaps_tool_and_releases_writer(self):
        self._signal_tool("run", True)

    def test_artifact_custom_signal_reaps_tool_and_releases_writer(self):
        self._signal_tool("artifact", True)


EXPECTED_TESTS = (
    "test_nested_transactions_and_recipe_admission",
    "test_injected_capabilities_enqueue_rolls_back_and_poisoned_owner_refuses",
    "test_outer_transaction_runtime_refusal",
    "test_snapshot_readonly_closed_detached_and_threads",
    "test_analysis_real_tools_and_text",
    "test_artifact_real_transform_and_both_cache_modes",
    "test_cancel_retry_exact_scope",
    "test_enqueue_sigint_reaps_tool_and_releases_writer",
    "test_processing_custom_signal_reaps_tool_and_releases_writer",
    "test_artifact_custom_signal_reaps_tool_and_releases_writer",
)


def main():
    if not __debug__:
        raise SystemExit(
            "processing qualification requires assertions; do not run with -O or -OO"
        )
    parser = argparse.ArgumentParser()
    parser.add_argument("--wheel", type=Path)
    parser.add_argument(
        "--dev",
        action="store_true",
        help="Explicit helperless development evidence; never release/wheel qualification",
    )
    parser.add_argument("--inventory", type=Path)
    parser.add_argument("--reference-root", type=Path)
    parser.add_argument("--reference-python", type=Path)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--scenario", choices=("reference", "native"))
    parser.add_argument("--root", type=Path)
    parser.add_argument("--reference-commit")
    args = parser.parse_args()
    if args.report:
        args.report.unlink(missing_ok=True)
    provenance = sibling("qualify_native_embedding")
    if args.scenario:
        oracle, calls = None, {}
        if args.scenario == "reference":
            oracle = provenance.reference_provenance(
                args.reference_root, args.reference_commit
            )
            sys.path.insert(0, str(args.reference_root / "src"))
        else:
            calls = trace()
        result = scenario(args.root, args.scenario)
        imports = (
            provenance.reference_imports(
                args.reference_root, oracle["tracked_source_sha256"]
            )
            if oracle
            else None
        )
        print(
            json.dumps(dict(result=result, calls=calls, oracle=oracle, imports=imports))
        )
        return
    assert (
        __debug__
        and (args.wheel or args.dev)
        and args.inventory
        and args.reference_root
        and args.reference_python
    )
    assert not (args.wheel and args.dev), (
        "dev and strict wheel evidence are separate modes"
    )
    installed = (
        sibling("qualify_native_wheel").verify_installed_wheel(args.wheel)
        if args.wheel
        else None
    )
    inventory = json.loads(args.inventory.read_text())
    rows = [row for row in inventory["rows"] if row["id"] in CONTRACTS]
    assert len(rows) == len(CONTRACTS) == 24
    oracle = provenance.reference_provenance(
        args.reference_root, inventory["reference_commit"]
    )
    calls = trace()
    result = sibling("native_qualification_results").run_required_suite(
        unittest.defaultTestLoader.loadTestsFromTestCase(InstalledProcessing),
        {InstalledProcessing: EXPECTED_TESTS},
        expected_count=10,
    )
    installed_calls = calls.copy()
    from catabolic_native import Store

    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        Store.initialize(root / "seed.db")
        outputs = {}
        for engine, python in (
            ("reference", args.reference_python),
            ("native", Path(sys.executable)),
        ):
            shutil.copyfile(root / "seed.db", root / (engine + ".db"))
            command = [
                str(python),
                "-I",
                "-B",
                "-X",
                f"pycache_prefix={root / (engine + '-pycache')}",
                str(Path(__file__).resolve()),
                "--scenario",
                engine,
                "--root",
                str(root),
            ]
            if engine == "reference":
                command += [
                    "--reference-root",
                    str(args.reference_root.resolve()),
                    "--reference-commit",
                    inventory["reference_commit"],
                ]
            completed = subprocess.run(
                command, cwd=directory, capture_output=True, text=True, timeout=120
            )
            assert completed.returncode == 0, (
                engine,
                completed.stdout,
                completed.stderr,
            )
            outputs[engine] = json.loads(completed.stdout)
        reference, native = outputs["reference"]["result"], outputs["native"]["result"]
        assert reference == native, json.dumps(
            {
                key: [reference.get(key), native.get(key)]
                for key in reference.keys() | native.keys()
                if reference.get(key) != native.get(key)
            },
            indent=2,
        )
        assert outputs["reference"]["oracle"] == oracle
        paired_calls = outputs["native"]["calls"]
        oracle["imported_modules"] = outputs["reference"]["imports"]
    if args.wheel:
        assert (
            sibling("qualify_native_wheel").verify_installed_wheel(args.wheel)
            == installed
        )
    report = dict(
        format=1,
        scope="Separate persistent Artifacts/Processing checkpoint; unchanged baseline 69 rows / 19 tests / 82 groups and query 10 rows / 10 tests / 77 groups",
        acceptance_tests=result.testsRun,
        successful_scenarios=result.successful_scenarios,
        paired_groups=len(native),
        installed_wheel=installed,
        wheel_sha256=hashlib.sha256(args.wheel.read_bytes()).hexdigest()
        if args.wheel
        else None,
        development_only=args.dev,
        inventory_sha256=hashlib.sha256(args.inventory.read_bytes()).hexdigest(),
        oracle=oracle,
        installed_calls=installed_calls,
        paired_calls=paired_calls,
        coverage=[],
    )
    for row in rows:
        key = row["id"] + (".__init__" if row["signature"]["kind"] == "class" else "")
        report["coverage"].append(
            dict(
                id=row["id"],
                classification=row["classification"],
                qualification="selected-contracts-tested-with-gaps"
                if paired_calls.get(key)
                else "unqualified",
                installed_calls=installed_calls.get(key, 0),
                paired_calls=paired_calls.get(key, 0),
                **CONTRACTS[row["id"]],
            )
        )
    import catabolic_native

    package = Path(catabolic_native.__file__).parent
    report["installed_package_sha256"] = {
        str(path.relative_to(package)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sorted(package.rglob("*"))
        if path.is_file() and "__pycache__" not in path.parts
    }
    report["script_sha256"] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    report["qualification_scripts_sha256"] = {
        name: hashlib.sha256(Path(__file__).with_name(name).read_bytes()).hexdigest()
        for name in (
            "native_processing_embedding_tests.py",
            "qualify_native_embedding.py",
            "qualify_native_wheel.py",
            "native_qualification_results.py",
        )
    }
    assert provenance.reference_provenance(
        args.reference_root, inventory["reference_commit"]
    ) == {key: value for key, value in oracle.items() if key != "imported_modules"}
    if args.report:
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            dict(
                tests=result.testsRun,
                paired_groups=len(native),
                inventory_rows=len(rows),
                report=str(args.report),
            )
        )
    )


if __name__ == "__main__":
    main()
