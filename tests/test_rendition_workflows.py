# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Catalog-isolated publication, receipt validation and semantic rule requirements."""

import copy
import hashlib
import os
import shutil
import unittest
from unittest.mock import patch

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.graphql_query import execute_graphql
from catabolic.item_workflow import ItemWorkflow
from catabolic.layouts import PRESETS, Layouts
from catabolic.manifest import Manifest
from catabolic.migration import load_migrations, upgrade_database
from catabolic.outputs import Outputs
from catabolic.processing import Processing
from catabolic.receipts import import_receipt, source_receipt
from catabolic.reconcile import Reconciler
from catabolic.rendition_publication import Publication
from catabolic.sql_query import execute_sql
from tests import test_outputs, test_rules
from tests.test_migrations import contents, create_legacy


class ReceiptTest(unittest.TestCase):
    def setUp(self):
        test_outputs.OutputTest.setUp(self)
        self.definition = self.outputs.define("external", {"purpose": "custom"})

    def receipt(self):
        return {
            "version": 1,
            **source_receipt(self.app, self.files["source.mkv"], self.item),
            "producer": "fixture-processor",
            "instance": "local",
            "job_id": "job-1",
            "attempt": "1",
            "configuration_digest": "a" * 64,
            "tools": {"fixture": "1.0"},
            "outcome": "complete",
            "outputs": [
                {
                    "location": "media",
                    "path": "external.mkv",
                    "definition_id": self.definition["id"],
                    "size": len(b"external.mkv"),
                }
            ],
        }

    def test_receipt_import_is_idempotent_and_queryable_without_fabricated_jobs(self):
        receipt = self.receipt()
        receipt["outputs"][0]["sha256"] = hashlib.sha256(b"external.mkv").hexdigest()
        first = import_receipt(self.app, receipt)
        self.assertEqual(
            first["output_ids"], import_receipt(self.app, receipt)["output_ids"]
        )
        self.assertEqual(self.store.rows("SELECT * FROM processing_jobs"), [])
        self.assertEqual(self.store.rows("SELECT * FROM processing_artifacts"), [])
        self.assertEqual(self.outputs.get(first["output_ids"][0])["purpose"], "custom")
        self.assertEqual(
            self.store.rows(
                "SELECT active FROM item_files WHERE file_id=?",
                (self.files["external.mkv"],),
            )[0]["active"],
            0,
        )
        self.assertEqual(
            execute_sql(self.path, "SELECT purpose,producer FROM catalog_renditions")[
                "rows"
            ],
            [["custom", "fixture-processor"]],
        )
        result = execute_graphql(self.path, "{ renditions { nodes } }")
        self.assertEqual(
            result["data"]["renditions"]["nodes"][0]["producer"], "fixture-processor"
        )
        changed = copy.deepcopy(receipt)
        changed["tools"]["fixture"] = "2.0"
        with self.assertRaisesRegex(CatabolicError, "conflicting"):
            import_receipt(self.app, changed)

    def test_receipt_cannot_create_lineage_cycle(self):
        self.app.media.associate(self.files["external.mkv"], self.item)
        self.outputs.register(
            self.files["source.mkv"],
            self.files["external.mkv"],
            self.item,
            self.definition["id"],
        )
        with self.assertRaisesRegex(CatabolicError, "cycle"):
            import_receipt(self.app, self.receipt())
        self.assertEqual(len(self.store.rows("SELECT * FROM media_outputs")), 1)
        self.assertEqual(self.store.rows("SELECT * FROM external_receipts"), [])

    def test_multi_output_receipt_rolls_back_all_on_bad_checksum(self):
        receipt = self.receipt()
        receipt["outputs"].append(
            {
                **receipt["outputs"][0],
                "path": "third.mkv",
                "size": len(b"third.mkv"),
                "sha256": "0" * 64,
            }
        )
        with self.assertRaisesRegex(CatabolicError, "checksum"):
            import_receipt(self.app, receipt)
        for table in ("media_outputs", "external_receipts", "receipt_outputs"):
            self.assertEqual(self.store.rows("SELECT * FROM " + table), [])

    def test_changed_source_and_unsafe_output_are_rejected(self):
        receipt = self.receipt()
        receipt["outputs"][0]["path"] = "../external.mkv"
        with self.assertRaises(CatabolicError):
            import_receipt(self.app, receipt)
        receipt = self.receipt()
        (self.root / "media/source.mkv").write_bytes(b"changed revision")
        with self.assertRaises(CatabolicError):
            import_receipt(self.app, receipt)
        self.assertEqual(self.store.rows("SELECT * FROM external_receipts"), [])

    def test_late_file_change_rolls_back_registration(self):
        receipt = self.receipt()
        original = Outputs.record

        def changed(adapter, *args, **kwargs):
            result = original(adapter, *args, **kwargs)
            (self.root / "media/external.mkv").write_bytes(
                b"changed during registration"
            )
            return result

        with (
            patch.object(Outputs, "record", changed),
            self.assertRaisesRegex(CatabolicError, "changed"),
        ):
            import_receipt(self.app, receipt)
        self.assertEqual(self.store.rows("SELECT * FROM media_outputs"), [])
        self.assertEqual(self.store.rows("SELECT * FROM external_receipts"), [])

    def test_external_publication_requires_receipt_and_honors_rejection(self):
        folder = self.root / "external-links"
        folder.mkdir()
        self.app.bind("output", "external", str(folder))
        publication = Publication(self.app)
        publication.put("external", {"purpose": "custom"})
        self.assertEqual(publication.candidates("external")[0], [])
        result = import_receipt(self.app, self.receipt())
        self.assertEqual(len(publication.candidates("external")[0]), 1)
        publication.decide("external", result["output_ids"][0], True, "Not wanted here")
        self.assertEqual(publication.candidates("external")[0], [])
        publication.decide("external", result["output_ids"][0], False, "Reviewed again")
        self.assertEqual(len(publication.candidates("external")[0]), 1)
        (self.root / "media/source.mkv").write_bytes(b"new source")
        self.assertEqual(publication.candidates("external")[0], [])

    def test_plain_external_registration_is_not_automatically_trusted(self):
        folder = self.root / "links"
        folder.mkdir()
        self.app.bind("output", "external", str(folder))
        self.outputs.register(
            self.files["external.mkv"],
            self.files["source.mkv"],
            self.item,
            self.definition["id"],
        )
        publication = Publication(self.app)
        publication.put("external", {"purpose": "custom"})
        rows, report = publication.candidates("external")
        self.assertEqual(rows, [])
        self.assertIn("no validated receipt", report["excluded"][0]["reason"])

    def test_publication_checks_live_revision_and_fresh_verification(self):
        folder = self.root / "links"
        folder.mkdir()
        self.app.bind("output", "external", str(folder))
        import_receipt(self.app, self.receipt())
        publication = Publication(self.app)
        publication.put("external", {"purpose": "custom"})
        processor = Processing(self.app)
        file_id = self.files["external.mkv"]
        processor.enqueue("hash", file_ids=[file_id])
        self.assertTrue(processor.run()["complete"])
        processor.enqueue("verify", file_ids=[file_id])
        self.assertTrue(processor.run()["complete"])
        path = self.root / "media/external.mkv"
        original = path.read_bytes()
        before = path.stat()
        path.write_bytes(b"X" * len(original))
        os.utime(path, ns=(before.st_atime_ns, before.st_mtime_ns))
        self.assertNotEqual(before.st_ctime_ns, path.stat().st_ctime_ns)
        # Even the old matching hash fact must not bypass the live revision.
        self.assertEqual(publication.candidates("external")[0], [])
        processor.enqueue("verify", file_ids=[file_id])
        self.assertEqual(processor.run()["counts"], {"mismatch": 1})
        self.assertEqual(publication.candidates("external")[0], [])
        path.write_bytes(original)
        os.utime(path, ns=(before.st_atime_ns, before.st_mtime_ns))
        self.assertEqual(publication.candidates("external")[0], [])
        processor.enqueue("verify", file_ids=[file_id])
        self.assertTrue(processor.run()["complete"])
        self.assertEqual(len(publication.candidates("external")[0]), 1)

    def test_other_profile_rendition_is_not_an_original(self):
        import_receipt(self.app, self.receipt())
        self.app.media.associate(self.files["external.mkv"], self.item)
        self.app.add_profile("other")
        other = Application(self.store, "other")
        other.bind("source", "media", str(self.root / "media"))
        other.scan()
        folder = self.root / "other-links"
        folder.mkdir()
        other.bind("output", "external", str(folder))
        Publication(other).put(
            "external", {"purpose": "transcode", "include_originals": True}
        )
        layouts = Layouts(other)
        layouts.put("native", copy.deepcopy(PRESETS["catabolic"]))
        result = layouts.run("native", "external", apply=True)
        self.assertTrue(result["safe"], result)
        self.assertEqual(result["desired_count"], 1)
        self.assertEqual(
            self.store.rows("SELECT file_id FROM mappings WHERE active=1"),
            [{"file_id": self.files["source.mkv"]}],
        )

    def test_large_filtered_inventory_refuses_to_disable_existing_mapping(self):
        folder = self.root / "links"
        folder.mkdir()
        self.app.bind("output", "external", str(folder))
        layouts = Layouts(self.app)
        layouts.put("native", copy.deepcopy(PRESETS["catabolic"]))
        layouts.run("native", "external", apply=True)
        before = self.store.rows("SELECT * FROM mappings")
        import_receipt(self.app, self.receipt())
        Publication(self.app).put(
            "external", {"purpose": "transcode", "include_originals": True}
        )
        # The first 100001 active rows are excluded renditions. The original
        # sorts after them, outside the planner's bounded inventory query.
        with self.store.transaction() as db:
            db.execute("UPDATE item_files SET id='z-original' WHERE active=1")
            db.executemany(
                "INSERT INTO items VALUES (?,'movie','{\"title\":\"Fixture\"}')",
                [(f"bulk-{i}",) for i in range(100001)],
            )
            db.executemany(
                "INSERT INTO item_files(id,file_id,item_id,role,origin,active) VALUES (?,?,?,'primary','explicit',1)",
                [
                    (f"bulk-{i:06d}", self.files["external.mkv"], f"bulk-{i}")
                    for i in range(100001)
                ],
            )
        with self.assertRaisesRegex(CatabolicError, "100000"):
            layouts.run("native", "external", apply=True, allow_empty=True)
        self.assertEqual(self.store.rows("SELECT * FROM mappings"), before)

    def test_upgrade_preserves_populated_schema_eleven(self):
        (self.root / "legacy").mkdir()
        path = create_legacy(self.root / "legacy")
        upgrade_database(path, migrations=load_migrations()[:11])
        before = contents(path)
        upgrade_database(path)
        after = contents(path)
        for table, rows in before.items():
            if table != "schema_migrations":
                self.assertEqual(rows, after[table], table)


@unittest.skipUnless(
    shutil.which("ffmpeg") and shutil.which("ffprobe"), "real media tools required"
)
class PublicationTest(unittest.TestCase):
    setUpClass = classmethod(test_rules.RuleTest.setUpClass.__func__)
    tearDownClass = classmethod(test_rules.RuleTest.tearDownClass.__func__)
    setUp = test_rules.RuleTest.setUp
    probe = test_rules.RuleTest.probe
    rule = test_rules.RuleTest.rule

    def catalogs(self):
        layouts = Layouts(self.app)
        layouts.put("native", copy.deepcopy(PRESETS["catabolic"]))
        for name in ("originals", "mobile"):
            directory = self.root / name
            directory.mkdir()
            self.app.bind("output", name, str(directory))
        return layouts

    def test_render_publish_manifest_without_global_activation(self):
        self.probe()
        rule = self.rule("h264-720p")
        self.rules.apply(rule["id"])
        result = self.rules.run(rule["id"])
        self.assertTrue(result["complete"], result)
        output = Outputs(self.app).list()["outputs"][0]
        self.assertEqual(output["purpose"], "transcode")
        layouts = self.catalogs()
        Publication(self.app).put(
            "mobile", {"rule_id": rule["id"], "purpose": "transcode"}
        )
        for catalog in ("originals", "mobile"):
            plan = layouts.run("native", catalog, apply=True)
            self.assertTrue(plan["safe"], plan)
            self.assertEqual(plan["desired_count"], 1)
            Reconciler(self.app).apply(catalog)
            self.assertTrue(Reconciler(self.app).verify(catalog)["healthy"])
        rows = self.store.rows(
            "SELECT catalog,file_id FROM mappings WHERE active=1 ORDER BY catalog"
        )
        self.assertEqual(
            rows,
            [
                {"catalog": "mobile", "file_id": output["file_id"]},
                {"catalog": "originals", "file_id": self.file_id},
            ],
        )
        self.assertEqual(
            self.store.rows(
                "SELECT active FROM item_files WHERE file_id=?", (output["file_id"],)
            )[0]["active"],
            0,
        )
        with self.store.transaction():
            manifest = Manifest(self.app).build("mobile")
        self.assertTrue(manifest["content"]["associations"][0]["active"])
        self.assertEqual(layouts.run("native", "mobile", apply=True)["change_count"], 0)
        stats = self.rules.statistics(rule["id"])
        self.assertGreater(stats["attempts"][0]["output_bytes"], 0)
        self.assertEqual(self.source_file.read_bytes(), self.payload)

    def test_query_can_select_inactive_admitted_renditions_only_in_authorized_catalog(
        self,
    ):
        self.probe()
        rule = self.rule("h264-720p")
        self.rules.apply(rule["id"])
        self.rules.run(rule["id"])
        layouts = self.catalogs()
        definition = copy.deepcopy(PRESETS["catabolic"])
        definition["selection"] = {
            "language": "sql",
            "profile": "default",
            "query": "SELECT file_id FROM catalog_renditions WHERE purpose='transcode' AND profile=:profile",
        }
        layouts.put("transcodes", definition)
        self.assertEqual(layouts.run("transcodes", "originals")["desired_count"], 0)
        Publication(self.app).put("mobile", {"purpose": "transcode"})
        self.assertEqual(layouts.run("transcodes", "mobile")["desired_count"], 1)

    def test_required_deferred_match_blocks_and_retry_satisfies_one_requirement(self):
        rule = self.rule(required=True)
        workflow = ItemWorkflow(self.app)
        self.rules.apply(rule["id"])
        with self.assertRaisesRegex(CatabolicError, "cannot mark"):
            workflow.set_status(self.item_id, "complete")
        self.probe()
        job = self.rules.apply(rule["id"])["queued"][0]["job_id"]
        Processing(self.app).cancel(job)
        retry = self.rules.apply(rule["id"], retry_failed=True)
        self.assertNotEqual(retry["queued"][0]["job_id"], job)
        self.assertTrue(self.rules.run(rule["id"])["complete"])
        self.assertEqual(len(self.store.rows("SELECT * FROM rule_requirements")), 1)
        workflow.set_status(self.item_id, "complete")
        self.assertEqual(workflow.get(self.item_id)["status"], "complete")
        self.assertEqual(
            self.store.rows("SELECT state FROM processing_jobs WHERE id=?", (job,))[0][
                "state"
            ],
            "cancelled",
        )
        self.assertTrue(
            any(
                "previous_job_id" in entry["data"]
                for entry in workflow.log(self.item_id)["entries"]
            )
        )

    def test_rule_requirement_can_be_waived_without_deleting_history(self):
        rule = self.rule(required=True)
        self.rules.apply(rule["id"])
        workflow = ItemWorkflow(self.app)
        requirement = self.store.rows("SELECT id FROM rule_requirements")[0]["id"]
        workflow.resolve(requirement, "waived", note="No mobile version needed")
        self.rules.apply(rule["id"])
        workflow.set_status(self.item_id, "complete")
        self.assertEqual(
            self.store.rows("SELECT state FROM rule_requirements")[0]["state"], "waived"
        )

    def test_recipe_size_acceptance_fails_without_publishing(self):
        recipe = self.a.recipe(
            "tiny-ratio",
            "h264-720p",
            {"max_size_ratio": 0.00001, "require_duration": True},
        )
        self.a.enqueue(self.file_id, recipe["id"], "generated", self.item_id)
        result = self.a.run()
        self.assertFalse(result["complete"])
        self.assertIn("size ratio", result["errors"][0]["error"])
        self.assertEqual(self.store.rows("SELECT * FROM media_outputs"), [])
        self.assertEqual(self.source_file.read_bytes(), self.payload)
