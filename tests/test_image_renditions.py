# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Image recipe admission and decoded, authenticated photo derivatives."""

import hashlib
import importlib.util
import json
import os
import shutil
import subprocess
import tempfile
import time
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from threading import Barrier
from unittest.mock import patch

from catabolic import rendering
from catabolic.access import grant_put, issue, principal_put
from catabolic.app import Application
from catabolic.artifacts import Artifacts
from catabolic.content_access import revision_of
from catabolic.domain import CatabolicError
from catabolic.http_worker_local import run
from catabolic.maintenance import run as maintenance
from catabolic.outputs import RENDITIONS_SQL
from catabolic.processing import Processing
from catabolic.rules import Rules
from catabolic.store import Store, encode


class ImageDefinitionTest(unittest.TestCase):
    def test_options_are_bounded_and_old_recipes_keep_their_definitions(self):
        self.assertEqual(rendering.definition("thumbnail", {})["start_seconds"], 0)
        self.assertNotIn("quality", rendering.definition("thumbnail", {}))
        for preset in rendering.PHOTO_PRESETS:
            recipe = rendering.definition(preset, {})
            self.assertEqual(recipe["quality"], 85)
            for options in (
                {"quality": True},
                {"quality": 0},
                {"quality": 101},
                {"width": 0},
                {"height": 4097},
                {"width": "160"},
                {"max_input_pixels": 40000001},
                {"max_input_bytes": 1},
                {"timeout": 301},
                {"max_output_bytes": 67108865},
                {"video_stream": 1},
                {"crop": "evil"},
            ):
                with (
                    self.subTest(preset=preset, options=options),
                    self.assertRaises(CatabolicError),
                ):
                    rendering.definition(preset, options)

    def test_only_static_bounded_sdr_images_are_admitted(self):
        recipe = rendering.definition("image-webp", {"max_input_pixels": 1000})
        image = {"codec_type": "video", "codec_name": "png", "width": 20, "height": 30}
        data = {"format": {"format_name": "png_pipe"}, "streams": [image]}
        self.assertIsNone(rendering.input_error(recipe, data))
        for change in (
            {"width": 100},
            {"height": None},
            {"nb_frames": "2"},
            {"nb_read_packets": "2"},
            {"codec_name": "h264"},
            {"color_transfer": "smpte2084"},
        ):
            with self.subTest(change=change):
                self.assertIsNotNone(
                    rendering.input_error(
                        recipe, {**data, "streams": [{**image, **change}]}
                    )
                )
        self.assertIsNotNone(
            rendering.input_error(recipe, {**data, "format": {"format_name": "mov"}})
        )

    def test_byte_budget_and_animation_reject_before_probe(self):
        with tempfile.TemporaryFile() as source, tempfile.TemporaryFile() as output:
            source.write(b"x" * 1025)
            source.flush()
            recipe = rendering.definition("image-jpeg", {"max_input_bytes": 1024})
            with (
                patch.object(rendering, "probe") as probe,
                self.assertRaisesRegex(CatabolicError, "byte budget"),
            ):
                rendering.render(
                    source.fileno(), output.fileno(), recipe, {}, lambda: None
                )
            probe.assert_not_called()
            source.seek(0)
            source.truncate()
            source.write(
                b"RIFF"
                + b"\x16\x00\x00\x00"
                + b"WEBPVP8X"
                + b"\x0a\x00\x00\x00"
                + b"\x02"
                + b"\x00" * 9
            )
            source.flush()
            with (
                patch.object(rendering, "probe") as probe,
                self.assertRaisesRegex(CatabolicError, "animated WebP"),
            ):
                rendering.render(
                    source.fileno(), output.fileno(), recipe, {}, lambda: None
                )
            probe.assert_not_called()


@unittest.skipUnless(
    importlib.util.find_spec("fastapi")
    and importlib.util.find_spec("httpx")
    and shutil.which("ffmpeg")
    and shutil.which("ffprobe"),
    "HTTP and FFmpeg required",
)
class ImageHTTPTest(unittest.TestCase):
    def setUp(self):
        from fastapi.testclient import TestClient

        from catabolic.http.app import create_app

        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name).resolve()
        source = self.root / "source"
        source.mkdir()
        generated = self.root / "generated"
        generated.mkdir()
        self.original = source / "photo.png"
        subprocess.run(
            [
                "ffmpeg",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x180",
                "-frames:v",
                "1",
                "-threads",
                "1",
                str(self.original),
            ],
            check=True,
            capture_output=True,
        )
        self.source_hash = hashlib.sha256(self.original.read_bytes()).hexdigest()
        self.database = self.root / "catalog.db"
        Store.initialize(self.database)
        with Store(self.database, writable=True) as store:
            app = Application(store)
            app.bind("source", "source", str(source))
            app.scan()
            self.file = store.rows("SELECT id FROM files")[0]["id"]
            self.item = app.put_item("photo", {"fixture": "photo"}, {"title": "Photo"})[
                "id"
            ]
            app.media.associate(self.file, self.item)
            a = Artifacts(app)
            a.bind("generated", generated)
            self.operations = []
            for operation, preset, size, quality in [
                ("image-jpeg", "image-jpeg", 160, 90),
                ("image-jpeg-low", "image-jpeg", 160, 40),
                ("image-webp", "image-webp", 80, 90),
            ]:
                found = next(
                    p
                    for p in rendering.capabilities()["presets"]
                    if p["name"] == preset
                )
                if not found["available"]:
                    if preset == "image-jpeg":
                        self.skipTest(str(found["missing"]))
                    continue
                recipe = a.recipe(
                    operation,
                    preset,
                    {"width": size, "height": size, "quality": quality},
                )
                self.operations.append(operation)
                with store.transaction() as db:
                    db.execute(
                        "INSERT INTO api_operations(id,profile,recipe_id,location) VALUES (?,?,?,?)",
                        (operation, "default", recipe["id"], "generated"),
                    )
            with store.transaction() as db:
                db.execute("INSERT INTO api_sources VALUES ('default','generated')")
                db.execute(
                    "INSERT INTO api_worker_status VALUES (?,?,?,?)",
                    (
                        "default",
                        os.getpid(),
                        encode(rendering.capabilities()),
                        time.time(),
                    ),
                )
            self.headers = []
            for principal in ("one", "two"):
                principal_put(store, "default", principal)
                request = grant_put(
                    store,
                    principal,
                    {
                        "actions": ["metadata:read", "processing:request"],
                        "item_ids": [self.item],
                        "file_ids": [self.file],
                        "operation_ids": self.operations,
                    },
                )
                read = grant_put(
                    store,
                    principal,
                    {
                        "actions": ["metadata:read", "content:read"],
                        "item_ids": [self.item],
                        "derivatives": True,
                    },
                )
                token = issue(
                    store, principal, [request["grant_id"], read["grant_id"]]
                )["token"]
                self.headers.append(
                    {
                        "Authorization": "Bearer " + token,
                        "Idempotency-Key": "image-request",
                    }
                )
            self.body = {
                "item_id": self.item,
                "source_file_id": self.file,
                "source_revision": revision_of(store, "default", self.file),
                "operation_id": "image-jpeg",
            }
        self.client = TestClient(create_app(self.database, read_only=False))
        self.addCleanup(self.client.close)

    def submit(self, body=None, headers=None):
        # once=True deliberately stops the worker. Announce the next supervised
        # worker before new demand, just as setup does before the initial request.
        with Store(self.database, writable=True) as store:
            with store.transaction() as db:
                db.execute(
                    "INSERT INTO api_worker_status VALUES (?,?,?,?) ON CONFLICT(profile) DO UPDATE SET worker_pid=excluded.worker_pid,capabilities=excluded.capabilities,updated_at=excluded.updated_at",
                    (
                        "default",
                        os.getpid(),
                        encode(rendering.capabilities()),
                        time.time(),
                    ),
                )
        r = self.client.post(
            "/v1/rendition-requests",
            headers=headers or self.headers[0],
            json=body or self.body,
        )
        self.assertIn(r.status_code, (200, 202), r.text)
        result = r.json()
        self.assertEqual(r.status_code, 200 if result["state"] == "ready" else 202)
        return result

    def result(self, request, codec, dimensions, mime):
        ready = self.client.get(request["status_url"], headers=self.headers[0]).json()
        self.assertEqual(ready["state"], "ready", ready)
        response = self.client.get(
            ready["result"]["content_path"], headers=self.headers[0]
        )
        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual(response.headers["content-type"], mime)
        self.assertEqual(
            self.client.get(ready["result"]["content_path"]).status_code, 401
        )
        path = self.root / ("delivered.jpg" if codec == "mjpeg" else "delivered.webp")
        path.write_bytes(response.content)
        data = json.loads(
            subprocess.check_output(
                ["ffprobe", "-v", "error", "-show_streams", "-of", "json", str(path)]
            )
        )
        self.assertEqual(len(data["streams"]), 1)
        image = data["streams"][0]
        self.assertEqual(
            (image["codec_name"], image["width"], image["height"]), (codec, *dimensions)
        )
        subprocess.run(
            ["ffmpeg", "-v", "error", "-i", str(path), "-f", "null", "-"],
            check=True,
            capture_output=True,
        )
        return ready

    def test_concurrent_photo_requests_share_work_and_cached_bytes(self):
        barrier = Barrier(2)

        def submit(headers):
            barrier.wait(timeout=5)
            for _ in range(100):
                r = self.client.post(
                    "/v1/rendition-requests", headers=headers, json=self.body
                )
                if r.status_code != 503:
                    return r
                time.sleep(0.01)
            return r

        with ThreadPoolExecutor(max_workers=2) as pool:
            responses = list(pool.map(submit, self.headers))
        for r in responses:
            self.assertEqual(r.status_code, 202, r.text)
        first, second = [r.json() for r in responses]
        with Store(self.database) as store:
            self.assertEqual(len(store.rows("SELECT id FROM processing_jobs")), 1)
            self.assertEqual(len(store.rows("SELECT job_id FROM api_reservations")), 1)
        self.assertTrue(run(self.database, once=True)["complete"])
        ready = self.result(first, "mjpeg", (160, 90), "image/jpeg")
        self.assertEqual(
            self.client.get(second["status_url"], headers=self.headers[1]).json()[
                "result"
            ],
            ready["result"],
        )
        self.assertEqual(
            self.client.get(second["status_url"], headers=self.headers[0]).status_code,
            404,
        )
        repeated = self.submit(headers={**self.headers[0], "Idempotency-Key": "cached"})
        self.assertEqual(repeated["state"], "ready")
        self.assertEqual(repeated["result"], ready["result"])
        with Store(self.database) as store:
            self.assertEqual(len(store.rows("SELECT id FROM processing_jobs")), 1)
            self.assertTrue(
                all(
                    r["purpose"] == "thumbnail"
                    for r in store.rows(f"SELECT purpose FROM ({RENDITIONS_SQL})")
                )
            )
        self.assertEqual(
            hashlib.sha256(self.original.read_bytes()).hexdigest(), self.source_hash
        )

    def test_webp_variant_is_a_distinct_decodable_image(self):
        if "image-webp" not in self.operations:
            self.skipTest("FFmpeg libwebp encoder unavailable")
        jpeg = self.submit()
        self.assertTrue(run(self.database, once=True)["complete"])
        self.result(jpeg, "mjpeg", (160, 90), "image/jpeg")
        webp = self.submit(
            {**self.body, "operation_id": "image-webp"},
            {**self.headers[0], "Idempotency-Key": "webp"},
        )
        self.assertTrue(run(self.database, once=True)["complete"])
        self.result(webp, "webp", (80, 45), "image/webp")
        with Store(self.database) as store:
            self.assertEqual(len(store.rows("SELECT id FROM processing_jobs")), 2)
        self.assertEqual(
            hashlib.sha256(self.original.read_bytes()).hexdigest(), self.source_hash
        )

    def test_quality_variants_do_not_reuse_each_others_output(self):
        high = self.submit()
        self.assertTrue(run(self.database, once=True)["complete"])
        high_result = self.result(high, "mjpeg", (160, 90), "image/jpeg")
        high_bytes = (self.root / "delivered.jpg").read_bytes()
        low = self.submit(
            {**self.body, "operation_id": "image-jpeg-low"},
            {**self.headers[0], "Idempotency-Key": "low"},
        )
        self.assertTrue(run(self.database, once=True)["complete"])
        low_result = self.result(low, "mjpeg", (160, 90), "image/jpeg")
        self.assertNotEqual(
            high_result["result"]["file_id"], low_result["result"]["file_id"]
        )
        self.assertNotEqual(high_bytes, (self.root / "delivered.jpg").read_bytes())

    def photo_rules(self, store, operations):
        app = Application(store)
        processor = Processing(app)
        processor.enqueue("probe", location="source", limit=100)
        self.assertTrue(processor.run(limit=100)["complete"])
        rules = Rules(app)
        identifiers = []
        for operation in operations:
            recipe = store.rows(
                "SELECT recipe_id FROM api_operations WHERE id=?", (operation,)
            )[0]["recipe_id"]
            rule = rules.put(
                "photo-" + operation,
                recipe,
                "generated",
                {
                    "language": "sql",
                    "query": "SELECT item_id FROM catalog_items WHERE kind='photo'",
                },
            )
            identifiers.append(rule["id"])
        return rules, identifiers

    def test_rules_prewarm_variants_and_http_serves_the_same_cached_outputs(self):
        operations = ["image-jpeg", "image-jpeg-low"]
        if "image-webp" in self.operations:
            operations.append("image-webp")
        with Store(self.database, writable=True) as store:
            rules, identifiers = self.photo_rules(store, operations)
            for identifier in identifiers:
                applied = rules.apply(identifier)
                self.assertEqual(len(applied["queued"]), 1, applied)
                completed = rules.run(identifier)
                self.assertTrue(completed["complete"], completed)
                self.assertEqual(completed["after"]["counts"]["satisfied"], 1)
                self.assertEqual(rules.apply(identifier)["queued"], [])
        for operation in operations:
            request = self.submit(
                {**self.body, "operation_id": operation},
                {**self.headers[0], "Idempotency-Key": "prewarmed-" + operation},
            )
            self.assertEqual(request["state"], "ready")
            if operation == "image-webp":
                self.result(request, "webp", (80, 45), "image/webp")
            else:
                self.result(request, "mjpeg", (160, 90), "image/jpeg")
        with Store(self.database) as store:
            self.assertEqual(
                len(
                    store.rows(
                        "SELECT id FROM processing_jobs WHERE operation='render'"
                    )
                ),
                len(operations),
            )
            self.assertEqual(
                len(store.rows("SELECT id FROM media_outputs")), len(operations)
            )
            self.assertEqual(store.rows("SELECT * FROM api_reservations"), [])
        self.assertEqual(
            hashlib.sha256(self.original.read_bytes()).hexdigest(), self.source_hash
        )

    def test_rule_adopts_on_demand_image_job_without_duplicate_rendering(self):
        request = self.submit()
        with Store(self.database, writable=True) as store:
            job = store.rows("SELECT job_id FROM api_requests")[0]["job_id"]
            rules, identifiers = self.photo_rules(store, ["image-jpeg"])
            applied = rules.apply(identifiers[0])
            self.assertEqual(applied["queued"], [])
            self.assertEqual(applied["adopted"], 1)
            self.assertEqual(
                store.rows("SELECT job_id FROM rule_jobs")[0]["job_id"], job
            )
            completed = rules.run(identifiers[0])
            self.assertTrue(completed["complete"], completed)
            self.assertEqual(
                len(
                    store.rows(
                        "SELECT id FROM processing_jobs WHERE operation='render'"
                    )
                ),
                1,
            )
        self.result(request, "mjpeg", (160, 90), "image/jpeg")

    def test_maintenance_runs_enabled_image_rules_in_bounded_resumable_cycles(self):
        with Store(self.database, writable=True) as store:
            app = Application(store)
            rules, identifiers = self.photo_rules(
                store, ["image-jpeg", "image-jpeg-low"]
            )
            # Saving a rule never enables unattended rendering by itself.
            disabled = maintenance(app, inventory_only=True, rules=True, render_rules=1)
            self.assertTrue(disabled["complete"], disabled)
            self.assertEqual(store.rows("SELECT * FROM rule_jobs"), [])
            for identifier in identifiers:
                rules.enable(identifier, True)
            first = maintenance(app, inventory_only=True, rules=True, render_rules=1)
            self.assertFalse(first["complete"])
            self.assertEqual(first["summary"]["rules"]["backlog"]["satisfied"], 1)
            self.assertEqual(first["summary"]["rules"]["backlog"]["queued"], 1)
            second = maintenance(app, inventory_only=True, rules=True, render_rules=1)
            self.assertTrue(second["complete"], second)
            self.assertEqual(second["summary"]["rules"]["backlog"]["satisfied"], 2)
            repeated = maintenance(app, inventory_only=True, rules=True, render_rules=1)
            self.assertTrue(repeated["complete"], repeated)
            self.assertEqual(len(store.rows("SELECT id FROM processing_artifacts")), 2)
        request = self.submit()
        self.assertEqual(request["state"], "ready")
        self.result(request, "mjpeg", (160, 90), "image/jpeg")

    def test_maintenance_reprobes_changed_photo_before_creating_new_variant(self):
        with Store(self.database, writable=True) as store:
            app = Application(store)
            rules, identifiers = self.photo_rules(store, ["image-jpeg"])
            rules.enable(identifiers[0], True)
            first = maintenance(app, inventory_only=True, rules=True, render_rules=1)
            self.assertTrue(first["complete"], first)
            subprocess.run(
                [
                    "ffmpeg",
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=c=blue:s=100x200",
                    "-frames:v",
                    "1",
                    "-threads",
                    "1",
                    str(self.original),
                ],
                check=True,
                capture_output=True,
            )
            stale = maintenance(app, inventory_only=True, rules=True, render_rules=1)
            self.assertFalse(stale["complete"])
            self.assertEqual(stale["summary"]["rules"]["backlog"]["deferred"], 1)
            self.assertEqual(len(store.rows("SELECT id FROM processing_artifacts")), 1)
            refreshed = maintenance(
                app,
                inventory_only=True,
                process="probe",
                settle=0,
                rules=True,
                render_rules=1,
            )
            self.assertTrue(refreshed["complete"], refreshed)
            self.assertEqual(len(store.rows("SELECT id FROM processing_artifacts")), 2)
            revision = revision_of(store, "default", self.file)
        request = self.submit({**self.body, "source_revision": revision})
        self.assertEqual(request["state"], "ready")
        self.result(request, "mjpeg", (80, 160), "image/jpeg")

    def test_small_input_is_not_upscaled(self):
        subprocess.run(
            [
                "ffmpeg",
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=37x19",
                "-frames:v",
                "1",
                "-threads",
                "1",
                str(self.original),
            ],
            check=True,
            capture_output=True,
        )
        with Store(self.database, writable=True) as store:
            Application(store).scan()
            revision = revision_of(store, "default", self.file)
        request = self.submit({**self.body, "source_revision": revision})
        self.assertTrue(run(self.database, once=True)["complete"])
        self.result(request, "mjpeg", (37, 19), "image/jpeg")

    def test_source_change_invalidates_ready_request_and_requires_new_revision(self):
        request = self.submit()
        self.assertTrue(run(self.database, once=True)["complete"])
        ready = self.result(request, "mjpeg", (160, 90), "image/jpeg")
        subprocess.run(
            [
                "ffmpeg",
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=100x200",
                "-frames:v",
                "1",
                "-threads",
                "1",
                str(self.original),
            ],
            check=True,
            capture_output=True,
        )
        with Store(self.database, writable=True) as store:
            Application(store).scan()
            revision = revision_of(store, "default", self.file)
        self.assertEqual(
            self.client.get(request["status_url"], headers=self.headers[0]).json()[
                "state"
            ],
            "stale",
        )
        # The old immutable derivative URL remains the original rendition; it
        # must never silently become the new source's resized image.
        self.assertEqual(
            self.client.get(
                ready["result"]["content_path"], headers=self.headers[0]
            ).content,
            (self.root / "delivered.jpg").read_bytes(),
        )
        # A stale source revision is rejected before any replacement work.
        self.submit(
            {**self.body, "source_revision": revision},
            {**self.headers[0], "Idempotency-Key": "warm"},
        )
        response = self.client.post(
            "/v1/rendition-requests",
            headers={**self.headers[0], "Idempotency-Key": "old"},
            json=self.body,
        )
        self.assertEqual(response.status_code, 409)
        fresh = self.submit(
            {**self.body, "source_revision": revision},
            {**self.headers[0], "Idempotency-Key": "new"},
        )
        self.assertTrue(run(self.database, once=True)["complete"])
        self.result(fresh, "mjpeg", (80, 160), "image/jpeg")


@unittest.skipUnless(
    shutil.which("ffmpeg") and shutil.which("ffprobe"), "FFmpeg required"
)
class ImageRenderTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.available = {
            p["name"]: p["available"] for p in rendering.capabilities()["presets"]
        }
        self.identity = rendering.tools()

    def fixture(self, name, filter="testsrc=size=320x180"):
        path = self.root / name
        subprocess.run(
            [
                "ffmpeg",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                filter,
                "-frames:v",
                "1",
                "-threads",
                "1",
                str(path),
            ],
            check=True,
            capture_output=True,
        )
        return path

    def render(self, source, preset, options=None):
        if not self.available[preset]:
            self.skipTest("encoder unavailable: " + preset)
        output = self.root / (source.stem + "-output." + rendering.PRESETS[preset][0])
        original = source.read_bytes()
        with source.open("rb") as input, output.open("w+b") as target:
            evidence = rendering.render(
                input.fileno(),
                target.fileno(),
                rendering.definition(preset, options or {}),
                self.identity,
                lambda: None,
            )
        subprocess.run(
            ["ffmpeg", "-v", "error", "-i", str(output), "-f", "null", "-"],
            check=True,
            capture_output=True,
        )
        self.assertEqual(source.read_bytes(), original)
        return output, evidence

    def test_static_jpeg_and_webp_sources_are_supported(self):
        if not self.available["image-webp"]:
            self.skipTest("WebP fixture encoder unavailable")
        for name in ("input.jpg", "input.webp"):
            with self.subTest(input=name):
                source = self.fixture(name)
                _, evidence = self.render(
                    source, "image-jpeg", {"width": 80, "height": 80}
                )
                self.assertEqual(evidence["output"]["summary"]["width"], 80)
                self.assertEqual(evidence["output"]["summary"]["height"], 45)

    def test_webp_preserves_alpha(self):
        source = self.fixture("alpha.png", "color=c=red@0.5:s=32x18,format=rgba")
        output, _ = self.render(source, "image-webp")

        def pixels(path):
            return subprocess.check_output(
                [
                    "ffmpeg",
                    "-v",
                    "error",
                    "-i",
                    str(path),
                    "-pix_fmt",
                    "rgba",
                    "-f",
                    "rawvideo",
                    "-",
                ]
            )

        before = pixels(source)
        after = pixels(output)
        self.assertEqual(len(before), 32 * 18 * 4)
        self.assertTrue(all(0 < alpha < 255 for alpha in before[3::4]))
        self.assertEqual(before[3::4], after[3::4])

    def test_real_pixel_budget_failure_creates_no_output(self):
        source = self.fixture("large.png")
        output = self.root / "output.jpg"
        with source.open("rb") as input, output.open("w+b") as target:
            with self.assertRaisesRegex(CatabolicError, "pixel budget"):
                rendering.render(
                    input.fileno(),
                    target.fileno(),
                    rendering.definition("image-jpeg", {"max_input_pixels": 100}),
                    self.identity,
                    lambda: None,
                )
            self.assertEqual(os.fstat(target.fileno()).st_size, 0)

    def test_concatenated_images_are_rejected_before_rendering(self):
        for extension in ("png", "jpg"):
            with self.subTest(extension=extension):
                source = self.fixture("sequence." + extension)
                source.write_bytes(source.read_bytes() * 2)
                output = self.root / "rejected.jpg"
                with source.open("rb") as input, output.open("w+b") as target:
                    with self.assertRaisesRegex(CatabolicError, "single static"):
                        rendering.render(
                            input.fileno(),
                            target.fileno(),
                            rendering.definition("image-jpeg", {}),
                            self.identity,
                            lambda: None,
                        )
                    self.assertEqual(os.fstat(target.fileno()).st_size, 0)
