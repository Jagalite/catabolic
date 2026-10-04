# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import hashlib
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.consumer_adapters import ConsumerError
from catabolic.destination_mappings import events, recover, run
from catabolic.domain import CatabolicError
from catabolic.mapping_adapters import capabilities
from catabolic.process_runner import CommandFailure
from catabolic.saved_queries import Queries
from catabolic.store import Store
from catabolic.targets import definitions
from tests.test_media_model import populate_media


class PublicationMappingsTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.database = self.root / "catalog.sqlite3"
        Store.initialize(self.database)
        with Store(self.database, writable=True) as store:
            app = Application(store)
            self.files, _ = populate_media(app, self.root)
            for identifier, kind in (
                ("movie", "movie"),
                ("edition", "book_edition"),
                ("audio", "audiobook"),
                ("photo", "photo"),
            ):
                app.put_item(
                    kind,
                    {},
                    {
                        "title": "Title & Café",
                        "year": 2026,
                        "author": "Author",
                        "album": "Photos",
                    },
                    identifier,
                )

            app.media.associate(
                self.files["movie.srt"],
                "movie",
                role="subtitle",
                metadata={"language": "en"},
            )
        self.before = self.source_state()
        credential = patch.dict(os.environ, {"IMMICH_API_KEY": "fixture-secret"})
        credential.start()
        self.addCleanup(credential.stop)
        remote = patch(
            "catabolic.publication_mappings.request", return_value=b'{"id":"user-a"}'
        )
        self.identity_request = remote.start()
        self.addCleanup(remote.stop)

    def source_state(self):
        return {
            str(p): (p.stat().st_ino, hashlib.sha256(p.read_bytes()).hexdigest())
            for p in (self.root / "media").rglob("*")
            if p.is_file()
        }

    def definition(
        self, adapter, operation, *, sql=None, catalog="global", output=None
    ):
        sql = sql or "SELECT item_id FROM catalog_items"
        with Store(self.database, writable=True) as store:
            query = Queries(store, "default").put(
                "mapping-input",
                {
                    "version": 1,
                    "mode": "selection",
                    "entity": "item_id",
                    "selection": {"language": "sql", "query": sql},
                },
            )
        dest = {"adapter": adapter, "operation": operation, "catalog": catalog}
        if output is not None:
            dest["path"] = str(output)
        return {
            "version": 2,
            "id": adapter + "-test",
            "query": query["id"],
            "destination": dest,
        }

    def apply(self, definition, **options):
        preview = run(self.database, "default", definition)
        self.assertTrue(preview["safe"], preview)
        return run(
            self.database,
            "default",
            definition,
            apply=True,
            expected_plan=preview["plan_id"],
            **options,
        )

    def test_all_targets_have_truthful_operations(self):
        all_caps = capabilities()
        self.assertEqual(len(all_caps), 23)
        for adapter in definitions():
            self.assertIn("folder", all_caps[adapter]["operations"])
        for adapter in ("calibre", "calibre-web", "immich"):
            self.assertFalse(
                all_caps[adapter]["operations"]["import"]["metadata_updates"]
            )
        for adapter in ("nfo", "opds", "xspf"):
            self.assertIn("export", all_caps[adapter]["operations"])
        self.assertIn("metadata", all_caps["plex"]["operations"])
        self.assertNotIn("metadata", all_caps["emby"]["operations"])

    def test_all_seventeen_folder_targets_preview_apply_and_repeat(self):
        for adapter in definitions():
            with self.subTest(adapter=adapter):
                directory = self.root / adapter
                directory.mkdir()
                with Store(self.database, writable=True) as store:
                    Application(store).bind("output", adapter, str(directory))
                d = self.definition(adapter, "folder", catalog=adapter)
                preview = run(self.database, "default", d)
                self.assertEqual(list(directory.iterdir()), [])
                with Store(self.database) as store:
                    self.assertFalse(
                        store.rows(
                            "SELECT * FROM projection_bindings WHERE catalog=?",
                            (adapter,),
                        )
                    )
                applied = run(
                    self.database,
                    "default",
                    d,
                    apply=True,
                    expected_plan=preview["plan_id"],
                )
                self.assertTrue(applied["complete"], applied)
                self.assertTrue(applied["verification"]["healthy"])
                repeated = self.apply(d)
                self.assertEqual(repeated["execution"]["applied"], [])
        self.assertEqual(self.source_state(), self.before)

    def test_three_export_formats_query_selection_and_readback(self):
        for adapter, item in (
            ("nfo", "movie"),
            ("opds", "edition"),
            ("xspf", "track1"),
        ):
            with self.subTest(adapter=adapter):
                output = self.root / (adapter + "-bundle")
                d = self.definition(
                    adapter,
                    "export",
                    sql="SELECT item_id FROM catalog_items WHERE item_id='"
                    + item
                    + "'",
                    output=output,
                )
                if adapter == "opds":
                    d["destination"]["base_url"] = "https://books.example/"
                p = run(self.database, "default", d)
                self.assertFalse(output.exists())
                self.assertEqual(
                    {e["item_id"] for e in p["plan"]["document"]["content"]["entries"]},
                    {item},
                )
                r = run(
                    self.database, "default", d, apply=True, expected_plan=p["plan_id"]
                )
                self.assertTrue(r["complete"], r)
                self.assertTrue((output / "catalog-manifest.json").is_file())
                self.assertTrue(self.apply(d)["unchanged"])
        self.assertEqual(self.source_state(), self.before)

    def test_export_refuses_unowned_existing_path_and_tampering(self):
        output = self.root / "bundle"
        d = self.definition(
            "xspf",
            "export",
            sql="SELECT item_id FROM catalog_items WHERE item_id='track1'",
            output=output,
        )
        output.mkdir()
        with self.assertRaisesRegex(ConsumerError, "destination_exists"):
            run(self.database, "default", d)
        output.rmdir()
        self.assertTrue(self.apply(d)["complete"])
        (output / "catalog.xspf").write_text("manual change")
        with self.assertRaisesRegex(ConsumerError, "owned_export_changed"):
            run(self.database, "default", d)
        self.assertEqual((output / "catalog.xspf").read_text(), "manual change")

    def test_all_import_adapters_stage_copies_and_do_not_repeat(self):
        calls = []

        def command(argv, **kwargs):
            calls.append(argv)
            kwargs["on_start"]()
            source = Path(argv[-1])
            staged = list(source.iterdir()) if source.is_dir() else [source]
            self.assertTrue(staged)
            for p in staged:
                self.assertFalse(p.is_symlink())
                self.assertTrue(p.read_bytes().startswith(b"synthetic media"))

        with (
            patch(
                "catabolic.importers.shutil.which",
                side_effect=lambda executable: "/fixture/" + executable,
            ),
            patch("catabolic.importers.command_output", side_effect=command),
            patch.dict(os.environ, {"IMMICH_API_KEY": "fixture-secret"}),
        ):
            for adapter, item in (
                ("calibre", "edition"),
                ("calibre-web", "edition"),
                ("immich", "photo"),
            ):
                with self.subTest(adapter=adapter):
                    output = (
                        "https://immich.example"
                        if adapter == "immich"
                        else self.root / adapter
                    )
                    d = self.definition(
                        adapter,
                        "import",
                        sql="SELECT item_id FROM catalog_items WHERE item_id='"
                        + item
                        + "'",
                        output=output,
                    )
                    count = len(calls)
                    p = run(self.database, "default", d)
                    self.assertEqual(len(calls), count)
                    r = run(
                        self.database,
                        "default",
                        d,
                        apply=True,
                        expected_plan=p["plan_id"],
                    )
                    self.assertTrue(r["complete"], r)
                    self.assertEqual(r["results"][0]["state"], "submitted")
                    self.assertEqual(len(calls), count + 1)
                    self.assertTrue(self.apply(d)["results"][0]["unchanged"])
                    self.assertEqual(len(calls), count + 1)
                    self.assertNotIn(
                        "fixture-secret", json.dumps(events(self.database, "default"))
                    )
        self.assertEqual(self.source_state(), self.before)

    def test_failed_import_fences_replay_and_requires_reviewed_resolution(self):
        d = self.definition(
            "immich",
            "import",
            sql="SELECT item_id FROM catalog_items WHERE item_id='photo'",
            output="https://immich.example",
        )

        def failed(argv, **kwargs):
            kwargs["on_start"]()
            raise CommandFailure("timeout", "lost response")

        with (
            patch("catabolic.importers.shutil.which", return_value="/fixture/immich"),
            patch("catabolic.importers.command_output", side_effect=failed),
            patch.dict(os.environ, {"IMMICH_API_KEY": "fixture-secret"}),
        ):
            result = self.apply(d)
        self.assertFalse(result["complete"])
        with self.assertRaisesRegex(ConsumerError, "unresolved_write"):
            run(self.database, "default", d)
        identifier = result["event_id"]
        p = recover(self.database, "default", identifier)
        self.assertFalse(p["complete"])
        self.identity_request.return_value = b'{"id":"user-b"}'
        with self.assertRaisesRegex(
            ConsumerError, "import_destination_identity_changed"
        ):
            recover(
                self.database,
                "default",
                identifier,
                apply=True,
                resolution="submitted",
                expected_plan=p["plan_id"],
            )
        self.identity_request.return_value = b'{"id":"user-a"}'
        with self.assertRaisesRegex(ConsumerError, "requires_expected_plan"):
            recover(
                self.database, "default", identifier, apply=True, resolution="submitted"
            )
        result = recover(
            self.database,
            "default",
            identifier,
            apply=True,
            resolution="submitted",
            expected_plan=p["plan_id"],
        )
        self.assertTrue(result["complete"])
        self.assertTrue(self.apply(d)["results"][0]["unchanged"])

    def test_immich_account_scope_and_key_rotation(self):
        d = self.definition(
            "immich",
            "import",
            sql="SELECT item_id FROM catalog_items WHERE item_id='photo'",
            output="https://immich.example",
        )
        with (
            patch("catabolic.importers.shutil.which", return_value="/fixture/immich"),
            patch("catabolic.importers.command_output") as command,
        ):
            self.assertTrue(self.apply(d)["complete"])
            with patch.dict(os.environ, {"IMMICH_API_KEY": "rotated-secret"}):
                self.assertTrue(self.apply(d)["results"][0]["unchanged"])
            self.assertEqual(command.call_count, 1)
            old = run(self.database, "default", d)
            self.identity_request.return_value = b'{"id":"user-b"}'
            with self.assertRaisesRegex(ConsumerError, "stale_plan"):
                run(
                    self.database,
                    "default",
                    d,
                    apply=True,
                    expected_plan=old["plan_id"],
                )
            self.assertTrue(self.apply(d)["complete"])
            self.assertEqual(command.call_count, 2)
            self.assertEqual(
                {e["scope"][-1] for e in events(self.database, "default")},
                {"user-a", "user-b"},
            )
            self.assertNotIn(
                "fixture-secret", json.dumps(events(self.database, "default"))
            )
        args, kwargs = self.identity_request.call_args
        self.assertEqual(args[0], "https://immich.example/api/users/me")
        self.assertEqual(kwargs["headers"]["x-api-key"], "fixture-secret")

    def test_immich_identity_failure_never_launches(self):
        d = self.definition(
            "immich",
            "import",
            sql="SELECT item_id FROM catalog_items WHERE item_id='photo'",
            output="https://immich.example/api",
        )
        with patch("catabolic.importers.command_output") as command:
            for response in (b"{}", b"[]", b"not-json"):
                self.identity_request.return_value = response
                with self.assertRaisesRegex(
                    ConsumerError, "invalid_import_account_identity"
                ):
                    run(self.database, "default", d)
            command.assert_not_called()
        with patch.dict(os.environ, {}, clear=True):
            with self.assertRaisesRegex(CatabolicError, "IMMICH_API_KEY"):
                run(self.database, "default", d)
        self.assertFalse(events(self.database, "default"))

    def test_legacy_immich_history_is_not_assigned_to_current_user(self):
        from catabolic.destination_mappings import write_event

        d = self.definition(
            "immich",
            "import",
            sql="SELECT item_id FROM catalog_items WHERE item_id='photo'",
            output="https://immich.example",
        )
        with (
            patch("catabolic.importers.shutil.which", return_value="/fixture/immich"),
            patch("catabolic.importers.command_output") as command,
        ):
            self.apply(d)
            event = events(self.database, "default")[-1]
            identifier = event.pop("id")
            event["scope"] = event["scope"][:3]
            event["destination_identity"] = None
            with Store(self.database, writable=True) as store:
                write_event(store, "default", identifier, event)
            with self.assertRaisesRegex(
                ConsumerError, "legacy_import_account_identity_unknown"
            ):
                self.apply(d)
            self.assertEqual(command.call_count, 1)

    def test_complete_export_recovery_revalidates_sources(self):
        from catabolic.publication_mappings import publish_bundle

        for change in ("modify", "delete"):
            with self.subTest(change=change):
                d = self.definition(
                    "xspf",
                    "export",
                    sql="SELECT item_id FROM catalog_items WHERE item_id='track1'",
                    output=self.root / ("bundle-" + change),
                )

                def interrupted(*args, **kwargs):
                    publish_bundle(*args, **kwargs)
                    raise KeyboardInterrupt()

                with patch(
                    "catabolic.publication_mappings.publish_bundle",
                    side_effect=interrupted,
                ):
                    with self.assertRaises(KeyboardInterrupt):
                        self.apply(d)
                event = events(self.database, "default")[-1]
                source = next(iter(event["data"]["sources"].values()))
                path = Path(source["root"]) / source["path"]
                backup = path.with_suffix(".original")
                path.rename(backup)
                if change == "modify":
                    path.write_bytes(b"changed media")
                try:
                    for apply in (False, True):
                        with self.assertRaises((CatabolicError, OSError)):
                            recover(self.database, "default", event["id"], apply=apply)
                    self.assertEqual(
                        events(self.database, "default")[-1]["state"], "started"
                    )
                finally:
                    if path.exists():
                        path.unlink()
                    backup.rename(path)
                # The next subtest needs a fresh recorded source revision.
                with Store(self.database, writable=True) as store:
                    Application(store).scan()

    def test_stale_source_and_unsupported_operation_fail_before_writes(self):
        d = self.definition(
            "xspf",
            "export",
            sql="SELECT item_id FROM catalog_items WHERE item_id='track1'",
            output=self.root / "bundle",
        )
        p = run(self.database, "default", d)
        (self.root / "media/track01.flac").write_bytes(b"changed")
        with self.assertRaises(CatabolicError):
            run(self.database, "default", d, apply=True, expected_plan=p["plan_id"])
        self.assertFalse((self.root / "bundle").exists())
        d["destination"]["operation"] = "metadata"
        with self.assertRaisesRegex(ConsumerError, "unsupported_destination_operation"):
            run(self.database, "default", d)

    def folder_definition(
        self,
        sql="SELECT item_id FROM catalog_items WHERE item_id IN ('movie','track1')",
    ):
        output = self.root / "mapped"
        output.mkdir(exist_ok=True)
        with Store(self.database, writable=True) as store:
            Application(store).bind("output", "mapped", str(output))
        return self.definition("emby", "folder", sql=sql, catalog="mapped")

    def test_folder_removal_budget_and_manual_files(self):
        d = self.folder_definition()
        self.assertTrue(self.apply(d)["complete"])
        manual = self.root / "mapped/manual.txt"
        manual.write_text("keep")
        d2 = self.folder_definition(
            "SELECT item_id FROM catalog_items WHERE item_id='movie'"
        )
        p = run(self.database, "default", d2)
        self.assertEqual(p["removals"], 1)
        with self.assertRaisesRegex(ConsumerError, "removal_budget"):
            run(self.database, "default", d2, apply=True, expected_plan=p["plan_id"])
        self.assertTrue(self.apply(d2, max_removals=1)["complete"])
        self.assertEqual(manual.read_text(), "keep")
        other = {**d2, "id": "other-owner"}
        with self.assertRaisesRegex(ConsumerError, "another_projection"):
            run(self.database, "default", other)

    def test_folder_interruption_recovery(self):
        from catabolic.reconcile import Reconciler

        d = self.folder_definition()
        original = Reconciler.apply

        def interrupted(reconciler, *args, **kwargs):
            def stop(_):
                raise KeyboardInterrupt()

            return original(reconciler, *args, **kwargs, after_filesystem=stop)

        with (
            patch.object(Reconciler, "apply", interrupted),
            self.assertRaises(KeyboardInterrupt),
        ):
            self.apply(d)
        event = events(self.database, "default")[-1]
        self.assertEqual(event["state"], "started")
        result = recover(self.database, "default", event["id"], apply=True)
        self.assertTrue(result["complete"], result)
        self.assertTrue(self.apply(d)["complete"])

    def test_partial_export_resumes_only_missing_files(self):
        output = self.root / "bundle"
        d = self.definition(
            "xspf",
            "export",
            sql="SELECT item_id FROM catalog_items WHERE item_id='track1'",
            output=output,
        )

        def interrupted(app, document, export, path, *, on_created):
            directory = Path(path)
            directory.mkdir()
            st = directory.stat()
            on_created({"device": st.st_dev, "inode": st.st_ino})
            raise KeyboardInterrupt()

        with (
            patch(
                "catabolic.publication_mappings.publish_bundle", side_effect=interrupted
            ),
            self.assertRaises(KeyboardInterrupt),
        ):
            self.apply(d)
        event = events(self.database, "default")[-1]
        self.assertFalse(recover(self.database, "default", event["id"])["complete"])
        result = recover(self.database, "default", event["id"], apply=True)
        self.assertTrue(result["complete"], result)
        self.assertTrue((output / "catalog.xspf").exists())
        self.assertTrue(self.apply(d)["unchanged"])
        self.assertEqual(self.source_state(), self.before)

    def test_partial_export_tampering_never_overwritten(self):
        output = self.root / "bundle"
        d = self.definition(
            "xspf",
            "export",
            sql="SELECT item_id FROM catalog_items WHERE item_id='track1'",
            output=output,
        )

        def interrupted(app, document, export, path, *, on_created):
            directory = Path(path)
            directory.mkdir()
            st = directory.stat()
            on_created({"device": st.st_dev, "inode": st.st_ino})
            (directory / "catalog.xspf").write_text("external edit")
            raise OSError("interrupted")

        with patch(
            "catabolic.publication_mappings.publish_bundle", side_effect=interrupted
        ):
            result = self.apply(d)
        self.assertFalse(result["complete"])
        r = recover(self.database, "default", result["event_id"], apply=True)
        self.assertFalse(r["complete"])
        self.assertEqual((output / "catalog.xspf").read_text(), "external edit")

    def test_import_partial_batch_does_not_repeat_completed_groups(self):
        d = self.definition(
            "immich",
            "import",
            sql="SELECT item_id FROM catalog_items WHERE item_id IN ('photo','derivative')",
            output="https://immich.example",
        )
        calls = []

        def command(argv, **kwargs):
            calls.append(argv)
            kwargs["on_start"]()
            if len(calls) == 2:
                raise CommandFailure("timeout", "uncertain")

        with (
            patch("catabolic.importers.shutil.which", return_value="/fixture/immich"),
            patch("catabolic.importers.command_output", side_effect=command),
            patch.dict(os.environ, {"IMMICH_API_KEY": "fixture-secret"}),
        ):
            result = self.apply(d)
            self.assertFalse(result["complete"])
            self.assertEqual(
                [r["state"] for r in result["results"]], ["submitted", "uncertain"]
            )
            identifier = result["event_id"]
            p = recover(self.database, "default", identifier)
            recover(
                self.database,
                "default",
                identifier,
                apply=True,
                resolution="not_applied",
                expected_plan=p["plan_id"],
            )
            result = self.apply(d)
            self.assertTrue(result["complete"])
            self.assertEqual(len(calls), 3)

    def test_export_metadata_change_invalidates_reviewed_plan(self):
        output = self.root / "bundle"
        d = self.definition(
            "xspf",
            "export",
            sql="SELECT item_id FROM catalog_items WHERE item_id='track1'",
            output=output,
        )
        p = run(self.database, "default", d)
        with Store(self.database, writable=True) as store:
            Application(store).put_item("track", {}, {"title": "Changed"}, "track1")
        with self.assertRaisesRegex(ConsumerError, "stale_plan"):
            run(self.database, "default", d, apply=True, expected_plan=p["plan_id"])
        self.assertFalse(output.exists())

    def test_import_unsupported_selection_and_missing_cli_do_not_launch(self):
        d = self.definition(
            "calibre",
            "import",
            sql="SELECT item_id FROM catalog_items WHERE item_id='movie'",
            output=self.root / "books",
        )
        with self.assertRaisesRegex(ConsumerError, "unsupported_import_selection"):
            run(self.database, "default", d)
        d = self.definition(
            "calibre",
            "import",
            sql="SELECT item_id FROM catalog_items WHERE item_id='edition'",
            output=self.root / "books",
        )
        with (
            patch("catabolic.importers.shutil.which", return_value=None),
            self.assertRaisesRegex(ConsumerError, "import_cli_missing"),
        ):
            self.apply(d)
        self.assertFalse(events(self.database, "default"))

    def test_import_rescan_and_untransmitted_metadata_do_not_reimport(self):
        d = self.definition(
            "calibre",
            "import",
            sql="SELECT item_id FROM catalog_items WHERE item_id='edition'",
            output=self.root / "books",
        )
        with (
            patch(
                "catabolic.importers.shutil.which", return_value="/fixture/calibredb"
            ),
            patch("catabolic.importers.command_output") as command,
        ):
            self.assertTrue(self.apply(d)["complete"])
            with Store(self.database, writable=True) as store:
                app = Application(store)
                app.scan()
                app.put_item(
                    "book_edition",
                    {},
                    {
                        "title": "Title & Café",
                        "author": "Author",
                        "summary": "local change",
                    },
                    "edition",
                )
            self.assertTrue(self.apply(d)["results"][0]["unchanged"])
            self.assertEqual(command.call_count, 1)

    def test_import_replaced_destination_is_not_adopted(self):
        output = self.root / "books"
        output.mkdir()
        d = self.definition(
            "calibre",
            "import",
            sql="SELECT item_id FROM catalog_items WHERE item_id='edition'",
            output=output,
        )
        with (
            patch(
                "catabolic.importers.shutil.which", return_value="/fixture/calibredb"
            ),
            patch("catabolic.importers.command_output"),
        ):
            self.assertTrue(self.apply(d)["complete"])
            output.rename(self.root / "old-books")
            output.mkdir()
            with self.assertRaisesRegex(ConsumerError, "destination_identity_changed"):
                run(self.database, "default", d)

    def test_cli_publication_and_import_recovery_arguments(self):
        import contextlib
        import io

        from catabolic.cli import main, parser

        output = self.root / "bundle"
        d = self.definition(
            "xspf",
            "export",
            sql="SELECT item_id FROM catalog_items WHERE item_id='track1'",
            output=output,
        )
        file = self.root / "mapping.json"
        file.write_text(json.dumps(d))

        def cli(*args):
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                code = main(
                    ["--db", str(self.database), "--machine", "projection", *args]
                )
            self.assertEqual(code, 0, out.getvalue())
            return json.loads(out.getvalue())["data"]

        p = cli("mapping-preview", "--definition", str(file))
        self.assertTrue(
            cli(
                "mapping-apply",
                "--definition",
                str(file),
                "--expected-plan",
                p["plan_id"],
            )["complete"]
        )
        event = cli("mapping-events")["events"][-1]
        self.assertEqual(cli("mapping-events", "--event", event["id"])["version"], 2)
        args = parser().parse_args(
            [
                "projection",
                "mapping-recover",
                "event",
                "--resolution",
                "submitted",
                "--expected-plan",
                "hash",
                "--apply",
            ]
        )
        self.assertEqual(args.resolution, "submitted")
