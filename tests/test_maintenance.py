# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Disposable end-to-end maintenance cycles and failure boundaries."""

import contextlib
import io
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.cli import main
from catabolic.curation import Curation
from catabolic.domain import CatabolicError
from catabolic.item_workflow import ItemWorkflow
from catabolic.layouts import PRESETS, Layouts
from catabolic.maintenance import render_report, run
from catabolic.processing import Processing
from catabolic.reconcile import Reconciler
from catabolic.store import Store
from tests.test_migrations import create_legacy


class MaintenanceTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.source = self.root / "source"
        self.source.mkdir()
        self.output = self.root / "output"
        self.output.mkdir()
        self.path = self.root / "catalog.sqlite3"
        Store.initialize(self.path)
        self.store = Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.app.bind("source", "media", str(self.source))
        self.app.bind("output", "global", str(self.output))
        self.source_file = self.source / "Notes.txt"
        self.source_file.write_text("An ordinary text document.\n")
        self.app.scan()
        self.file = self.app.files()["files"][0]["id"]
        self.item = self.app.put_item(
            "document", {"local": "notes"}, {"title": "Notes"}
        )["id"]
        self.app.media.associate(self.file, self.item)
        self.layouts = Layouts(self.app)
        self.layouts.put("native", PRESETS["catabolic"])
        self.layouts.run("native", apply=True)

    def links(self):
        return {
            str(path.relative_to(self.output)): (path.lstat().st_ino, os.readlink(path))
            for path in self.output.rglob("*")
            if path.is_symlink()
        }

    def test_cycle_discovers_files_updates_layout_and_refreshes_manifest(self):
        initial = run(self.app, manifest=True)
        self.assertTrue(initial["complete"], initial)
        original_links = self.links()
        (self.source / "Other.txt").write_text("More text\n")
        result = run(self.app, manifest=True)
        self.assertTrue(result["complete"], result)
        self.assertEqual(result["new_files"], 1)
        self.assertEqual(result["summary"]["files"]["uncataloged_present"], 1)
        self.assertEqual(original_links, self.links())
        other = self.app.files(unidentified=True)["files"][0]["id"]
        item = self.app.put_item("document", {"local": "other"}, {"title": "Other"})[
            "id"
        ]
        self.app.media.associate(other, item)
        ItemWorkflow(self.app).set_status(item, "complete", note="Reviewed")
        result = run(self.app, manifest=True, limit=1)
        self.assertTrue(result["complete"], result)
        self.assertEqual(len(self.links()), 2)
        self.assertEqual(result["summary"]["files"]["uncataloged"], 0)
        manifest = json.loads((self.output / ".catabolic-manifest.json").read_text())
        self.assertEqual(len(manifest["content"]["items"]), 2)
        synced = next(stage for stage in result["stages"] if stage["stage"] == "sync")
        self.assertEqual(synced["applied_count"], 1)

    def test_summary_counts_all_states_beyond_detail_limit_and_keeps_worklog(self):
        workflow = ItemWorkflow(self.app)
        workflow.set_status(self.item, "complete", note="Checked source")
        for state in ("pending", "in_progress", "deferred", "ignored", "complete"):
            item = self.app.put_item("document", {"fixture": state}, {"title": state})[
                "id"
            ]
            if state != "pending":
                workflow.set_status(item, state, note="Fixture")
        for n in range(105):
            (self.source / f"uncataloged-{n}.txt").write_text("text")
        self.source_file.unlink()
        journal = self.store.rows("SELECT * FROM item_worklog")
        result = run(self.app, inventory_only=True, limit=1)
        self.assertTrue(result["complete"], result)
        self.assertEqual(
            result["summary"]["items"]["by_status"],
            dict.fromkeys(
                (
                    "pending",
                    "in_progress",
                    "deferred",
                    "ignored",
                    "complete",
                    "needs_attention",
                ),
                1,
            ),
        )
        self.assertEqual(result["summary"]["items"]["incomplete"], 4)
        self.assertEqual(result["summary"]["files"]["uncataloged_present"], 105)
        self.assertEqual(result["summary"]["files"]["by_availability"]["missing"], 1)
        self.assertEqual(journal, self.store.rows("SELECT * FROM item_worklog"))

    def test_profile_availability_does_not_reclassify_global_associations(self):
        self.app.add_profile("other")
        other = Application(self.store, "other")
        self.source_file.unlink()
        result = run(other, inventory_only=True)
        self.assertFalse(result["complete"])
        self.assertEqual(result["summary"]["files"]["by_availability"]["unknown"], 1)
        self.assertEqual(result["summary"]["files"]["uncataloged"], 0)

    def test_multiple_sources_proposals_and_disabled_association_counts(self):
        second = self.root / "second-source"
        second.mkdir()
        (second / "Unknown.txt").write_text("Unidentified content\n")
        self.app.bind("source", "second", str(second))
        association = self.store.rows("SELECT id FROM item_files")[0]["id"]
        for mapping in self.store.rows("SELECT id FROM mappings WHERE active=1"):
            self.app.disable_mapping(mapping["id"])
        self.app.media.disable_association(association)
        Curation(self.app).put(
            self.file,
            {
                "item": {
                    "kind": "document",
                    "identities": {"fixture": "proposed"},
                    "metadata": {"title": "Candidate"},
                }
            },
        )
        result = run(self.app, inventory_only=True)
        self.assertTrue(result["complete"], result)
        self.assertEqual(result["new_files"], 1)
        self.assertEqual(result["summary"]["files"]["uncataloged_present"], 2)
        self.assertEqual(result["summary"]["proposals"]["pending"], 1)
        rendered = render_report(result)
        self.assertIn("2 uncataloged (2 present)", rendered)
        self.assertIn("Proposals: pending: 1", rendered)
        self.assertIn("Output verification: not run", rendered)

    def test_query_layout_refresh_and_empty_selection_protection(self):
        definition = {
            **PRESETS["catabolic"],
            "selection": {
                "language": "sql",
                "query": "SELECT item_id FROM catalog_items WHERE kind='document'",
            },
        }
        self.layouts.put("native", definition)
        self.assertTrue(run(self.app)["complete"])
        before = self.store.rows("SELECT * FROM mappings")
        links = self.links()
        self.layouts.put(
            "native",
            {
                **definition,
                "selection": {
                    "language": "sql",
                    "query": "SELECT item_id FROM catalog_items WHERE 0",
                },
            },
        )
        result = run(self.app, max_removals=100)
        self.assertFalse(result["complete"])
        self.assertEqual(result["stopped_at"], "planning")
        self.assertEqual(before, self.store.rows("SELECT * FROM mappings"))
        self.assertEqual(links, self.links())

    def test_legacy_database_requires_explicit_upgrade(self):
        legacy_root = self.root / "legacy"
        legacy_root.mkdir()
        database = create_legacy(legacy_root)
        before = database.read_bytes()
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = main(["--db", str(database), "--json", "maintenance"])
        self.assertEqual(code, 2)
        self.assertEqual(out.getvalue(), "")
        self.assertIn("upgrade", json.loads(err.getvalue())["error"]["message"])
        self.assertEqual(before, database.read_bytes())

    def test_offline_root_stops_before_scan_and_preserves_outputs(self):
        self.assertTrue(run(self.app)["complete"])
        links = self.links()
        before = self.store.rows("SELECT * FROM observations")
        self.source.rename(self.root / "offline")
        result = run(self.app)
        self.assertFalse(result["complete"])
        self.assertEqual(result["stopped_at"], "preflight")
        self.assertEqual(before, self.store.rows("SELECT * FROM observations"))
        self.assertEqual(links, self.links())

    def test_partial_scan_stops_processing_and_sync(self):
        before = self.store.rows("SELECT * FROM mappings")
        with (
            patch(
                "catabolic.app.walk_files",
                return_value=([], ["directory inaccessible"]),
            ),
            patch.object(Processing, "run") as processing,
        ):
            result = run(self.app, process="sniff")
        self.assertFalse(result["complete"])
        self.assertEqual(result["stopped_at"], "scan")
        processing.assert_not_called()
        self.assertEqual(before, self.store.rows("SELECT * FROM mappings"))
        self.assertEqual(list(self.output.iterdir()), [])

    def test_removal_budget_rolls_back_layout_changes_then_explicit_budget_succeeds(
        self,
    ):
        self.assertTrue(run(self.app)["complete"])
        before = self.store.rows("SELECT * FROM mappings")
        links = self.links()
        self.layouts.put(
            "native",
            {"version": 1, "rules": [{"name": "all", "path": "renamed/{file.name}"}]},
        )
        result = run(self.app)
        self.assertFalse(result["complete"])
        self.assertEqual(result["stopped_at"], "planning")
        self.assertEqual(before, self.store.rows("SELECT * FROM mappings"))
        self.assertEqual(links, self.links())
        result = run(self.app, max_removals=1, max_removal_percent=100)
        self.assertTrue(result["complete"], result)
        self.assertEqual(list(self.links()), ["renamed/Notes.txt"])

    def test_all_catalogs_preflight_prevents_partial_publication(self):
        foreign = self.root / "foreign"
        foreign.mkdir()
        self.app.bind("output", "second", str(foreign))
        self.layouts.run("native", "second", apply=True)
        (foreign / "personal.txt").write_text("Do not touch")
        result = run(self.app, catalog=None)
        self.assertFalse(result["complete"])
        self.assertEqual(list(self.output.iterdir()), [])
        self.assertEqual((foreign / "personal.txt").read_text(), "Do not touch")
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])

    def test_processing_is_stable_bounded_and_does_not_run_other_jobs(self):
        unrelated = Processing(self.app).enqueue("hash", file_ids=[self.file])[
            "queued"
        ][0]
        first = run(self.app, process="sniff", inventory_only=True)
        self.assertFalse(first["complete"])
        stage = next(s for s in first["stages"] if s["stage"] == "processing")
        self.assertEqual(stage["waiting_for_stability"], 1)
        for n in range(2):
            (self.source / f"extra-{n}.txt").write_text("Text content\n")
        second = run(self.app, process="sniff", settle=0, batch=1, inventory_only=True)
        stage = next(s for s in second["stages"] if s["stage"] == "processing")
        self.assertEqual(stage["processing"]["processed"], 1)
        self.assertEqual(stage["pending_beyond_batch"], 2)
        self.assertFalse(second["complete"])
        for _ in range(2):
            result = run(
                self.app, process="sniff", settle=0, batch=1, inventory_only=True
            )
        self.assertTrue(result["complete"], result)
        self.assertEqual(
            self.store.rows(
                "SELECT state FROM processing_jobs WHERE id=?", (unrelated,)
            )[0]["state"],
            "queued",
        )

    def test_failed_analysis_reports_backlog_and_skips_outputs(self):
        job = Processing(self.app).enqueue("sniff", file_ids=[self.file])["queued"][0]
        with self.store.transaction() as db:
            db.execute("UPDATE processing_jobs SET state='failed' WHERE id=?", (job,))
        result = run(self.app, process="sniff", settle=0)
        self.assertFalse(result["complete"])
        self.assertEqual(result["stopped_at"], "processing")
        self.assertEqual(result["summary"]["jobs"]["failed"], 1)
        self.assertEqual(list(self.output.iterdir()), [])

    def test_excluded_observations_are_not_processed(self):
        result = run(
            self.app,
            inventory_only=True,
            process="sniff",
            settle=0,
            exclude=["Notes.txt"],
        )
        self.assertTrue(result["complete"], result)
        self.assertEqual(self.store.rows("SELECT * FROM processing_jobs"), [])
        self.assertEqual(result["summary"]["files"]["by_availability"]["present"], 1)

    def test_unhealthy_verification_keeps_exact_totals_and_skips_manifest(self):
        from catabolic.manifest import Manifest

        verification = {
            "healthy": False,
            "catalogs": [
                {
                    "catalog": "global",
                    "healthy": False,
                    "verified_links": 0,
                    "issues": [{"reason": "changed externally"}] * 3,
                }
            ],
        }
        with (
            patch.object(Reconciler, "verify", return_value=verification),
            patch.object(Manifest, "write") as publish,
        ):
            result = run(self.app, manifest=True, limit=1)
        self.assertFalse(result["complete"])
        publish.assert_not_called()
        self.assertEqual(result["summary"]["outputs"]["catalogs"][0]["issue_count"], 3)
        stage = next(s for s in result["stages"] if s["stage"] == "verify")
        self.assertEqual(len(stage["catalogs"][0]["issues"]), 1)
        self.assertTrue(stage["catalogs"][0]["issues_truncated"])

    def test_pending_link_recovery_blocks_scan_and_returns_summary(self):
        def interrupt(*_args, **_kwargs):
            raise OSError("simulated crash")

        with patch.object(Reconciler, "_execute", side_effect=interrupt):
            first = run(self.app)
        self.assertFalse(first["complete"])
        self.assertTrue(self.store.rows("SELECT * FROM journal"))
        scans = self.store.rows("SELECT * FROM scans")
        second = run(self.app)
        self.assertEqual(second["stopped_at"], "preflight")
        self.assertIn("recover", second["errors"][0]["message"])
        self.assertEqual(scans, self.store.rows("SELECT * FROM scans"))
        self.assertEqual(second["summary"]["items"]["total"], 1)

    def test_final_hardlink_reference_survives_maintenance(self):
        self.app.bind("output", "global", link_mode="hardlink")
        self.assertTrue(run(self.app)["complete"])
        link = next(path for path in self.output.rglob("Notes.txt"))
        data = link.read_bytes()
        self.source_file.unlink()
        result = run(self.app, max_removals=100)
        self.assertFalse(result["complete"])
        self.assertEqual(link.stat().st_nlink, 1)
        self.assertEqual(link.read_bytes(), data)

    def test_validation_happens_before_scans(self):
        before = self.store.rows("SELECT * FROM scans")
        for options in (
            {"batch": 0},
            {"workers": 17},
            {"max_removals": -1},
            {"max_removal_percent": float("nan")},
            {"inventory_only": True, "manifest": True},
            {"exclude": ["../escape"]},
        ):
            with self.subTest(options=options), self.assertRaises(CatabolicError):
                run(self.app, **options)
        self.assertEqual(before, self.store.rows("SELECT * FROM scans"))

    def test_cli_json_success_and_blocked_cycle_exit_codes(self):
        self.store.close()

        def cli(*args):
            out, err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                code = main(["--db", str(self.path), "--json", "maintenance", *args])
            return code, json.loads(out.getvalue()), err.getvalue()

        code, result, progress = cli("--all-catalogs")
        self.assertEqual(code, 0, result)
        self.assertIn('"stage": "scan"', progress)
        self.assertEqual(result["summary"]["items"]["by_status"]["pending"], 1)
        self.source.rename(self.root / "offline")
        code, result, _ = cli()
        self.assertEqual(code, 3)
        self.assertFalse(result["inventory_updated"])
        self.assertIn("summary", result)
