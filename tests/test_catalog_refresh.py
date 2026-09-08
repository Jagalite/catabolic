# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import copy
import shutil
import unittest
from unittest.mock import patch

from catabolic.app import Application
from catabolic.catalog_refresh import CatalogRefresh, enqueue
from catabolic.layouts import PRESETS, Layouts
from catabolic.outputs import Outputs
from catabolic.processing import Processing
from catabolic.receipts import import_receipt, source_receipt
from catabolic.rendition_publication import Publication
from catabolic.store import Store
from tests import test_rendition_workflows, test_rules


class ReceiptRefreshTest(unittest.TestCase):
    setUp = test_rendition_workflows.ReceiptTest.setUp
    receipt = test_rendition_workflows.ReceiptTest.receipt

    def catalog(self, name="mobile", enable=True):
        root = self.root / name
        root.mkdir()
        self.app.bind("output", name, str(root))
        Layouts(self.app).put("native", copy.deepcopy(PRESETS["catabolic"]))
        Publication(self.app).put(name, {"purpose": "custom"})
        Layouts(self.app).run("native", name, apply=True)
        if enable:
            CatalogRefresh(self.app).configure(name, enabled=True)
        return root

    def test_import_updates_only_enabled_catalog_and_never_notifies_servers(self):
        with patch(
            "catabolic.network_adapters.Refresh.after_sync",
            side_effect=AssertionError("server refresh must not be queued"),
        ):
            mobile = self.catalog()
            from catabolic.network_adapters import Refresh

            Refresh(self.app).configure(
                "mobile", "http://127.0.0.1:9", "UNUSED_TEST_TOKEN"
            )
            self.catalog("other", enable=False)
            result = import_receipt(self.app, self.receipt())
        self.assertTrue(result["catalog_refresh"]["complete"])
        self.assertEqual(self.store.rows("SELECT * FROM refresh_dirty"), [])
        self.assertEqual(self.store.rows("SELECT * FROM refresh_events"), [])
        links = [p for p in mobile.rglob("*") if p.is_symlink()]
        self.assertEqual(len(links), 1)
        self.assertEqual(links[0].resolve(), self.root / "media/external.mkv")
        self.assertEqual(
            self.store.rows("SELECT * FROM mappings WHERE catalog='other'"), []
        )
        self.assertEqual(CatalogRefresh(self.app).pending()["pending"], [])

    def test_offline_catalog_retries_after_restart_without_rerender(self):
        mobile = self.catalog()
        offline = self.root / "offline"
        mobile.rename(offline)
        result = import_receipt(self.app, self.receipt())
        self.assertEqual(result["catalog_refresh"]["pending"], 1)
        self.assertEqual(
            self.store.rows("SELECT * FROM mappings WHERE catalog='mobile'"), []
        )
        self.store.close()
        offline.rename(mobile)
        with Store(self.path, writable=True) as store:
            app = Application(store)
            refresh = CatalogRefresh(app)
            self.assertEqual(refresh.run()["processed"], 0)  # durable retry backoff
            with patch(
                "catabolic.rendering.render",
                side_effect=AssertionError("must not render"),
            ):
                report = refresh.run(force=True)
            self.assertTrue(report["complete"], report)
            self.assertEqual(refresh.run(force=True)["processed"], 0)
        self.assertEqual(len([p for p in mobile.rglob("*") if p.is_symlink()]), 1)

    def test_events_coalesce_and_rollback_with_producer_transaction(self):
        self.catalog()
        with self.assertRaisesRegex(RuntimeError, "rollback"):
            with self.store.transaction() as db:
                enqueue(db, self.app.profile)
                raise RuntimeError("rollback")
        self.assertEqual(CatalogRefresh(self.app).pending()["pending"], [])
        with self.store.transaction() as db:
            enqueue(db, self.app.profile)
            enqueue(db, self.app.profile)
        pending = CatalogRefresh(self.app).pending()["pending"]
        self.assertEqual(len(pending), 1)
        self.assertEqual(pending[0]["generation"], 2)
        CatalogRefresh(self.app).configure("mobile", enabled=False)
        import_receipt(self.app, self.receipt())
        self.assertEqual(CatalogRefresh(self.app).pending()["pending"], [])

    def test_queue_generation_is_not_lost_if_new_event_arrives_during_sync(self):
        self.catalog()
        from catabolic.reconcile import Reconciler

        original = Reconciler.apply

        def event(reconciler, *args, **kwargs):
            result = original(reconciler, *args, **kwargs)
            with self.store.transaction() as db:
                enqueue(db, self.app.profile)
            return result

        with patch.object(Reconciler, "apply", event):
            result = import_receipt(self.app, self.receipt())
        self.assertEqual(result["catalog_refresh"]["pending"], 1)
        self.assertTrue(CatalogRefresh(self.app).run(force=True)["complete"])

    def test_interrupted_link_write_recovers_from_journal(self):
        mobile = self.catalog()
        from catabolic.reconcile import Reconciler

        original = Reconciler._execute

        def interrupted(reconciler, operation, **kwargs):
            def stop(_operation):
                raise SystemExit("simulated interruption")

            original(reconciler, operation, after_filesystem=stop)

        # Interrupt after journaled intent; replay completes with the normal
        # reconciler, including an already-created link or directory.
        with (
            patch.object(Reconciler, "_execute", interrupted),
            self.assertRaises(SystemExit),
        ):
            import_receipt(self.app, self.receipt())
        self.assertEqual(len(CatalogRefresh(self.app).pending()["pending"]), 1)
        self.assertTrue(self.store.rows("SELECT id FROM journal"))
        self.assertTrue(CatalogRefresh(self.app).run(force=True)["complete"])
        self.assertEqual(len([p for p in mobile.rglob("*") if p.is_symlink()]), 1)

    def test_watch_retries_queued_work_and_releases_writer_before_sleep(self):
        mobile = self.catalog()
        with (
            patch("catabolic.receipts.finish", side_effect=SystemExit("crash")),
            self.assertRaises(SystemExit),
        ):
            import_receipt(self.app, self.receipt())
        self.store.close()
        from catabolic.catalog_refresh_cli import dispatch
        from catabolic.cli import parser

        args = parser().parse_args(
            ["--db", str(self.path), "catalog-refresh", "watch", "--interval", "1"]
        )

        def idle(_seconds):
            with Store(self.path, writable=True) as store:
                self.assertEqual(store.rows("SELECT * FROM catalog_refresh_queue"), [])
            raise KeyboardInterrupt()

        with (
            patch("catabolic.catalog_refresh_cli.time.sleep", idle),
            self.assertRaises(KeyboardInterrupt),
        ):
            dispatch(args)
        self.assertEqual(len([p for p in mobile.rglob("*") if p.is_symlink()]), 1)

    def test_removal_budget_blocks_mapping_changes(self):
        mobile = self.root / "mobile"
        mobile.mkdir()
        self.app.bind("output", "mobile", str(mobile))
        layouts = Layouts(self.app)
        layouts.put("native", copy.deepcopy(PRESETS["catabolic"]))
        layouts.run("native", "mobile", apply=True)
        CatalogRefresh(self.app).configure("mobile", enabled=True)
        before = self.store.rows("SELECT * FROM mappings")
        Publication(self.app).put("mobile", {"purpose": "custom"})
        result = import_receipt(self.app, self.receipt())
        self.assertEqual(result["catalog_refresh"]["pending"], 1)
        self.assertEqual(before, self.store.rows("SELECT * FROM mappings"))
        enabled = CatalogRefresh(self.app).configure(
            "mobile", enabled=True, max_removals=1
        )
        self.assertTrue(enabled["catalog_refresh"]["complete"])


@unittest.skipUnless(
    shutil.which("ffmpeg") and shutil.which("ffprobe"), "real media tools required"
)
class RenderRefreshTest(unittest.TestCase):
    setUpClass = classmethod(test_rules.RuleTest.setUpClass.__func__)
    tearDownClass = classmethod(test_rules.RuleTest.tearDownClass.__func__)
    setUp = test_rules.RuleTest.setUp
    probe = test_rules.RuleTest.probe
    rule = test_rules.RuleTest.rule

    def catalog(self):
        mobile = self.root / "mobile"
        mobile.mkdir()
        self.app.bind("output", "mobile", str(mobile))
        Publication(self.app).put("mobile", {"purpose": "transcode"})
        layouts = Layouts(self.app)
        layouts.put("native", copy.deepcopy(PRESETS["catabolic"]))
        layouts.run("native", "mobile", apply=True)
        CatalogRefresh(self.app).configure("mobile", enabled=True)
        return mobile

    def test_render_completion_updates_links_without_separate_maintenance(self):
        mobile = self.catalog()
        self.probe()
        rule = self.rule("h264-720p")
        self.rules.apply(rule["id"])
        result = self.rules.run(rule["id"])
        self.assertTrue(result["complete"], result)
        self.assertTrue(result["execution"]["catalog_refresh"]["complete"])
        links = [p for p in mobile.rglob("*") if p.is_symlink()]
        self.assertEqual(len(links), 1)
        self.assertEqual(links[0].resolve().parent, self.generated)
        self.assertEqual(self.source_file.read_bytes(), self.payload)

    def test_maintenance_rule_stage_defers_automatic_drain(self):
        self.catalog()
        self.probe()
        rule = self.rule("h264-720p")
        self.rules.enable(rule["id"], True)
        from catabolic.rules import maintain

        with patch.object(
            CatalogRefresh,
            "run",
            side_effect=AssertionError(
                "maintenance must control its own refresh scope"
            ),
        ):
            result = maintain(self.app, render_batch=1)
        self.assertTrue(result["safe_to_continue"], result)
        self.assertEqual(len(CatalogRefresh(self.app).pending()["pending"]), 1)

    def test_external_transcode_waits_for_probe_then_updates_links(self):
        mobile = self.catalog()
        captured = source_receipt(self.app, self.file_id, self.item_id)
        output = self.source_file.parent / "external.mkv"
        output.write_bytes(self.payload)
        self.app.scan()
        definition = Outputs(self.app).define("external", {"purpose": "transcode"})
        receipt = {
            "version": 1,
            **captured,
            "producer": "fixture",
            "instance": "one",
            "job_id": "job",
            "attempt": "1",
            "configuration_digest": "a" * 64,
            "tools": {"fixture": "1"},
            "outcome": "complete",
            "outputs": [
                {
                    "location": "media",
                    "path": output.name,
                    "definition_id": definition["id"],
                    "size": output.stat().st_size,
                }
            ],
        }
        import_receipt(self.app, receipt)
        self.assertEqual([p for p in mobile.rglob("*") if p.is_symlink()], [])
        processor = Processing(self.app)
        processor.enqueue("probe", location="media")
        result = processor.run()
        self.assertTrue(result["complete"], result)
        self.assertTrue(result["catalog_refresh"]["complete"])
        self.assertEqual(len([p for p in mobile.rglob("*") if p.is_symlink()]), 1)

    def test_crash_after_render_commit_leaves_work_for_next_worker(self):
        mobile = self.catalog()
        self.probe()
        rule = self.rule("h264-720p")
        self.rules.apply(rule["id"])
        with (
            patch("catabolic.catalog_refresh.finish", side_effect=SystemExit("crash")),
            self.assertRaises(SystemExit),
        ):
            self.rules.run(rule["id"])
        self.assertEqual(len(CatalogRefresh(self.app).pending()["pending"]), 1)
        with patch(
            "catabolic.rendering.render",
            side_effect=AssertionError("must not rerender"),
        ):
            self.assertTrue(CatalogRefresh(self.app).run()["complete"])
        self.assertEqual(len([p for p in mobile.rglob("*") if p.is_symlink()]), 1)
