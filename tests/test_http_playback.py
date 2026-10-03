# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Real FFmpeg early HLS delivery, shared work and authorization/lifetime boundaries."""

import hashlib
import json
import os
import signal
import subprocess
import threading
import time
import unittest

from catabolic import playback_cache, playback_worker, rendering
from catabolic.access import authenticate, issue, revoke
from catabolic.app import Application
from catabolic.artifacts import Artifacts
from catabolic.content_access import revision_of
from catabolic.http_worker_local import hls_available, run
from catabolic.store import Store, encode
from tests import test_http_renditions as fixtures


@unittest.skipUnless(
    not getattr(fixtures.RenditionHTTPTest, "__unittest_skip__", False),
    "optional HTTP/FFmpeg dependencies",
)
class PlaybackTest(unittest.TestCase):
    def setUp(self):
        fixtures.RenditionHTTPTest.setUp(self)
        with Store(self.database, writable=True) as store:
            artifacts = Artifacts(Application(store))
            recipe = artifacts.recipe(
                "hls",
                "h264-720p",
                {
                    "encoder_speed": "veryfast",
                    "max_output_bytes": 16777216,
                    "reserve_bytes": 0,
                },
            )
            self.recipe = recipe["id"]
            self.caps = rendering.capabilities()
            self.caps["playback_hls"] = hls_available(self.caps)
            with store.transaction() as db:
                db.execute(
                    "UPDATE api_operations SET recipe_id=? WHERE id=?",
                    (self.recipe, self.operation),
                )
                db.execute(
                    "UPDATE api_worker_status SET capabilities=?", (encode(self.caps),)
                )

    def create(self, index=0, key=None, **body):
        headers = dict(self.headers[index])
        if key:
            headers["Idempotency-Key"] = key
        return self.client.post(
            "/v1/playback-sessions", headers=headers, json={**self.body, **body}
        )

    def job(self, identifier):
        with Store(self.database) as store:
            return store.rows(
                "SELECT j.* FROM api_playback_jobs j JOIN api_playback_sessions s ON s.job_id=j.id WHERE s.id=?",
                (identifier,),
            )[0]

    def long_source(self):
        source = self.root / "source/sample.mkv"
        source.unlink()
        subprocess.run(
            [
                "ffmpeg",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x120:rate=10:duration=10",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=10",
                "-c:v",
                "mpeg4",
                "-c:a",
                "aac",
                str(source),
            ],
            check=True,
            capture_output=True,
        )
        with Store(self.database, writable=True) as store:
            Application(store).scan()
            self.body["source_revision"] = revision_of(store, "default", self.file)
        return hashlib.sha256(source.read_bytes()).hexdigest()

    def start(self, identifier):
        job = self.job(identifier)
        failures = []

        def work():
            try:
                with Store(self.database, writable=True) as store:

                    def paced(argv):
                        index = argv.index("-i")
                        return argv[:index] + ["-re"] + argv[index:]

                    playback_worker.execute(store, job, self.caps, _command=paced)
            except BaseException as exc:
                failures.append(exc)

        thread = threading.Thread(target=work)
        thread.start()

        def retire():
            # Never leave an encoder writing into a fixture after test teardown.
            if thread.is_alive():
                with Store(self.database, writable=True) as store:
                    with store.transaction() as db:
                        db.execute("UPDATE api_playback_sessions SET cancelled=1")
            thread.join(15)
            self.assertFalse(thread.is_alive())
            self.assertEqual(failures, [])

        self.addCleanup(retire)
        return thread

    def wait_playing(self, identifier):
        deadline = time.monotonic() + 12
        while time.monotonic() < deadline:
            response = self.client.get(
                f"/v1/playback-sessions/{identifier}", headers=self.headers[0]
            )
            if response.status_code == 200 and response.json()["playlist_path"]:
                return response.json()
            if response.status_code == 200 and response.json()["state"] == "failed":
                self.fail(response.text)
            time.sleep(0.1)
        self.fail("first segment did not become playable")

    def test_early_delivery_shared_work_and_cancellation(self):
        original = self.long_source()
        first = self.create()
        self.assertEqual(first.status_code, 202, first.text)
        identifier = first.json()["session_id"]
        second = self.create(1)
        self.assertEqual(second.status_code, 202, second.text)
        other = second.json()["session_id"]
        self.assertEqual(self.job(identifier)["id"], self.job(other)["id"])
        self.assertEqual(self.create().json()["session_id"], identifier)
        self.assertEqual(self.create(ttl=60).status_code, 409)
        self.assertEqual(
            self.client.get(
                first.json()["status_url"], headers=self.headers[1]
            ).status_code,
            404,
        )
        before = self.client.get(
            f"/v1/playback-sessions/{identifier}/index.m3u8", headers=self.headers[0]
        )
        self.assertEqual(before.status_code, 409)
        thread = self.start(identifier)
        report = self.wait_playing(identifier)
        self.assertTrue(thread.is_alive(), "must serve before encoder completes")
        self.assertEqual(report["state"], "playing")
        playlist = self.client.get(report["playlist_path"], headers=self.headers[0])
        self.assertEqual(playlist.status_code, 200, playlist.text)
        self.assertNotIn("#EXT-X-ENDLIST", playlist.text)
        self.assertNotIn("/dev/fd", playlist.text)
        uri = next(
            line
            for line in playlist.text.splitlines()
            if line and not line.startswith("#")
        )
        segment_url = report["playlist_path"].rsplit("/", 1)[0] + "/" + uri
        segment = self.client.get(segment_url, headers=self.headers[0])
        self.assertEqual(segment.status_code, 200, segment.text)
        self.assertEqual(segment.headers["content-type"], "video/mp2t")
        self.assertEqual(segment.content[0], 0x47)
        self.assertEqual(self.client.get(segment_url).status_code, 401)
        head = self.client.head(segment_url, headers=self.headers[0])
        self.assertEqual(head.status_code, 200)
        self.assertEqual(head.content, b"")
        self.assertEqual(int(head.headers["content-length"]), len(segment.content))
        self.assertEqual(
            self.client.get(
                segment_url, headers={**self.headers[0], "Range": "bytes=0-1"}
            ).status_code,
            416,
        )
        fixture = self.root / "segment.ts"
        fixture.write_bytes(segment.content)
        streams = json.loads(
            subprocess.check_output(
                ["ffprobe", "-v", "error", "-show_streams", "-of", "json", str(fixture)]
            )
        )["streams"]
        self.assertEqual([s["codec_name"] for s in streams], ["h264", "aac"])
        subprocess.run(
            ["ffmpeg", "-v", "error", "-i", str(fixture), "-f", "null", "-"],
            check=True,
            capture_output=True,
        )
        self.client.post(
            f"/v1/playback-sessions/{identifier}/cancel", headers=self.headers[0]
        )
        self.assertEqual(
            self.client.get(
                report["playlist_path"], headers=self.headers[0]
            ).status_code,
            410,
        )
        other_playlist = f"/v1/playback-sessions/{other}/index.m3u8"
        self.assertEqual(
            self.client.get(other_playlist, headers=self.headers[1]).status_code, 200
        )
        self.client.post(
            f"/v1/playback-sessions/{other}/cancel", headers=self.headers[1]
        )
        thread.join(5)
        self.assertFalse(thread.is_alive())
        self.assertEqual(self.job(identifier)["state"], "cancelled")
        self.assertEqual(self.job(identifier)["reserved_bytes"], 0)
        self.assertEqual(
            hashlib.sha256((self.root / "source/sample.mkv").read_bytes()).hexdigest(),
            original,
        )
        run(self.database, once=True)
        self.assertFalse(list((self.root / "generated").glob(".playback-*")))

    def test_worker_completion_cache_reuse_and_closed_segments_only(self):
        response = self.create()
        identifier = response.json()["session_id"]
        run(self.database, once=True)
        ready = self.client.get(response.json()["status_url"], headers=self.headers[0])
        self.assertEqual(ready.json()["state"], "ready", ready.text)
        playlist = self.client.get(
            ready.json()["playlist_path"], headers=self.headers[0]
        )
        self.assertIn("#EXT-X-ENDLIST", playlist.text)
        job = self.job(identifier)
        directory = self.root / "generated" / playback_cache.directory_name(job)
        self.assertEqual(directory.stat().st_mode & 0o777, 0o700)
        self.assertEqual(
            (directory / "segment-000000.ts").stat().st_mode & 0o777, 0o600
        )
        for name in (
            "segment-999999.ts",
            "segment-000000.ts.tmp",
            "index.m3u8",
            "secret",
        ):
            r = self.client.get(
                f"/v1/playback-sessions/{identifier}/segments/{name}/content",
                headers=self.headers[0],
            )
            self.assertEqual(r.status_code, 404, (name, r.text))
        replay = self.create(key="new-session")
        self.assertEqual(replay.status_code, 200, replay.text)
        self.assertEqual(self.job(replay.json()["session_id"])["id"], job["id"])
        with Store(self.database) as store:
            self.assertEqual(
                store.rows("SELECT count(*) AS n FROM processing_artifacts")[0]["n"], 0
            )

    def test_reuses_completed_catalog_rendition_without_worker(self):
        with Store(self.database, writable=True) as store:
            artifacts = Artifacts(Application(store))
            artifacts.enqueue(self.file, self.recipe, "generated", self.item)
            self.assertTrue(artifacts.run()["complete"])
            with store.transaction() as db:
                db.execute("DELETE FROM api_worker_status")
        response = self.create()
        self.assertEqual(response.status_code, 200, response.text)
        self.assertIsNone(response.json()["playlist_path"])
        content = self.client.get(
            response.json()["content_path"], headers=self.headers[0]
        )
        self.assertEqual(content.status_code, 200, content.text)
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT * FROM api_playback_jobs"), [])

    def test_direct_rendition_session_rejects_hls_paths(self):
        with Store(self.database, writable=True) as store:
            artifacts = Artifacts(Application(store))
            artifacts.enqueue(self.file, self.recipe, "generated", self.item)
            self.assertTrue(artifacts.run()["complete"])
        response = self.create()
        self.assertEqual(response.status_code, 200, response.text)
        base = response.json()["status_url"]
        for suffix in ("/index.m3u8", "/segments/segment-000000.ts/content"):
            for method in (self.client.get, self.client.head):
                self.assertEqual(
                    method(base + suffix, headers=self.headers[0]).status_code, 404
                )

    def test_direct_rendition_pins_source_ctime_and_rejects_stale_reuse(self):
        with Store(self.database, writable=True) as store:
            artifacts = Artifacts(Application(store))
            artifacts.enqueue(self.file, self.recipe, "generated", self.item)
            self.assertTrue(artifacts.run()["complete"])
        first = self.create()
        self.assertEqual(first.status_code, 200, first.text)
        source = self.root / "source/sample.mkv"
        before = source.stat()
        payload = bytearray(source.read_bytes())
        payload[-1] ^= 1
        source.write_bytes(payload)
        os.utime(source, ns=(before.st_atime_ns, before.st_mtime_ns))
        self.assertNotEqual(source.stat().st_ctime_ns, before.st_ctime_ns)
        self.assertEqual(
            self.client.get(
                first.json()["status_url"], headers=self.headers[0]
            ).status_code,
            409,
        )
        next_session = self.create(key="edited-source")
        self.assertEqual(next_session.status_code, 202, next_session.text)
        self.assertIsNone(next_session.json()["content_path"])

    def test_authorized_replay_refreshes_rotated_worker_credential(self):
        first = self.create()
        self.assertEqual(first.status_code, 202, first.text)
        identifier = first.json()["session_id"]
        with Store(self.database, writable=True) as store:
            old = authenticate(store, self.headers[0]["Authorization"].split()[1])
            replacement = issue(store, old.principal, [g["_id"] for g in old.grants])
            revoke(store, old.token["id"])
        self.headers[0]["Authorization"] = "Bearer " + replacement["token"]
        replay = self.create()
        self.assertEqual(replay.json()["session_id"], identifier)
        with Store(self.database) as store:
            session = store.rows(
                "SELECT token_id FROM api_playback_sessions WHERE id=?", (identifier,)
            )[0]
        self.assertEqual(session["token_id"], replacement["token_id"])
        run(self.database, once=True)
        ready = self.client.get(first.json()["status_url"], headers=self.headers[0])
        self.assertEqual(ready.json()["state"], "ready", ready.text)

    def test_expiry_and_cleanup_preserve_unrelated_files(self):
        response = self.create()
        identifier = response.json()["session_id"]
        run(self.database, once=True)
        job = self.job(identifier)
        unrelated = self.root / "generated/unrelated.txt"
        unrelated.write_text("keep")
        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                db.execute("UPDATE api_playback_sessions SET expires=0")
                db.execute("UPDATE api_playback_jobs SET updated_at=0")
            playback_cache.cleanup(store, "default")
        self.assertEqual(
            self.client.get(
                response.json()["status_url"], headers=self.headers[0]
            ).status_code,
            410,
        )
        self.assertFalse(
            (self.root / "generated" / playback_cache.directory_name(job)).exists()
        )
        self.assertEqual(unrelated.read_text(), "keep")

    def test_cleanup_advances_past_retired_cache_tombstones(self):
        for number in range(22):
            response = self.create(key=f"retired-{number}")
            self.assertEqual(response.status_code, 202, response.text)
            identifier = response.json()["session_id"]
            self.client.post(
                f"/v1/playback-sessions/{identifier}/cancel", headers=self.headers[0]
            )
            with Store(self.database, writable=True) as store:
                playback_cache.cleanup(store, "default")
        with Store(self.database) as store:
            self.assertEqual(
                store.rows(
                    "SELECT count(*) AS n FROM api_playback_jobs WHERE state='queued'"
                )[0]["n"],
                0,
            )

    def test_revocation_operation_disable_and_source_changes(self):
        response = self.create()
        run(self.database, once=True)
        path = self.client.get(
            response.json()["status_url"], headers=self.headers[0]
        ).json()["playlist_path"]
        with Store(self.database, writable=True) as store:
            revoke(store, self.headers[0]["Authorization"].split()[1].split(".")[0])
        self.assertEqual(
            self.client.get(path, headers=self.headers[0]).status_code, 401
        )
        second = self.create(1)
        other_path = second.json()["playlist_path"]
        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                db.execute("UPDATE api_operations SET enabled=0")
        self.assertEqual(
            self.client.get(other_path, headers=self.headers[1]).status_code, 403
        )
        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                db.execute("UPDATE api_operations SET enabled=1")
        with (self.root / "source/sample.mkv").open("ab") as stream:
            stream.write(b"changed")
        self.assertEqual(
            self.client.get(other_path, headers=self.headers[1]).status_code, 409
        )

    def test_storage_limits_and_timeout_are_terminal(self):
        for option in ({"max_output_bytes": 1024}, {"timeout": 1}):
            with self.subTest(option=option):
                with Store(self.database, writable=True) as store:
                    artifacts = Artifacts(Application(store))
                    recipe = artifacts.recipe(
                        "limited", "h264-720p", {"reserve_bytes": 0, **option}
                    )
                    with store.transaction() as db:
                        db.execute(
                            "UPDATE api_operations SET recipe_id=?", (recipe["id"],)
                        )
                if "timeout" in option:
                    self.long_source()
                    with Store(self.database, writable=True) as store:
                        with store.transaction() as db:
                            db.execute(
                                "INSERT INTO api_worker_status VALUES (?,?,?,?)",
                                (
                                    "default",
                                    os.getpid(),
                                    encode(self.caps),
                                    time.time(),
                                ),
                            )
                response = self.create(key=str(option))
                self.assertEqual(response.status_code, 202, response.text)
                identifier = response.json()["session_id"]
                if "timeout" in option:
                    thread = self.start(identifier)
                    thread.join(5)
                else:
                    run(self.database, once=True)
                job = self.job(identifier)
                self.assertEqual(job["state"], "failed", job)
                self.assertEqual(job["reserved_bytes"], 0)
                self.assertIsNone(
                    self.client.get(
                        response.json()["status_url"], headers=self.headers[0]
                    ).json()["playlist_path"]
                )

    def test_missing_worker_read_only_and_profile_rejection(self):
        from fastapi.testclient import TestClient

        from catabolic.http.app import create_app

        with TestClient(create_app(self.database)) as client:
            self.assertEqual(
                client.post(
                    "/v1/playback-sessions", headers=self.headers[0], json=self.body
                ).status_code,
                403,
            )
        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                db.execute("DELETE FROM api_worker_status")
        self.assertEqual(self.create().status_code, 409)
        with Store(self.database, writable=True) as store:
            recipe = Artifacts(Application(store)).recipe("unsupported", "thumbnail")
            with store.transaction() as db:
                db.execute("UPDATE api_operations SET recipe_id=?", (recipe["id"],))
        rejected = self.create()
        self.assertEqual(rejected.status_code, 409)
        self.assertEqual(rejected.json()["code"], "playback_profile_unsupported")

    def test_cache_symlink_and_worker_recovery(self):
        response = self.create()
        identifier = response.json()["session_id"]
        run(self.database, once=True)
        job = self.job(identifier)
        directory = self.root / "generated" / playback_cache.directory_name(job)
        segment = directory / "segment-000000.ts"
        segment.unlink()
        segment.symlink_to(self.root / "source/sample.mkv")
        url = f"/v1/playback-sessions/{identifier}/segments/segment-000000.ts/content"
        self.assertEqual(self.client.get(url, headers=self.headers[0]).status_code, 409)
        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                db.execute("UPDATE api_playback_jobs SET state='running'")
            playback_worker.recover(store, "default")
        self.assertEqual(self.job(identifier)["state"], "failed")

    def test_revoked_last_viewer_stops_running_encoder(self):
        self.long_source()
        response = self.create()
        identifier = response.json()["session_id"]
        thread = self.start(identifier)
        self.wait_playing(identifier)
        with Store(self.database, writable=True) as store:
            revoke(store, self.headers[0]["Authorization"].split()[1].split(".")[0])
        thread.join(5)
        self.assertFalse(thread.is_alive())
        self.assertEqual(self.job(identifier)["state"], "cancelled")

    def test_reservations_account_for_durable_jobs_and_shared_sessions(self):
        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                # Deliberately unavailable capacity; don't allocate any media bytes.
                db.execute("UPDATE api_playback_jobs SET reserved_bytes=0")
            from catabolic.rendition_requests import admit

            access = authenticate(store, self.headers[0]["Authorization"].split()[1])
            admit(access, self.body, "durable-reservation", self.caps)
            device = os.stat(self.root / "generated").st_dev
            baseline = playback_cache.reserved_bytes(store, device)
        first = self.create()
        second = self.create(1)
        self.assertEqual(first.status_code, 202, first.text)
        self.assertEqual(second.status_code, 202, second.text)
        with Store(self.database) as store:
            self.assertEqual(
                playback_cache.reserved_bytes(store, device), baseline + 16777216
            )

    def test_completed_multisegment_output_decodes_and_seeks(self):
        self.long_source()
        response = self.create()
        run(self.database, once=True)
        status = self.client.get(
            response.json()["status_url"], headers=self.headers[0]
        ).json()
        self.assertEqual(status["state"], "ready", status)
        self.assertEqual(status["segment_count"], 5)
        self.assertAlmostEqual(status["available_seconds"], 10, delta=0.1)
        manifest = self.client.get(
            status["playlist_path"], headers=self.headers[0]
        ).text
        segments = []
        base = status["playlist_path"].rsplit("/", 1)[0]
        for uri in manifest.splitlines():
            if uri and not uri.startswith("#"):
                result = self.client.get(base + "/" + uri, headers=self.headers[0])
                self.assertEqual(result.status_code, 200)
                segments.append(result.content)
        output = self.root / "completed.ts"
        output.write_bytes(b"".join(segments))
        subprocess.run(
            ["ffmpeg", "-v", "error", "-xerror", "-i", str(output), "-f", "null", "-"],
            check=True,
            capture_output=True,
        )
        subprocess.run(
            [
                "ffmpeg",
                "-v",
                "error",
                "-ss",
                "6",
                "-i",
                str(output),
                "-t",
                "1",
                "-f",
                "null",
                "-",
            ],
            check=True,
            capture_output=True,
        )


class EncoderLifetimeTest(unittest.TestCase):
    def test_orphan_encoder_stops_when_worker_is_killed(self):
        import sys
        import tempfile
        from pathlib import Path

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            child_script = root / "encoder.py"
            child_script.write_text(
                "import os,time\nopen('child.pid','w').write(str(os.getpid()))\n"
                "f=open('progress','ab',buffering=0)\nwhile True:\n f.write(b'x')\n time.sleep(0.02)\n"
            )
            # The simulated worker owns the real lifetime wrapper/process group.
            # Its child continuously records progress until that worker disappears.
            script = """
import os, sys
from catabolic.process_runner import command_output
directory=os.open(sys.argv[1],os.O_RDONLY|os.O_DIRECTORY)
source=os.open(os.devnull,os.O_RDONLY)
command_output([sys.executable,'-m','catabolic.playback_process',str(os.getpid()),f'{source},{directory}',sys.executable,sys.argv[2]],pass_fds=(source,directory),timeout=30)
"""
            supervisor = subprocess.Popen(
                [sys.executable, "-c", script, directory, str(child_script)],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            child = None
            try:
                deadline = time.monotonic() + 5
                while not (root / "progress").exists() and time.monotonic() < deadline:
                    time.sleep(0.02)
                self.assertTrue((root / "progress").exists())
                child = int((root / "child.pid").read_text())
                supervisor.kill()
                supervisor.wait(timeout=5)
                time.sleep(0.4)
                size = (root / "progress").stat().st_size
                time.sleep(0.2)
                self.assertEqual((root / "progress").stat().st_size, size)
            finally:
                if supervisor.poll() is None:
                    supervisor.kill()
                supervisor.wait(timeout=5)
                if child:
                    try:
                        os.kill(child, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
