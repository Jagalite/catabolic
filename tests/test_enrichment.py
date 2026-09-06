"""New catalog workflows against disposable sources, real tools and crash fixtures."""

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
import wave
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.copy_selection import CopySelection
from catabolic.curation import Curation
from catabolic.domain import CatabolicError
from catabolic.graphql_query import execute_graphql
from catabolic.layouts import Layouts
from catabolic.network_adapters import Refresh, tmdb_candidates
from catabolic.processing import Processing, ProcessingFailure, command_output
from catabolic.sql_query import execute_sql
from catabolic.store import Store, encode


class EnrichmentTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.source = self.root / "source"
        self.source.mkdir()
        self.output = self.root / "output"
        self.output.mkdir()
        self.database = self.root / "catalog.sqlite3"
        self.file = self.source / "Example.wav"
        with wave.open(str(self.file), "wb") as audio:
            audio.setnchannels(1)
            audio.setsampwidth(2)
            audio.setframerate(8000)
            audio.writeframes(b"\0\0" * 800)
        Store.initialize(self.database)
        self.store = Store(self.database, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.app.bind("source", "media", str(self.source))
        self.app.bind("output", "global", str(self.output))
        self.app.scan()
        self.file_id = self.app.files()["files"][0]["id"]
        self.c = Curation(self.app)
        self.p = Processing(self.app)

    def item(self):
        return self.app.put_item(
            "track", {"fixture": "track"}, {"title": "Curated title"}
        )["id"]

    def proposal(self, **extra):
        return self.c.put(
            self.file_id,
            {
                "item": {
                    "kind": "track",
                    "identities": {"fixture": "track"},
                    "metadata": {"title": "Candidate title", "year": 2020},
                },
                **extra,
            },
            evidence={"reason": "test"},
        )

    def test_proposal_acceptance_is_atomic_idempotent_and_preserves_fields(self):
        item = self.item()
        proposal = self.proposal()
        self.assertEqual(self.proposal()["id"], proposal["id"])
        accepted = self.c.decide(proposal["id"], accept=True, actor="agent:test")
        self.assertEqual(accepted["result"]["item_id"], item)
        self.assertEqual(accepted["result"]["preserved_fields"], ["title"])
        self.assertEqual(
            self.c.decide(proposal["id"], accept=True, actor="agent:test"), accepted
        )
        self.assertEqual(len(self.c.history()["events"]), 1)
        metadata = json.loads(
            self.store.rows("SELECT metadata FROM items")[0]["metadata"]
        )
        self.assertEqual(metadata, {"title": "Curated title", "year": 2020})
        self.assertEqual(
            self.store.rows("SELECT role FROM item_files")[0]["role"], "primary"
        )

    def test_stale_and_rejected_proposals_cannot_mutate(self):
        proposal = self.proposal()
        self.item()
        with self.assertRaisesRegex(CatabolicError, "stale"):
            self.c.decide(proposal["id"], accept=True, actor="test")
        self.c.decide(proposal["id"], accept=False, actor="test")
        with self.assertRaisesRegex(CatabolicError, "already decided"):
            self.c.decide(proposal["id"], accept=True, actor="test")
        self.assertEqual(self.store.rows("SELECT * FROM item_files"), [])

    def test_acceptance_rolls_back_item_creation_on_association_error(self):
        proposal = self.proposal(part=0)
        with self.assertRaises(CatabolicError):
            self.c.decide(proposal["id"], accept=True, actor="test")
        self.assertEqual(self.store.rows("SELECT * FROM items"), [])
        self.assertEqual(self.c.get(proposal["id"])["state"], "pending")

    def test_acceptance_revalidates_live_source(self):
        proposal = self.proposal()
        self.file.unlink()
        with self.assertRaises(OSError):
            self.c.decide(proposal["id"], accept=True, actor="test")
        self.assertEqual(self.store.rows("SELECT * FROM items"), [])

    def test_hash_cache_integrity_mismatch_preserves_baseline(self):
        initial = self.file.read_bytes()
        job = self.p.enqueue("hash", file_ids=[self.file_id])["queued"][0]
        self.assertTrue(self.p.run()["complete"])
        self.assertEqual(
            self.p.enqueue("hash", file_ids=[self.file_id])["cached"], [job]
        )
        baseline = self.store.rows("SELECT * FROM content_baselines")[0]
        self.file.write_bytes(b"changed content")
        self.app.scan()
        self.p.enqueue("verify", file_ids=[self.file_id])
        result = self.p.run()
        self.assertEqual(result["counts"], {"mismatch": 1})
        self.assertEqual(
            self.store.rows("SELECT * FROM content_baselines")[0], baseline
        )
        self.assertEqual(baseline["digest"], hashlib.sha256(initial).hexdigest())
        self.assertFalse(self.p.facts(self.file_id)["facts"][0]["current"])

    def test_duplicates_keep_occurrences(self):
        other = self.source / "copy.wav"
        other.write_bytes(self.file.read_bytes())
        self.app.scan()
        self.p.enqueue("hash")
        self.p.run()
        self.assertEqual(self.p.duplicates()["groups"][0]["occurrences"], 2)
        self.assertEqual(len(self.app.files()["files"]), 2)

    def test_changed_source_job_never_publishes(self):
        self.p.enqueue("hash", file_ids=[self.file_id])
        self.file.write_bytes(b"new")
        self.assertEqual(self.p.run()["counts"], {"changed": 1})
        self.assertEqual(self.store.rows("SELECT * FROM content_baselines"), [])

    def test_cancel_retry_and_crash_claim_resume(self):
        job = self.p.enqueue("hash", file_ids=[self.file_id])["queued"][0]
        self.assertTrue(self.p.cancel(job)["cancelled"])
        self.assertEqual(self.p.run()["processed"], 0)
        self.p.retry(job)
        with self.store.transaction() as db:
            db.execute(
                "UPDATE processing_jobs SET state='running',attempts=1 WHERE id=?",
                (job,),
            )
        self.assertTrue(self.p.run()["complete"])
        self.assertEqual(self.p.list()["jobs"][0]["attempts"], 2)

    def test_worker_limits_and_safe_source_symlink(self):
        with self.assertRaises(CatabolicError):
            self.p.run(workers=100)
        self.p.enqueue("hash", file_ids=[self.file_id])
        self.file.rename(self.source / "original.wav")
        self.file.symlink_to(self.source / "original.wav")
        self.assertFalse(self.p.run()["complete"])
        self.assertEqual(self.store.rows("SELECT * FROM file_facts"), [])

    @unittest.skipUnless(shutil.which("ffprobe"), "ffprobe optional")
    def test_real_audio_probe_and_read_only_queries(self):
        before = self.file.read_bytes()
        self.p.enqueue("probe", file_ids=[self.file_id])
        result = self.p.run()
        self.assertTrue(result["complete"], self.p.list())
        fact = self.p.facts(self.file_id)["facts"][0]
        self.assertIn("pcm_s16le", fact["data"]["summary"]["audio_codecs"])
        self.assertEqual(self.file.read_bytes(), before)
        sql = execute_sql(
            self.database,
            "SELECT operation,current FROM catalog_facts WHERE profile=:profile",
        )
        self.assertEqual(sql["rows"], [["probe", 1]])
        result = execute_graphql(
            self.database,
            "{files {nodes {id facts}} jobs {nodes pageInfo {hasNextPage}}}",
        )
        self.assertNotIn("errors", result, result)
        self.assertEqual(
            result["data"]["files"]["nodes"][0]["facts"][0]["operation"], "probe"
        )

    @unittest.skipUnless(shutil.which("ffmpeg"), "ffmpeg optional")
    def test_real_decode(self):
        self.p.enqueue("decode", file_ids=[self.file_id])
        self.assertTrue(self.p.run()["complete"], self.p.list())

    def test_missing_tool_and_subprocess_limits(self):
        with patch("catabolic.processing.shutil.which", return_value=None):
            self.p.enqueue("probe", file_ids=[self.file_id])
            self.assertEqual(self.p.run()["counts"], {"unsupported": 1})
        with self.assertRaises(ProcessingFailure) as timeout:
            command_output(
                [sys.executable, "-c", "import time;time.sleep(10)"], timeout=0.05
            )
        self.assertEqual(timeout.exception.state, "timeout")
        with self.assertRaises(ProcessingFailure):
            command_output([sys.executable, "-c", 'print("x"*100000)'], maximum=100)
        cancel = threading.Event()
        cancel.set()
        with self.assertRaises(ProcessingFailure) as cancelled:
            command_output(
                [sys.executable, "-c", "import time;time.sleep(10)"], cancel=cancel
            )
        self.assertEqual(cancelled.exception.state, "cancelled")

    def test_subtitle_discovery_and_indexed_search(self):
        item = self.item()
        self.app.media.associate(self.file_id, item)
        subtitle = self.source / "Example.en.forced.srt"
        subtitle.write_text(
            "1\n00:00:01,500 --> 00:00:03,000\nThe blue planet\n\n", encoding="utf-8"
        )
        self.app.scan()
        discovered = self.c.sidecars(apply=True)["candidates"]
        self.assertEqual(len(discovered), 1)
        self.assertTrue(discovered[0]["payload"]["metadata"]["forced"])
        self.assertEqual(discovered[0]["payload"]["metadata"]["language"], "en")
        self.c.decide(discovered[0]["proposal"], accept=True, actor="test")
        subtitle_id = discovered[0]["file_id"]
        self.p.enqueue("text", file_ids=[subtitle_id])
        self.p.run()
        hits = self.p.search("BLUE planet")["hits"]
        self.assertEqual(len(hits), 1)
        self.assertEqual(hits[0]["locator"]["seconds"], 1.5)
        subtitle.write_text("new words", encoding="utf-8")
        self.app.scan()
        self.assertFalse(self.p.search("blue")["hits"][0]["current"])
        self.p.enqueue("text", file_ids=[subtitle_id])
        self.p.run()
        self.assertEqual(self.p.search("blue")["hits"], [])

    def test_completeness_uses_explicit_expected_snapshot(self):
        item = self.item()
        collection = self.app.put_item(
            "album", {"fixture": "album"}, {"title": "Album"}
        )["id"]
        definition = {
            "source": "fixture",
            "edition": "original",
            "ordering": "disc-track",
            "complete": True,
            "members": [
                {"key": "1-1", "item_id": item},
                {"key": "1-2", "identity": {"fixture": "missing"}},
            ],
        }
        expected = self.c.expected_put(collection, definition)
        states = [m["status"] for m in self.c.completeness(expected["id"])["members"]]
        self.assertEqual(states, ["unidentified", "missing"])
        self.app.media.associate(self.file_id, item)
        self.assertEqual(
            self.c.completeness(expected["id"])["members"][0]["status"], "present"
        )
        self.file.unlink()
        self.app.scan()
        self.assertEqual(
            self.c.completeness(expected["id"])["members"][0]["status"], "unavailable"
        )
        self.assertEqual(
            self.c.expected_put(collection, definition)["id"], expected["id"]
        )

    def test_copy_policy_ties_and_layout_selection(self):
        item = self.item()
        self.app.media.associate(self.file_id, item)
        copy = self.source / "copy.wav"
        copy.write_bytes(self.file.read_bytes() + b"large")
        self.app.scan()
        other = next(
            r["id"] for r in self.app.files()["files"] if r["id"] != self.file_id
        )
        self.app.media.associate(other, item)
        selection = CopySelection(self.app)
        selection.put("global", {"prefer": []})
        self.assertFalse(selection.plan("global")["safe"])
        selection.put("global", {"prefer": [{"field": "size", "order": "desc"}]})
        self.assertEqual(selection.plan("global")["decisions"][0]["selected"], other)
        layouts = Layouts(self.app)
        layouts.put(
            "test",
            {"version": 1, "rules": [{"name": "all", "path": "{item.id}/{file.name}"}]},
        )
        result = layouts.run("test", "global", apply=True)
        self.assertTrue(result["safe"], result)
        self.assertEqual(
            len(self.store.rows("SELECT * FROM mappings WHERE active=1")), 1
        )

    def test_provider_records_candidates_without_accepting(self):
        response = {
            "results": [{"id": 123, "title": "Example", "release_date": "2020-01-01"}],
            "total_pages": 1,
        }
        with (
            patch.dict(os.environ, {"TMDB_TOKEN": "secret-value"}),
            patch(
                "catabolic.network_adapters.request",
                return_value=json.dumps(response).encode(),
            ) as request,
        ):
            result = tmdb_candidates(
                self.app, self.file_id, "Example", year=2020, apply=True
            )
        self.assertEqual(len(result["candidates"]), 1)
        self.assertNotIn("secret-value", encode(result))
        self.assertEqual(self.store.rows("SELECT * FROM items"), [])
        self.assertEqual(self.c.list()["proposals"][0]["state"], "pending")
        self.assertTrue(
            request.call_args.kwargs["headers"]["Authorization"].startswith("Bearer ")
        )

    def test_refresh_retry_is_independent_of_links(self):
        refresh = Refresh(self.app)
        refresh.configure("global", "http://localhost:8096", "TEST_JELLYFIN_TOKEN")
        self.assertEqual(refresh.after_sync({"healthy": True, "applied": []}), [])
        from catabolic.network_adapters import record_output_change

        with self.store.transaction() as db:
            record_output_change(db, "default", "global")
        event = refresh.after_sync({"healthy": True})[0]
        with (
            patch.dict(os.environ, {"TEST_JELLYFIN_TOKEN": "secret"}),
            patch(
                "catabolic.network_adapters.request",
                side_effect=CatabolicError("HTTP request failed with status 503"),
            ),
        ):
            self.assertFalse(refresh.run()["complete"])
        with (
            patch.dict(os.environ, {"TEST_JELLYFIN_TOKEN": "secret"}),
            patch("catabolic.network_adapters.request", return_value=b"") as request,
        ):
            self.assertTrue(refresh.run()["complete"])
            self.assertTrue(request.call_args.args[0].endswith("/Library/Refresh"))
        self.assertEqual(refresh.list()["events"][0]["attempts"], 2)
        self.assertEqual(refresh.list()["events"][0]["id"], event)
        self.assertNotIn("secret", encode(refresh.list()))
        self.assertEqual(list(self.output.iterdir()), [])

    def test_cli_enqueue_run_and_results(self):
        self.store.close()
        env = {
            **os.environ,
            "PYTHONPATH": str(Path(__file__).resolve().parents[1] / "src"),
        }

        def cli(*args):
            result = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "catabolic",
                    "--db",
                    str(self.database),
                    "--json",
                    *args,
                ],
                cwd=self.root,
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr or result.stdout)
            return json.loads(result.stdout)

        self.assertEqual(
            len(cli("process", "enqueue", "hash", "--file-id", self.file_id)["queued"]),
            1,
        )
        self.assertEqual(cli("process", "run")["counts"], {"complete": 1})
        self.assertEqual(
            cli("process", "facts", self.file_id)["facts"][0]["operation"], "hash"
        )

    def test_proposal_graphql_pages_bind_filter_context(self):
        for index in range(3):
            self.c.put(
                self.file_id,
                {
                    "item": {
                        "kind": "track",
                        "identities": {"fixture": str(index)},
                        "metadata": {"title": str(index)},
                    }
                },
            )
        first = execute_graphql(
            self.database,
            '{proposals(first:1,state:"pending") {nodes pageInfo {endCursor hasNextPage}}}',
        )
        self.assertNotIn("errors", first, first)
        cursor = first["data"]["proposals"]["pageInfo"]["endCursor"]
        next_page = execute_graphql(
            self.database,
            'query($after:String){proposals(first:1,state:"pending",after:$after) {nodes pageInfo {hasNextPage}}}',
            variables=json.dumps({"after": cursor}),
        )
        self.assertNotIn("errors", next_page, next_page)
        wrong = execute_graphql(
            self.database,
            'query($after:String){proposals(first:1,state:"rejected",after:$after) {nodes}}',
            variables=json.dumps({"after": cursor}),
        )
        self.assertIn("errors", wrong)

    def test_signature_probe_is_small_and_tool_independent(self):
        self.p.enqueue("sniff", file_ids=[self.file_id])
        with patch(
            "catabolic.processing.command_output",
            side_effect=AssertionError("unexpected subprocess"),
        ):
            self.assertTrue(self.p.run()["complete"])
        data = self.p.facts(self.file_id)["facts"][0]["data"]
        self.assertEqual(data["mime"], "audio/wav")
        self.assertLessEqual(data["bytes_read"], 4096)

    def test_hardlink_occurrences_reuse_a_physical_result(self):
        os.link(self.file, self.source / "linked.wav")
        self.app.scan()
        self.p.enqueue("hash")
        result = self.p.run()
        self.assertTrue(result["complete"])
        self.assertEqual(result["reused_physical_results"], 1)
        self.assertEqual(len(self.store.rows("SELECT * FROM content_baselines")), 2)

    def test_watcher_settles_and_does_not_repeat_failed_inputs(self):
        from catabolic.watching import cycle

        with patch("catabolic.watching.time.time", return_value=1000):
            first = cycle(self.app, operation="sniff", settle=30)
        self.assertEqual(first["queued"], 0)
        with patch("catabolic.watching.time.time", return_value=1031):
            second = cycle(self.app, operation="sniff", settle=30)
        self.assertEqual(second["queued"], 1)
        with patch("catabolic.watching.time.time", return_value=1062):
            third = cycle(self.app, operation="sniff", settle=30)
        self.assertEqual(third["queued"], 0)
        self.assertEqual(third["cached"], 1)
        self.file.write_bytes(b"changed")
        self.app.scan()
        with patch("catabolic.watching.time.time", return_value=1063):
            self.assertEqual(cycle(self.app, operation="sniff", settle=30)["queued"], 0)

    def test_crashed_processing_runner_resumes_real_claim(self):
        self.p.enqueue("hash", file_ids=[self.file_id])
        self.store.close()
        script = """import os,sys
from catabolic.store import Store
from catabolic.app import Application
from catabolic.processing import Processing
from unittest.mock import patch
with Store(sys.argv[1],writable=True) as store:
    with patch('catabolic.processing.process',side_effect=lambda *args:os._exit(77)):
        Processing(Application(store)).run()
"""
        env = {
            **os.environ,
            "PYTHONPATH": str(Path(__file__).resolve().parents[1] / "src"),
        }
        result = subprocess.run(
            [sys.executable, "-c", script, str(self.database)],
            env=env,
            cwd=self.root,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 77, result.stderr)
        self.store = Store(self.database, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.p = Processing(self.app)
        self.assertEqual(self.p.list()["jobs"][0]["state"], "running")
        self.assertTrue(self.p.run()["complete"])
        self.assertEqual(self.p.list()["jobs"][0]["attempts"], 2)

    def test_scan_stages_bounded_batches_and_caps_errors(self):
        from catabolic.filesystem import root_handle, walk_files
        from catabolic.scan_staging import ScanStaging

        with ScanStaging() as stage:
            for index in range(1200):
                stage.append(
                    {
                        "path": str(index),
                        "size": 1,
                        "mtime_ns": 1,
                        "device": 1,
                        "inode": index,
                    }
                )
                self.assertLess(len(stage.buffer), 500)
            self.assertEqual(sum(1 for _ in stage), 1200)
        for index in range(1100):
            (self.source / str(index)).touch()
        real_stat = os.stat

        def failing(path, *args, **kwargs):
            if kwargs.get("dir_fd") is not None:
                raise PermissionError("fixture denied")
            return real_stat(path, *args, **kwargs)

        with (
            root_handle(self.app.binding("source", "media")) as fd,
            patch("catabolic.filesystem.os.stat", side_effect=failing),
        ):
            observed, errors = walk_files(fd)
        self.assertEqual(observed, [])
        self.assertEqual(len(errors), 1001)
        self.assertIn("omitted", errors[-1])

    def test_sql_processing_selection_and_probe_template(self):
        selection = self.root / "selection.json"
        selection.write_text(
            json.dumps(
                {
                    "language": "sql",
                    "query": "SELECT file_id FROM catalog_files WHERE profile=:profile",
                    "profile": "default",
                }
            )
        )
        from catabolic.cli import parser
        from catabolic.enrichment_cli import dispatch

        args = parser().parse_args(
            ["process", "enqueue", "sniff", "--selection", str(selection)]
        )
        self.assertEqual(len(dispatch(self.app, args)["queued"]), 1)
        self.p.run()
        item = self.item()
        self.app.media.associate(self.file_id, item)
        layouts = Layouts(self.app)
        layouts.put(
            "probe-test",
            {
                "version": 1,
                "rules": [{"name": "all", "path": "{probe.height}/{file.name}"}],
            },
        )
        result = layouts.run("probe-test", "global")
        self.assertFalse(result["safe"])

    def test_refresh_dirty_survives_recovery_and_unchanged_sync(self):
        refresh = Refresh(self.app)
        refresh.configure("global", "http://localhost:8096", "TEST_TOKEN")
        item = self.item()
        self.app.put_mapping("global", self.file_id, item, "Example.wav")
        from catabolic.reconcile import Reconciler

        reconciler = Reconciler(self.app)

        def stop(operation):
            if operation["kind"] == "create":
                raise RuntimeError("injected after filesystem")

        with self.assertRaises(RuntimeError):
            reconciler.apply(after_filesystem=stop)
        self.assertEqual(refresh.list()["events"], [])
        reconciler.recover()
        result = reconciler.apply()
        self.assertEqual(len(result["refresh_events"]), 1)
        self.assertNotIn("refresh_events", reconciler.apply())
        self.assertEqual(len(refresh.list()["events"]), 1)

    @unittest.skipUnless(shutil.which("ffprobe"), "ffprobe optional")
    def test_image_probe_and_successful_probe_layout(self):
        import struct
        import zlib

        def chunk(kind, data):
            return (
                struct.pack(">I", len(data))
                + kind
                + data
                + struct.pack(">I", zlib.crc32(kind + data))
            )

        data = (
            b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", 32, 16, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress((b"\0" + b"\xff\0\0" * 32) * 16))
            + chunk(b"IEND", b"")
        )
        image = self.source / "sample.png"
        image.write_bytes(data)
        self.app.scan()
        identifier = next(
            r["id"] for r in self.app.files()["files"] if r["path"] == "sample.png"
        )
        self.p.enqueue("probe", file_ids=[identifier])
        self.assertTrue(self.p.run()["complete"], self.p.list())
        fact = self.p.facts(identifier)["facts"][0]
        self.assertEqual(fact["data"]["summary"]["height"], 16)
        item = self.app.put_item("photo", {"fixture": "photo"}, {"title": "Photo"})[
            "id"
        ]
        self.app.media.associate(identifier, item)
        layouts = Layouts(self.app)
        layouts.put(
            "image",
            {
                "version": 1,
                "rules": [
                    {
                        "name": "photos",
                        "when": {"kinds": ["photo"]},
                        "path": "{probe.height}/{file.name}",
                    }
                ],
            },
        )
        result = layouts.run("image", "global", apply=True)
        self.assertTrue(result["safe"], result)
        self.assertEqual(
            self.store.rows("SELECT path FROM mappings")[0]["path"], "16/sample.png"
        )

    def test_workers_share_physical_limits_and_queries_remain_available(self):
        import time

        from catabolic.processing import process

        other = self.root / "other"
        other.mkdir()
        (other / "another.wav").write_bytes(self.file.read_bytes())
        self.app.bind("source", "other", str(other))
        self.app.scan()
        self.p.enqueue("hash")
        active = peak = 0
        lock = threading.Lock()

        def measured(job, cancel):
            nonlocal active, peak
            with lock:
                active += 1
                peak = max(active, peak)
            try:
                # A separate query connection remains usable while workers do I/O.
                value = execute_sql(
                    self.database,
                    "SELECT count(*) FROM catalog_files WHERE profile=:profile",
                )
                self.assertEqual(value["rows"], [[2]])
                time.sleep(0.03)
                return process(job, cancel)
            finally:
                with lock:
                    active -= 1

        with patch("catabolic.processing.process", side_effect=measured):
            result = self.p.run(
                workers=4,
                per_device=1,
                storage_groups={"media": "group-a", "other": "group-b"},
            )
        self.assertTrue(result["complete"])
        self.assertEqual(peak, 1)

    def test_failed_job_is_cached_until_explicit_retry(self):
        self.p.enqueue("text", file_ids=[self.file_id])
        self.assertEqual(self.p.run()["counts"], {"unsupported": 1})
        self.assertEqual(
            len(self.p.enqueue("text", file_ids=[self.file_id])["cached"]), 1
        )
        self.assertEqual(self.p.run()["processed"], 0)
        self.assertEqual(
            len(
                self.p.enqueue("text", file_ids=[self.file_id], refresh=True)["queued"]
            ),
            1,
        )

    def test_copy_policy_keeps_only_attached_selected_sidecars(self):
        item = self.item()
        self.app.media.associate(self.file_id, item)
        second = self.source / "Other.wav"
        second.write_bytes(self.file.read_bytes() + b"more")
        for name in ("Example.en.srt", "Other.en.srt"):
            (self.source / name).write_text("1\n00:00:01,000 --> 00:00:02,000\nHello\n")
        self.app.scan()
        other = next(
            r["id"] for r in self.app.files()["files"] if r["path"] == "Other.wav"
        )
        self.app.media.associate(other, item)
        for candidate in self.c.sidecars(apply=True)["candidates"]:
            self.c.decide(candidate["proposal"], accept=True, actor="test")
        selector = CopySelection(self.app)
        selector.put("global", {"prefer": [{"field": "size", "order": "desc"}]})
        ids = selector.plan("global")["association_ids"]
        selected = self.store.rows(
            "SELECT f.path FROM item_files a JOIN files f ON f.id=a.file_id WHERE a.id IN ("
            + ",".join("?" for _ in ids)
            + ")",
            tuple(ids),
        )
        self.assertEqual({r["path"] for r in selected}, {"Other.wav", "Other.en.srt"})

    def test_real_local_refresh_http_contract_and_redirect_refusal(self):
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        from catabolic.network_adapters import record_output_change, request

        calls = []

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                calls.append((self.path, self.headers.get("X-Emby-Token")))
                self.send_response(204)
                self.end_headers()

            def do_GET(self):
                if self.path == "/redirect":
                    self.send_response(302)
                    self.send_header("Location", "/unexpected")
                    self.end_headers()
                else:
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(b"x" * 1024)

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(thread.join, 2)
        self.addCleanup(server.shutdown)
        endpoint = f"http://127.0.0.1:{server.server_port}"
        refresh = Refresh(self.app)
        refresh.configure("global", endpoint, "TEST_JELLYFIN_TOKEN")
        with self.store.transaction() as db:
            record_output_change(db, "default", "global")
        refresh.after_sync({"healthy": True})
        with patch.dict(os.environ, {"TEST_JELLYFIN_TOKEN": "fixture-only-token"}):
            self.assertTrue(refresh.run()["complete"])
        self.assertEqual(calls, [("/Library/Refresh", "fixture-only-token")])
        with self.assertRaisesRegex(CatabolicError, "redirect"):
            request(
                endpoint + "/redirect", headers={"Authorization": "fixture-only-token"}
            )
        with self.assertRaisesRegex(CatabolicError, "byte limit"):
            request(endpoint + "/large", maximum=20)

    def test_known_mismatch_invalidates_facts_even_with_preserved_mtime(self):
        self.p.enqueue("hash", file_ids=[self.file_id])
        self.p.run()
        before = self.file.stat()
        data = bytearray(self.file.read_bytes())
        data[-1] = 1
        self.file.write_bytes(data)
        os.utime(self.file, ns=(before.st_atime_ns, before.st_mtime_ns))
        self.app.scan()
        self.p.enqueue("verify", file_ids=[self.file_id])
        self.assertEqual(self.p.run()["counts"], {"mismatch": 1})
        fact = self.p.facts(self.file_id)["facts"][0]
        self.assertFalse(fact["current"])
        self.assertEqual(fact["status"], "invalidated")
        self.assertEqual(
            execute_sql(
                self.database,
                "SELECT current FROM catalog_facts WHERE profile=:profile",
            )["rows"],
            [[0]],
        )

    def test_refresh_configuration_is_profile_specific(self):
        default = Refresh(self.app)
        default.configure("global", "http://localhost:8096", "DEFAULT_TOKEN")
        self.app.add_profile("remote")
        remote = Application(self.store, "remote")
        Refresh(remote).configure("global", "http://localhost:9096", "REMOTE_TOKEN")
        from catabolic.network_adapters import record_output_change

        with self.store.transaction() as db:
            record_output_change(db, "default", "global")
        default.after_sync({"healthy": True})
        self.assertEqual(
            default.list()["events"][0]["target"]["endpoint"], "http://localhost:8096"
        )
        self.assertEqual(Refresh(remote).list()["events"], [])


class EnrichmentMigrationTest(unittest.TestCase):
    def test_schema_five_upgrade_preserves_catalog_and_backup(self):
        from catabolic.migration import load_migrations, upgrade_database
        from tests.test_migrations import contents, create_legacy

        with tempfile.TemporaryDirectory() as directory:
            path = create_legacy(Path(directory))
            upgrade_database(path, migrations=load_migrations()[:5])
            before = contents(path)
            result = upgrade_database(path)
            self.assertEqual([s["version"] for s in result["applied"]], [6])
            self.assertEqual(contents(Path(result["backup"])), before)
            self.assertEqual(
                {k: v for k, v in contents(path).items() if k in before}, before
            )
            with Store(path) as store:
                self.assertEqual(store.schema_version, 6)
