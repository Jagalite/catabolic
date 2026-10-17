# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native session/worker/cache qualification against Python HTTP and real FFmpeg."""

import hashlib
import json
import os
import shutil
import signal
import subprocess
import sys
import time
import unittest
from pathlib import Path

from catabolic.access import authenticate, grant_put, issue, revoke
from catabolic.app import Application
from catabolic.artifacts import Artifacts
from catabolic.store import Store, encode
from tests import test_http_playback as reference
from tests.test_rust_migration import BINARY


@unittest.skipUnless(
    sys.platform.startswith(("linux", "darwin")) and shutil.which("ffmpeg"),
    "native playback supervision (Linux or macOS) and FFmpeg",
)
class NativePlaybackTest(unittest.TestCase):
    def setUp(self):
        self.assertTrue(
            BINARY.is_file(), "build native binary before playback qualification"
        )
        reference.PlaybackTest.setUp(self)
        self.env = dict(os.environ)

    def invoke(self, surface, operation, *, index=0, ok=True, **request):
        env = {
            **self.env,
            "CATABOLIC_API_TOKEN": self.headers[index]["Authorization"].split()[1],
        }
        command = [
            str(BINARY),
            "--db",
            str(self.database),
            "operation",
            surface,
            operation,
            "--options",
            json.dumps(request),
        ]
        deadline = time.monotonic() + 8
        while True:
            result = subprocess.run(
                command, env=env, capture_output=True, text=True, timeout=30
            )
            if (
                result.returncode
                and "another Catabolic writer" in result.stderr
                and time.monotonic() < deadline
            ):
                time.sleep(0.04)
                continue
            break
        self.assertEqual(
            result.returncode, 0 if ok else 2, result.stdout + result.stderr
        )
        return json.loads(result.stdout) if ok else result.stderr

    def create(self, index=0, key="request-1", **kwargs):
        return self.invoke(
            "playback",
            "admit",
            index=index,
            idempotency_key=key,
            body=kwargs.pop("body", self.body),
            **kwargs,
        )

    def job(self, identifier):
        return reference.PlaybackTest.job(self, identifier)

    def status(self, identifier, index=0, **kwargs):
        return self.invoke(
            "playback", "status", index=index, session_id=identifier, **kwargs
        )

    def worker(self, operation="tick", **kwargs):
        return self.invoke("playback-worker", operation, **kwargs)

    def mutate(self, callback):
        deadline = time.monotonic() + 8
        while True:
            try:
                with Store(self.database, writable=True) as store:
                    return callback(store)
            except Exception as exc:
                if (
                    "another Catabolic writer" not in str(exc)
                    or time.monotonic() >= deadline
                ):
                    raise
                time.sleep(0.02)

    def start_worker(self, *, continuous=False):
        command = [
            str(BINARY),
            "--db",
            str(self.database),
            "operation",
            "playback-worker",
            "run",
            "--options",
            json.dumps({"once": not continuous}),
        ]
        worker = subprocess.Popen(
            command,
            env=self.env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

        def cleanup():
            if worker.poll() is None:
                worker.kill()
            worker.communicate(timeout=10)

        self.addCleanup(cleanup)
        return worker

    def wait_playing(self, identifier):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            status = self.status(identifier)
            self.assertNotEqual(status["state"], "failed", status)
            if status["playlist_path"]:
                return status
            time.sleep(0.05)
        self.fail("native worker did not publish a complete segment")

    def long_source(self):
        return reference.PlaybackTest.long_source(self)

    def paced_tools(self, *, truncate=False):
        """A fixture tool execs real FFmpeg, preserving the native child PID."""
        real = shutil.which("ffmpeg")
        directory = self.root / "tools"
        directory.mkdir()
        ffmpeg = directory / "ffmpeg"
        script = (
            f"#!{sys.executable}\nimport os,sys\na=sys.argv[1:]\n"
            f"if '-i' in a:\n open({str(self.root / 'encoder.pid')!r},'w').write(str(os.getpid()))\n"
            + (
                " a[a.index('-i'):a.index('-i')]=['-re']\n"
                if not truncate
                else " a[a.index('-f'):a.index('-f')]=['-t','1']\n"
            )
            + f"os.execv({real!r},[{real!r},*a])\n"
        )
        ffmpeg.write_text(script)
        ffmpeg.chmod(0o700)
        (directory / "ffprobe").symlink_to(shutil.which("ffprobe"))
        self.env["PATH"] = str(directory) + os.pathsep + os.environ["PATH"]
        caps = self.worker("capabilities")
        self.mutate(
            lambda store: store.db.execute(
                "UPDATE api_worker_status SET capabilities=?,updated_at=?",
                (encode(caps), time.time()),
            )
        )

    def assert_worker_finished(self, worker):
        out, err = worker.communicate(timeout=12)
        self.assertEqual(worker.returncode, 0, out + err)
        return json.loads(out)

    def test_admission_python_interop_shared_job_isolation_and_rotation(self):
        first = self.create()
        second = self.create(1)
        self.assertEqual(first["state"], "queued")
        self.assertEqual(
            self.job(first["session_id"])["id"], self.job(second["session_id"])["id"]
        )
        self.assertEqual(self.create()["session_id"], first["session_id"])
        self.assertIn("idempotency_conflict", self.create(ttl=60, ok=False))
        self.assertIn("not_found", self.status(first["session_id"], 1, ok=False))
        self.assertIn(
            "playback_not_ready",
            self.invoke(
                "playback", "playlist", session_id=first["session_id"], ok=False
            ),
        )
        python = self.client.get(first["status_url"], headers=self.headers[0])
        self.assertEqual(python.status_code, 200, python.text)
        self.assertEqual(python.json(), first)

        def rotate(store):
            old = authenticate(store, self.headers[0]["Authorization"].split()[1])
            replacement = issue(store, old.principal, [g["_id"] for g in old.grants])
            revoke(store, old.token["id"])
            return replacement

        replacement = self.mutate(rotate)
        self.headers[0]["Authorization"] = "Bearer " + replacement["token"]
        self.assertEqual(self.create()["session_id"], first["session_id"])
        with Store(self.database) as store:
            token = store.rows(
                "SELECT token_id FROM api_playback_sessions WHERE id=?",
                (first["session_id"],),
            )[0]["token_id"]
            self.assertEqual(token, replacement["token_id"])
            self.assertEqual(
                store.rows("SELECT sum(reserved_bytes) n FROM api_playback_jobs")[0][
                    "n"
                ],
                16777216,
            )
        self.worker()
        self.assertEqual(self.status(first["session_id"])["state"], "ready")

    def test_real_native_hls_python_delivery_decodes_and_ready_cache_reuse(self):
        original = self.long_source()
        first = self.create()
        identifier = first["session_id"]
        self.worker()
        ready = self.status(identifier)
        self.assertEqual(ready["state"], "ready", ready)
        self.assertEqual(ready["segment_count"], 5)
        self.assertAlmostEqual(ready["available_seconds"], 10, delta=0.15)
        manifest = self.invoke("playback", "playlist", session_id=identifier)[
            "playlist"
        ]
        self.assertIn("#EXT-X-ENDLIST", manifest)
        self.assertNotIn("/dev/fd", manifest)
        segments = []
        for uri in manifest.splitlines():
            if uri and not uri.startswith("#"):
                response = self.client.get(
                    f"/v1/playback-sessions/{identifier}/" + uri,
                    headers=self.headers[0],
                )
                self.assertEqual(response.status_code, 200, response.text)
                segments.append(response.content)
                name = uri.split("/")[1]
                inspected = self.invoke(
                    "playback", "inspect-segment", session_id=identifier, segment=name
                )
                self.assertEqual(
                    inspected["sha256"], hashlib.sha256(response.content).hexdigest()
                )
                self.assertEqual(inspected["size"], len(response.content))
        output = self.root / "output.ts"
        output.write_bytes(b"".join(segments))
        subprocess.run(
            ["ffmpeg", "-v", "error", "-xerror", "-i", str(output), "-f", "null", "-"],
            check=True,
            capture_output=True,
        )
        for name in [
            "segment-999999.ts",
            "segment-000000.ts.tmp",
            "../secret",
            "index.m3u8",
        ]:
            self.assertIn(
                "not_found",
                self.invoke(
                    "playback",
                    "inspect-segment",
                    session_id=identifier,
                    segment=name,
                    ok=False,
                ),
            )
        self.assertIn(
            "range_not_satisfiable",
            self.invoke(
                "playback",
                "inspect-segment",
                session_id=identifier,
                segment="segment-000000.ts",
                range="bytes=0-1",
                ok=False,
            ),
        )
        second = self.create(key="cached")
        self.assertEqual(second["state"], "ready")
        self.assertEqual(
            self.job(identifier)["id"], self.job(second["session_id"])["id"]
        )
        self.assertEqual(
            hashlib.sha256((self.root / "source/sample.mkv").read_bytes()).hexdigest(),
            original,
        )
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT * FROM processing_artifacts"), [])
        directory = (
            self.root / "generated" / (".playback-" + self.job(identifier)["id"])
        )
        self.assertEqual(directory.stat().st_mode & 0o777, 0o700)
        self.assertEqual(
            (directory / "segment-000000.ts").stat().st_mode & 0o777, 0o600
        )

    def test_growing_whole_segments_and_independent_cancellation(self):
        self.long_source()
        self.paced_tools()
        first = self.create()
        second = self.create(1)
        worker = self.start_worker()
        playing = self.wait_playing(first["session_id"])
        self.assertIsNone(
            worker.poll(), "HLS must be readable before the encode completes"
        )
        self.assertEqual(playing["state"], "playing")
        manifest = self.invoke("playback", "playlist", session_id=first["session_id"])[
            "playlist"
        ]
        self.assertNotIn("#EXT-X-ENDLIST", manifest)
        cancelled = self.invoke("playback", "cancel", session_id=first["session_id"])
        self.assertEqual(cancelled["state"], "cancelled")
        self.assertIn(
            "playback_session_cancelled",
            self.invoke(
                "playback", "playlist", session_id=first["session_id"], ok=False
            ),
        )
        self.assertIsNone(worker.poll())
        self.invoke("playback", "playlist", index=1, session_id=second["session_id"])
        self.invoke("playback", "cancel", index=1, session_id=second["session_id"])
        self.assert_worker_finished(worker)
        job = self.job(first["session_id"])
        self.assertEqual(job["state"], "cancelled")
        self.assertEqual(job["reserved_bytes"], 0)
        self.worker("cleanup")
        self.assertFalse(list((self.root / "generated").glob(".playback-*")))

    def test_stored_token_revocation_stops_running_native_encoder(self):
        self.long_source()
        self.paced_tools()
        first = self.create()
        worker = self.start_worker()
        self.wait_playing(first["session_id"])
        self.mutate(
            lambda store: revoke(
                store, self.headers[0]["Authorization"].split()[1].split(".")[0]
            )
        )
        self.assert_worker_finished(worker)
        self.assertEqual(self.job(first["session_id"])["state"], "cancelled")
        self.assertIn("unauthorized", self.status(first["session_id"], ok=False))

    def test_source_mutation_stops_native_worker(self):
        self.long_source()
        self.paced_tools()
        first = self.create()
        worker = self.start_worker()
        self.wait_playing(first["session_id"])
        with (self.root / "source/sample.mkv").open("ab") as f:
            f.write(b"changed")
        self.assert_worker_finished(worker)
        self.assertEqual(
            self.job(first["session_id"])["error"], "playback_source_changed"
        )
        self.assertNotEqual(self.status(first["session_id"], ok=False), "")

    def test_worker_tools_change_rejects_queued_job(self):
        first = self.create()
        self.mutate(
            lambda store: store.db.execute("UPDATE api_playback_jobs SET tools='{}'")
        )
        self.worker()
        job = self.job(first["session_id"])
        self.assertEqual(job["state"], "failed")
        self.assertEqual(job["error"], "playback_tools_changed")
        self.assertEqual(job["reserved_bytes"], 0)

    def test_expiry_owned_cleanup_and_foreign_symlink_refusal(self):
        first = self.create()
        self.worker()
        identifier = first["session_id"]
        job = self.job(identifier)
        directory = self.root / "generated" / (".playback-" + job["id"])
        unrelated = self.root / "generated/unrelated"
        unrelated.write_text("keep")
        foreign = directory / "foreign"
        foreign.write_text("keep")

        def expire(store):
            store.db.execute("UPDATE api_playback_sessions SET expires=0")
            store.db.execute("UPDATE api_playback_jobs SET updated_at=0")

        self.mutate(expire)
        self.assertIn("playback_session_expired", self.status(identifier, ok=False))
        self.worker("cleanup")
        self.assertEqual(foreign.read_text(), "keep")
        foreign.unlink()
        segment = directory / "segment-000000.ts"
        segment.unlink()
        segment.symlink_to(unrelated)
        self.worker("cleanup")
        self.assertTrue(segment.is_symlink())
        self.assertEqual(unrelated.read_text(), "keep")
        segment.unlink()
        self.worker("cleanup")
        self.assertFalse(directory.exists())
        self.assertEqual(unrelated.read_text(), "keep")
        self.assertEqual(self.job(identifier)["state"], "stale")
        with Store(self.database) as store:
            self.assertEqual(len(store.rows("SELECT * FROM api_playback_sessions")), 1)

    def test_direct_ready_rendition_without_worker_and_source_ctime_pin(self):
        def render(store):
            artifacts = Artifacts(Application(store))
            artifacts.enqueue(self.file, self.recipe, "generated", self.item)
            self.assertTrue(artifacts.run()["complete"])
            store.db.execute("DELETE FROM api_worker_status")

        self.mutate(render)
        first = self.create()
        self.assertEqual(first["state"], "ready")
        self.assertIsNone(first["playlist_path"])
        self.assertIsNotNone(first["content_path"])
        self.assertEqual(
            self.client.get(first["content_path"], headers=self.headers[0]).status_code,
            200,
        )
        self.assertIn(
            "not_found",
            self.invoke(
                "playback", "playlist", session_id=first["session_id"], ok=False
            ),
        )
        with Store(self.database) as store:
            self.assertEqual(store.rows("SELECT * FROM api_playback_jobs"), [])
        source = self.root / "source/sample.mkv"
        before = source.stat()
        data = bytearray(source.read_bytes())
        data[-1] ^= 1
        source.write_bytes(data)
        os.utime(source, ns=(before.st_atime_ns, before.st_mtime_ns))
        self.assertIn("revision changed", self.status(first["session_id"], ok=False))

    def test_budget_failure_and_reservation_limits(self):
        def limit(store):
            recipe = Artifacts(Application(store)).recipe(
                "limited", "h264-720p", {"reserve_bytes": 0, "max_output_bytes": 1024}
            )
            store.db.execute("UPDATE api_operations SET recipe_id=?", (recipe["id"],))

        self.mutate(limit)
        first = self.create()
        self.worker()
        job = self.job(first["session_id"])
        self.assertEqual(job["state"], "failed")
        self.assertEqual(job["error"], "playback_storage_budget")
        self.assertEqual(job["reserved_bytes"], 0)
        self.assertIsNone(self.status(first["session_id"])["playlist_path"])

    def test_truncated_duration_is_not_ready(self):
        self.long_source()
        self.paced_tools(truncate=True)
        first = self.create()
        self.worker()
        job = self.job(first["session_id"])
        self.assertEqual(job["state"], "failed")
        self.assertEqual(job["error"], "playback_duration_mismatch")
        self.assertEqual(job["reserved_bytes"], 0)

    def test_worker_death_kills_encoder_recovery_releases_reservation(self):
        self.long_source()
        self.paced_tools()
        first = self.create()
        worker = self.start_worker()
        self.wait_playing(first["session_id"])
        pid = int((self.root / "encoder.pid").read_text())
        worker.kill()
        worker.communicate(timeout=5)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            try:
                raw = Path(f"/proc/{pid}/stat").read_text()
            except FileNotFoundError:
                break
            if raw.rsplit(") ", 1)[1].startswith("Z"):
                break
            time.sleep(0.02)
        else:
            os.kill(pid, signal.SIGKILL)
            self.fail("orphan encoder survived killed worker")
        self.assertEqual(self.worker("recover")["recovered"], 1)
        job = self.job(first["session_id"])
        self.assertEqual(job["state"], "failed")
        self.assertEqual(job["error"], "playback_worker_interrupted")
        self.assertEqual(job["reserved_bytes"], 0)
        self.invoke("playback", "cancel", session_id=first["session_id"])
        self.worker("cleanup")
        self.assertFalse(list((self.root / "generated").glob(".playback-*")))

    def test_live_tool_change_is_fenced(self):
        self.long_source()
        self.paced_tools()
        first = self.create()
        worker = self.start_worker()
        self.wait_playing(first["session_id"])
        wrapper = self.root / "tools/ffmpeg"
        before = wrapper.stat()
        os.utime(wrapper, ns=(before.st_atime_ns, before.st_mtime_ns + 10_000_000_000))
        self.assert_worker_finished(worker)
        job = self.job(first["session_id"])
        self.assertEqual(job["state"], "failed")
        self.assertEqual(job["error"], "playback_tools_changed")
        self.assertEqual(job["reserved_bytes"], 0)

    def test_timeout_releases_reservation(self):
        self.long_source()
        self.paced_tools()

        def limit(store):
            recipe = Artifacts(Application(store)).recipe(
                "timeout",
                "h264-720p",
                {"reserve_bytes": 0, "max_output_bytes": 16777216, "timeout": 1},
            )
            store.db.execute("UPDATE api_operations SET recipe_id=?", (recipe["id"],))

        self.mutate(limit)
        first = self.create()
        worker = self.start_worker()
        self.assert_worker_finished(worker)
        job = self.job(first["session_id"])
        self.assertEqual(job["state"], "failed")
        self.assertEqual(job["error"], "playback_timeout")
        self.assertEqual(job["reserved_bytes"], 0)

    def test_reservations_limits_invalid_request_and_missing_worker(self):
        self.assertIn("invalid_idempotency_key", self.create(key="", ok=False))
        self.assertIn(
            "invalid_playback_request",
            self.create(body={**self.body, "extra": "x"}, ok=False),
        )
        self.assertIn("invalid_playback_request", self.create(ttl=True, ok=False))
        for i in range(10):
            self.create(key=f"bounded-{i}")
        self.assertIn("playback_session_limit", self.create(key="overflow", ok=False))

        def reserve(store):
            store.db.execute("UPDATE api_playback_sessions SET cancelled=1")
            store.db.execute("UPDATE api_playback_jobs SET reserved_bytes=?", (10**15,))
            recipe = Artifacts(Application(store)).recipe(
                "different",
                "h264-720p",
                {"reserve_bytes": 0, "max_output_bytes": 16777216, "crf": 30},
            )
            store.db.execute("UPDATE api_operations SET recipe_id=?", (recipe["id"],))

        self.mutate(reserve)
        self.assertIn(
            "storage_reservation_unavailable", self.create(key="capacity", ok=False)
        )
        with Store(self.database) as store:
            self.assertEqual(len(store.rows("SELECT * FROM api_playback_sessions")), 10)
        self.mutate(lambda store: store.db.execute("DELETE FROM api_worker_status"))
        self.assertIn("worker_unavailable", self.create(key="absent", ok=False))

    def test_python_session_native_worker_and_python_recovery(self):
        response = self.client.post(
            "/v1/playback-sessions", headers=self.headers[0], json=self.body
        )
        self.assertEqual(response.status_code, 202, response.text)
        first = response.json()
        self.assertEqual(self.create()["session_id"], first["session_id"])
        self.worker()
        ready = self.client.get(first["status_url"], headers=self.headers[0])
        self.assertEqual(ready.status_code, 200, ready.text)
        self.assertEqual(ready.json()["state"], "ready")

        def recover(store):
            from catabolic.playback_worker import recover

            store.db.execute("UPDATE api_playback_jobs SET state='running'")
            recover(store, "default")

        self.mutate(recover)
        self.assertEqual(self.status(first["session_id"])["state"], "failed")

    def test_python_supervisor_lock_excludes_native_recovery(self):
        from catabolic.database_io import acquire_writer_lock

        lock = acquire_writer_lock(
            self.database.with_name(self.database.name + ".api-worker")
        )
        try:
            result = subprocess.run(
                [
                    str(BINARY),
                    "--db",
                    str(self.database),
                    "operation",
                    "playback-worker",
                    "recover",
                    "--options",
                    "{}",
                ],
                env=self.env,
                text=True,
                capture_output=True,
                timeout=10,
            )
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            self.assertIn("another Catabolic writer", result.stderr)
        finally:
            os.close(lock)
        self.assertEqual(self.worker("recover")["recovered"], 0)

    def test_continuous_worker_advertises_readiness_and_shared_grants_are_current(self):
        self.mutate(lambda store: store.db.execute("DELETE FROM api_worker_status"))
        worker = self.start_worker(continuous=True)
        deadline = time.monotonic() + 10
        while True:
            with Store(self.database) as store:
                rows = store.rows("SELECT * FROM api_worker_status")
            if rows:
                break
            self.assertIsNone(worker.poll())
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.05)
        first = self.create()
        identifier = first["session_id"]
        deadline = time.monotonic() + 15
        while self.status(identifier)["state"] != "ready":
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.1)

        def disable(store):
            access = authenticate(store, self.headers[0]["Authorization"].split()[1])
            grant = next(g for g in access.grants if "content:read" in g["actions"])
            definition = {k: v for k, v in grant.items() if not k.startswith("_")}
            definition["item_ids"] = []
            grant_put(store, access.principal, definition, grant["_id"])

        self.mutate(disable)
        self.assertIn(
            "playback_content_not_authorized", self.status(identifier, ok=False)
        )

    def second_operation_grant(self):
        def configure(store):
            store.db.execute(
                "INSERT INTO api_operations(id,profile,recipe_id,location) SELECT 'second-operation',profile,recipe_id,location FROM api_operations WHERE id=?",
                (self.operation,),
            )
            access = authenticate(store, self.headers[1]["Authorization"].split()[1])
            grant = next(
                g for g in access.grants if "processing:request" in g["actions"]
            )
            definition = {k: v for k, v in grant.items() if not k.startswith("_")}
            definition["operation_ids"] = ["second-operation"]
            grant_put(store, access.principal, definition, grant["_id"])

        self.mutate(configure)
        return {**self.body, "operation_id": "second-operation"}

    def test_distinct_operation_grants_survive_first_viewer_cancellation(self):
        second_body = self.second_operation_grant()
        first = self.create()
        second = self.create(1, body=second_body)
        self.assertEqual(
            self.job(first["session_id"])["id"], self.job(second["session_id"])["id"]
        )
        self.invoke("playback", "cancel", session_id=first["session_id"])
        self.assertEqual(self.status(second["session_id"], 1)["state"], "queued")
        self.worker()
        ready = self.status(second["session_id"], 1)
        self.assertEqual(ready["state"], "ready", ready)
        self.assertGreater(ready["segment_count"], 0)
        self.assertEqual(self.status(first["session_id"])["state"], "cancelled")
        manifest = self.invoke(
            "playback", "playlist", index=1, session_id=second["session_id"]
        )
        self.assertIn("#EXT-X-ENDLIST", manifest["playlist"])
        job = self.job(second["session_id"])
        self.assertEqual(job["state"], "ready")
        self.assertEqual(job["reserved_bytes"], 0)

    def test_shared_viewer_must_still_match_pinned_job(self):
        second_body = self.second_operation_grant()
        with Store(self.database) as store:
            original = store.rows(
                "SELECT * FROM api_operations WHERE id='second-operation'"
            )[0]
        for change in ("item", "file", "revision", "recipe", "destination"):
            with self.subTest(change=change):

                def reset(store):
                    store.db.execute(
                        "UPDATE api_operations SET recipe_id=?,location=? WHERE id='second-operation'",
                        (original["recipe_id"], original["location"]),
                    )
                    store.db.execute(
                        "INSERT INTO api_worker_status VALUES (?,?,?,?) ON CONFLICT(profile) DO UPDATE SET worker_pid=excluded.worker_pid,capabilities=excluded.capabilities,updated_at=excluded.updated_at",
                        ("default", os.getpid(), encode(self.caps), time.time()),
                    )

                self.mutate(reset)
                first = self.create(key="first-" + change)
                second = self.create(1, key="second-" + change, body=second_body)
                self.invoke("playback", "cancel", session_id=first["session_id"])

                def rebind(store, change=change, second=second):
                    if change in ("item", "file", "revision"):
                        row = store.rows(
                            "SELECT body FROM api_playback_sessions WHERE id=?",
                            (second["session_id"],),
                        )[0]
                        body = json.loads(row["body"])
                        body[
                            {
                                "item": "item_id",
                                "file": "source_file_id",
                                "revision": "source_revision",
                            }[change]
                        ] = "unrelated"
                        store.db.execute(
                            "UPDATE api_playback_sessions SET body=? WHERE id=?",
                            (encode(body), second["session_id"]),
                        )
                    elif change == "recipe":
                        recipe = Artifacts(Application(store)).recipe(
                            "reassigned",
                            "h264-720p",
                            {
                                "reserve_bytes": 0,
                                "max_output_bytes": 16777216,
                                "crf": 30,
                            },
                        )
                        store.db.execute(
                            "UPDATE api_operations SET recipe_id=? WHERE id='second-operation'",
                            (recipe["id"],),
                        )
                    else:
                        root = self.root / "other-destination"
                        root.mkdir()
                        Artifacts(Application(store)).bind("other-destination", root)
                        store.db.execute(
                            "INSERT INTO api_sources VALUES ('default','other-destination')"
                        )
                        store.db.execute(
                            "UPDATE api_operations SET location='other-destination' WHERE id='second-operation'"
                        )

                self.mutate(rebind)
                self.worker()
                job = self.job(second["session_id"])
                self.assertEqual(job["state"], "cancelled", job)
                self.assertEqual(job["reserved_bytes"], 0)
