# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent metadata review cases, on disposable catalogs and loopback only.

Reuses the owner's transport fixture and frozen database setup, but defines its
own schedules and expectations. load_tests deliberately selects only new cases.
"""

import json
import subprocess
import unittest
from urllib.parse import urlencode
from xml.etree import ElementTree as ET

from catabolic.consumer_adapters import ConsumerError
from catabolic.store import Store
from tests.test_rust_plex_metadata import NativePlexMetadataTest


class IndependentMetadataReview(NativePlexMetadataTest):
    def test_review_selected_unchanged_value_is_verified(self):
        self.curate(title="Changed title", summary="Plex summary")
        preview = self.preview(fields=["title", "summary"])
        self.assertNotIn("summary", preview["items"][0]["changes"])
        self.after_put = lambda: self.node.set("summary", "Concurrent remote edit")
        result = self.apply(preview, fields=["title", "summary"])
        self.assertFalse(result["complete"])
        self.assertEqual(result["results"][0]["state"], "uncertain")
        self.assertEqual(
            result["results"][0]["error"]["code"], "metadata_verification_failed"
        )
        self.assertEqual(len(self.writes()), 1)
        self.assertEqual(self.events()[0]["state"], "uncertain")

    def test_review_selected_unchanged_lock_is_verified(self):
        self.curate(title="Changed title", summary="Plex summary")
        preview = self.preview(fields=["title", "summary"])
        self.after_put = lambda: ET.SubElement(
            self.node, "Field", name="summary", locked="1"
        )
        result = self.apply(preview, fields=["title", "summary"])
        self.assertEqual(result["results"][0]["state"], "uncertain")
        self.assertEqual(
            result["results"][0]["error"]["code"], "metadata_verification_failed"
        )
        self.assertEqual(len(self.writes()), 1)

    def test_review_unselected_remote_change_does_not_expand_write(self):
        self.after_put = lambda: self.node.set("summary", "Unselected remote edit")
        result = self.apply(fields=["title"])
        self.assertTrue(result["complete"])
        self.assertEqual(result["results"][0]["state"], "verified")
        self.assertEqual(self.node.get("summary"), "Unselected remote edit")
        self.assertEqual(
            set(self.writes()[0][2]), {"id", "type", "title.value", "title.locked"}
        )

    def test_review_historical_evidence_does_not_pin_full_part_set(self):
        with Store(self.database) as store:
            evidence = json.loads(
                store.rows("SELECT evidence FROM proposals")[0]["evidence"]
            )
        self.assertNotIn("remote_paths", evidence)
        self.after_put = lambda: ET.SubElement(
            self.node.find("Media"), "Part", file="/media/another-part.mkv"
        )
        # Exact frozen compatibility: without a historical full-set claim,
        # retained original path + unchanged GUID passes both implementations.
        result = self.apply(fields=["title"])
        self.assertTrue(result["complete"])
        self.assertEqual(result["results"][0]["state"], "verified")
        self.assertEqual(len(self.events()[0]["observed"]["paths"]), 2)
        self.assertEqual(len(self.writes()), 1)

    def test_review_evidence_pinned_full_part_set_change_is_uncertain(self):
        with Store(self.database, writable=True) as store, store.transaction() as db:
            row = store.rows("SELECT id,evidence FROM proposals")[0]
            evidence = json.loads(row["evidence"])
            evidence["remote_paths"] = ["/media/movie.mkv"]
            db.execute(
                "UPDATE proposals SET evidence=? WHERE id=?",
                (json.dumps(evidence), row["id"]),
            )
        self.after_put = lambda: ET.SubElement(
            self.node.find("Media"), "Part", file="/media/another-part.mkv"
        )
        result = self.apply(fields=["title"])
        self.assertEqual(result["results"][0]["state"], "uncertain")
        self.assertEqual(
            result["results"][0]["error"]["code"],
            "metadata_import_identity_changed",
        )
        self.assertEqual(len(self.writes()), 1)

    def test_review_guid_change_after_put_is_uncertain(self):
        self.after_put = lambda: self.node.set("guid", "plex://movie/replacement")
        result = self.apply(fields=["title"])
        self.assertEqual(result["results"][0]["state"], "uncertain")
        self.assertEqual(
            result["results"][0]["error"]["code"],
            "metadata_import_identity_changed",
        )
        self.assertEqual(len(self.writes()), 1)

    def test_review_clear_order_and_duplicates_are_hashed(self):
        options = dict(fields=["summary", "year"])
        first = self.preview(clear_fields=["year", "summary", "year"], **options)
        second = self.preview(clear_fields=["summary", "year"], **options)
        self.assertEqual(first["items"], second["items"])
        self.assertNotEqual(first["plan_id"], second["plan_id"])
        with self.assertRaisesRegex(ConsumerError, "metadata_plan_changed"):
            self.apply(first, clear_fields=["summary", "year"], **options)
        self.assertEqual(self.events(), [])
        self.assertEqual(self.writes(), [])

    def test_review_exact_encoded_put_boundary(self):
        title = "x" * 8192
        params = {
            "id": "1",
            "type": 1,
            "summary.value": "",
            "summary.locked": 0,
            "title.value": title,
            "title.locked": 0,
        }
        remaining = 16384 - len(urlencode(params).encode())
        self.assertLessEqual(remaining + 1, 8192)
        self.curate(title=title, summary="y" * remaining)
        self.preview(fields=["title", "summary"])
        self.curate(title=title, summary="y" * (remaining + 1))
        with self.assertRaisesRegex(ConsumerError, "metadata_request_too_large"):
            self.preview(fields=["title", "summary"])
        self.assertEqual(self.events(), [])
        self.assertEqual(self.writes(), [])

    def test_review_discovery_root_tags_remain_unrestricted(self):
        def transform(route, body):
            if route not in ("/", "/library/sections"):
                return body
            root = ET.fromstring(body)
            root.tag = "{urn:fixture}ArbitraryContainer"
            return ET.tostring(root)

        self.wire_transform = transform
        self.assertTrue(self.apply(fields=["title"])["complete"])

    def test_review_foreign_field_and_location_children_are_ignored(self):
        ET.SubElement(self.node, "{urn:fixture}Field", name="title", locked="bogus")

        def transform(route, body):
            root = ET.fromstring(body)
            if route == "/library/sections":
                ET.SubElement(root[0], "{urn:fixture}Location", path="relative")
            return ET.tostring(root)

        self.wire_transform = transform
        result = self.apply(fields=["title"])
        self.assertTrue(result["complete"])
        self.assertFalse(result["items"][0]["remote"]["fields"]["title"]["locked"])

    def test_review_noop_creates_no_intent_or_write(self):
        self.curate(title="Plex title")
        before = self.contents(self.database)
        result = self.apply(fields=["title"])
        self.assertTrue(result["complete"])
        self.assertEqual(result["results"][0]["state"], "unchanged")
        self.assertEqual(self.contents(self.database), before)
        self.assertEqual(self.events(), [])
        self.assertEqual(self.writes(), [])

    def test_review_started_event_is_durable_before_remote_effect(self):
        self.compare_reference = False
        captured = []

        def observe(route, method):
            if method == "PUT":
                captured.append(self.events())
                self.assertEqual(self.node.get("title"), "Plex title")

        self.on_native = observe
        result = self.apply(fields=["title"])
        self.assertEqual(len(captured), 1)
        self.assertEqual(len(captured[0]), 1)
        event = captured[0][0]
        self.assertEqual(event["state"], "started")
        self.assertEqual(event["plan_id"], result["plan_id"])
        self.assertEqual(event["changes"], result["items"][0]["changes"])
        with Store(self.database) as store:
            rows = store.rows(
                "SELECT id,severity,subject FROM consumer_events WHERE event='metadata_publish'"
            )
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["id"], result["results"][0]["event_id"])
        self.assertEqual(rows[0]["severity"], "info")
        self.assertEqual(json.loads(rows[0]["subject"])["state"], "verified")

    def test_review_deleted_intent_cannot_be_acknowledged(self):
        # Native exact-row acknowledgement is deliberately stronger than the
        # frozen UPDATE (which does not inspect affected rows).
        self.compare_reference = False

        def delete_event():
            with (
                Store(self.database, writable=True) as store,
                store.transaction() as db,
            ):
                db.execute("DELETE FROM consumer_events WHERE event='metadata_publish'")

        self.after_put = delete_event
        preview = self.preview(fields=["title"])
        expected, error, _, _, contents = self.reference(
            dict(
                item_ids=[self.item_id],
                fields=["title"],
                apply=True,
                expected_plan=preview["plan_id"],
            )
        )
        self.assertIsNone(error)
        self.assertTrue(expected["complete"])
        self.assertEqual(expected["results"][0]["state"], "verified")
        self.assertEqual(contents["consumer_events"], [])
        result = subprocess.run(
            self.command(
                item_ids=[self.item_id],
                fields=["title"],
                apply=True,
                expected_plan=preview["plan_id"],
            ),
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("metadata_attempt_changed", result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertEqual(len(self.writes()), 1)
        self.assertEqual(self.events(), [])

    def test_review_put_redirect_is_uncertain_without_following(self):
        self.compare_reference = False
        fixture = self

        def redirect(handler):
            fixture.native_calls.append(("/library/sections/7/all", "PUT", None))
            handler.send_response(307)
            handler.send_header("Location", "/redirect-target")
            handler.send_header("Content-Length", "0")
            handler.end_headers()

        self.remote.RequestHandlerClass.do_PUT = redirect
        result = self.apply(fields=["title"])
        self.assertEqual(result["results"][0]["state"], "uncertain")
        self.assertEqual(result["results"][0]["error"]["code"], "temporarily_failed")
        self.assertFalse(result["results"][0]["error"]["safe_to_retry"])
        self.assertEqual(self.events()[0]["state"], "uncertain")
        self.assertEqual(sum(method == "PUT" for _, method, _ in self.native_calls), 1)
        self.assertNotIn(
            "/redirect-target", [route for route, _, _ in self.native_calls]
        )
        self.assertEqual(self.node.get("title"), "Plex title")


def load_tests(loader, tests, pattern):
    return unittest.TestSuite(
        IndependentMetadataReview(name)
        for name in sorted(IndependentMetadataReview.__dict__)
        if name.startswith("test_review_")
    )
