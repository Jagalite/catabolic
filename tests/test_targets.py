import copy
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from xml.etree import ElementTree as ET

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.exports import build_export, publish_bundle
from catabolic.importers import import_catalog
from catabolic.layouts import PRESETS, Layouts, validate_layout
from catabolic.manifest import Manifest
from catabolic.reconcile import Reconciler
from catabolic.store import Store
from catabolic.targets import definitions, describe
from tests.test_media_model import populate_media


class TargetTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.path = self.root / "catalog.sqlite3"
        Store.initialize(self.path)
        self.store = Store(self.path, writable=True)
        self.addCleanup(self.store.close)
        self.app = Application(self.store)
        self.files, _ = populate_media(self.app, self.root)
        for identifier, kind in (
            ("movie", "movie"),
            ("edition", "book_edition"),
            ("audio", "audiobook"),
            ("photo", "photo"),
        ):
            self.app.put_item(
                kind,
                {},
                {
                    "title": "Title & <Café>",
                    "year": 2026,
                    "author": "Author",
                    "album": "My Photos!",
                },
                identifier,
            )
        self.app.media.associate(
            self.files["movie.srt"],
            "movie",
            role="subtitle",
            metadata={"language": "en"},
        )
        self.before = self.source_state()

    def source_state(self):
        return {
            str(path): (
                path.stat().st_ino,
                hashlib.sha256(path.read_bytes()).hexdigest(),
            )
            for path in (self.root / "media").rglob("*")
            if path.is_file()
        }

    def document(self, catalog="global"):
        with self.store.transaction():
            return Manifest(self.app).build(catalog)

    def test_every_target_layout_sync_and_profile_validation(self):
        self.assertEqual(len(describe()["targets"]), 20)
        self.assertEqual(len(definitions()), 17)
        layouts = Layouts(self.app)
        for target, definition in definitions().items():
            with self.subTest(target=target):
                directory = self.root / target
                directory.mkdir()
                self.app.bind("output", target, str(directory))
                layouts.put(target, definition)
                result = layouts.run(target, target, apply=True)
                self.assertTrue(result["applied"], result)
                self.assertGreater(result["desired_count"], 0)
                self.assertTrue(Reconciler(self.app).apply(target)["healthy"])
                self.assertTrue(Reconciler(self.app).verify(target)["healthy"])
                self.assertEqual(layouts.run(target, target, apply=True)["changes"], [])
                self.assertEqual(Reconciler(self.app).apply(target)["applied"], [])
                document = self.document(target)
                self.assertEqual(
                    document["content"]["layout"]["current_definition"]["profile"],
                    definition["profile"],
                )
                if target == "piwigo":
                    for entry in document["content"]["entries"]:
                        self.assertRegex(entry["path"], r"^[A-Za-z0-9/_.-]+$")
                changed = copy.deepcopy(definition)
                changed["rules"][0]["path"] = "Other/{file.name}"
                with self.assertRaisesRegex(CatabolicError, "exact naming rules"):
                    validate_layout(changed)
        self.assertEqual(self.source_state(), self.before)

    def test_formats_escape_metadata_and_bundle_never_overwrites(self):
        document = self.document()
        nfo = build_export(document, "nfo")
        movie = ET.fromstring(nfo["files"][0]["content"])
        self.assertEqual(movie.tag, "movie")
        self.assertEqual(movie.findtext("title"), "Title & <Café>")
        xspf = build_export(document, "xspf")
        root = ET.fromstring(xspf["files"][0]["content"])
        self.assertTrue(root.findall(".//{http://xspf.org/ns/0/}location"))
        opds = build_export(document, "opds", "https://books.example/library/")
        feed = json.loads(opds["files"][0]["content"])
        edition = next(
            value
            for value in feed["publications"]
            if value["metadata"]["title"] == "Title & <Café>"
        )
        self.assertEqual(len(edition["links"]), 2)
        self.assertEqual(
            {value["type"] for value in edition["links"]},
            {"application/epub+zip", "application/pdf"},
        )
        destination = self.root / "bundle"
        result = publish_bundle(self.app, document, nfo, destination)
        self.assertEqual(result["links"], 14)
        for entry in document["content"]["entries"]:
            self.assertTrue((destination / entry["path"]).is_symlink())
            self.assertTrue((destination / entry["path"]).exists())
        self.assertTrue((destination / nfo["files"][0]["path"]).is_file())
        with self.assertRaises(FileExistsError):
            publish_bundle(self.app, document, nfo, destination)
        with self.assertRaises(CatabolicError):
            publish_bundle(self.app, document, nfo, self.root / "media/new")
        self.assertFalse((self.root / "media/new").exists())
        for value in (
            None,
            "file:///tmp",
            "https://user:secret@books.example",
            "https://books.example/?token=secret",
        ):
            with self.assertRaises(CatabolicError):
                build_export(document, "opds", value)
        invalid = copy.deepcopy(document)
        next(item for item in invalid["content"]["items"] if item["id"] == "edition")[
            "metadata"
        ]["title"] = {"nested": "not text"}
        with self.assertRaisesRegex(CatabolicError, "title must be text"):
            build_export(invalid, "opds", "https://books.example/")
        self.assertEqual(self.source_state(), self.before)

    def test_tv_numbering_missing_metadata_and_extension_filtering(self):
        source = self.root / "media"
        (source / "episode.MKV").write_bytes(b"episode")
        (source / "episode.srt").write_bytes(b"subtitle")
        self.app.scan()
        files = {row["path"]: row["id"] for row in self.app.files()["files"]}
        self.app.put_item("series", {}, {"title": "Example Show"}, "show")
        self.app.put_item("season", {}, {}, "season")
        self.app.put_item("episode", {}, {"title": "Pilot"}, "episode")
        self.app.media.relate("season", "show", "part_of", position=1)
        self.app.media.relate("episode", "season", "part_of", position=2)
        self.app.media.associate(files["episode.MKV"], "episode")
        self.app.media.associate(
            files["episode.srt"],
            "episode",
            role="subtitle",
            metadata={"language": "fr"},
        )
        layouts = Layouts(self.app)
        for target in ("plex", "jellyfin", "emby", "kodi", "infuse"):
            directory = self.root / target
            directory.mkdir()
            self.app.bind("output", target, str(directory))
            layouts.put(target, definitions()[target])
            plan = layouts.run(target, target, apply=True)
            self.assertTrue(plan["applied"], plan)
            document = self.document(target)
            paths = {entry["path"] for entry in document["content"]["entries"]}
            self.assertIn("TV/Example Show/Season 01/Example Show - S01E02.MKV", paths)
            self.assertIn(
                "TV/Example Show/Season 01/Example Show - S01E02.fr.srt", paths
            )
            episode = next(
                ET.fromstring(value["content"])
                for value in build_export(document, "nfo")["files"]
                if "S01E02" in value["path"]
            )
            self.assertEqual(episode.findtext("season"), "1")
            self.assertEqual(episode.findtext("episode"), "2")
        # An unsupported suffix is skipped, and missing required metadata blocks
        # the whole apply rather than silently removing old generated mappings.
        self.app.media.associate(self.files["notes.txt"], "movie")
        self.app.put_item("movie", {}, {"title": "No year"}, "movie")
        before = self.store.rows("SELECT * FROM mappings WHERE catalog='jellyfin'")
        plan = layouts.run("jellyfin", "jellyfin", apply=True)
        self.assertFalse(plan["applied"])
        self.assertGreater(plan["skipped_associations"], 0)
        self.assertEqual(
            before, self.store.rows("SELECT * FROM mappings WHERE catalog='jellyfin'")
        )

    def test_real_import_subprocess_only_receives_private_copies(self):
        # An executable stand-in exercises argv/env/staging across a real process
        # boundary. It deliberately deletes its input copies.
        tools = self.root / "tools"
        tools.mkdir()
        log = self.root / "import.log"
        script = (
            "#!" + sys.executable + "\n"
            "import json, os, pathlib, sys\n"
            "p = pathlib.Path(sys.argv[-1])\n"
            "files = list(p.iterdir()) if p.is_dir() else [p]\n"
            "assert all('catabolic-import-' in str(f) for f in files)\n"
            "assert 'IMMICH_DELETE_ASSETS' not in os.environ\n"
            "with open(os.environ['CATABOLIC_IMPORT_TEST_LOG'], 'a') as log:\n"
            "    log.write(json.dumps({'argv': sys.argv[1:], 'files': [f.name for f in files]}) + '\\n')\n"
            "for f in files: f.unlink()\n"
        )
        for name in ("calibredb", "immich"):
            (tools / name).write_text(script)
            (tools / name).chmod(0o755)
        self.store.close()
        env = {
            **os.environ,
            "PATH": str(tools) + os.pathsep + os.environ.get("PATH", ""),
            "CATABOLIC_IMPORT_TEST_LOG": str(log),
            "IMMICH_API_KEY": "test-secret",
            "IMMICH_DELETE_ASSETS": "true",
        }
        for target in ("calibre", "calibre-web", "immich"):
            destination = (
                "https://photos.example/api"
                if target == "immich"
                else str(self.root / "calibre")
            )
            result = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "catabolic",
                    "--db",
                    str(self.path),
                    "target",
                    "import",
                    target,
                    "--destination",
                    destination,
                    "--apply",
                ],
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertTrue(json.loads(result.stdout)["complete"])
            self.assertNotIn("test-secret", result.stdout + result.stderr)
        records = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual(len(records), 4)
        self.assertEqual(self.source_state(), self.before)

    def test_bundle_alias_collision_and_symlink_parent_refused(self):
        document = self.document()
        bad = {
            "format": "nfo",
            "files": [{"path": "movies/movie/movie.MKV", "content": "oops"}],
        }
        with self.assertRaisesRegex(CatabolicError, "collision"):
            publish_bundle(self.app, document, bad, self.root / "conflict")
        self.assertFalse((self.root / "conflict").exists())
        (self.root / "alias").symlink_to(self.root / "media", target_is_directory=True)
        with self.assertRaises((OSError, CatabolicError)):
            publish_bundle(
                self.app,
                document,
                build_export(document, "xspf"),
                self.root / "alias/new",
            )
        self.assertFalse((self.root / "media/new").exists())

    def test_import_preflight_grouped_formats_and_source_isolation(self):
        document = self.document()
        destination = str(self.root / "calibre-library")
        with patch("catabolic.importers.subprocess.run") as run:
            plan = import_catalog(self.app, document, "calibre", destination)
            self.assertEqual(len(plan["actions"]), 2)
            run.assert_not_called()
        calls = []

        def importer(argv, **kwargs):
            calls.append(argv)
            stage = Path(argv[-1])
            if argv[1] == "add":
                self.assertIn("--one-book-per-directory", argv)
                self.assertTrue(
                    any(value.startswith("--identifier=catabolic:") for value in argv)
                )
                self.assertEqual(
                    {path.suffix for path in stage.iterdir()}, {".epub", ".pdf"}
                )
                for file in stage.iterdir():
                    file.unlink()  # An external tool cannot delete original media.
            else:
                self.assertEqual(argv[1:3], ["upload", "--no-progress"])
                self.assertNotIn("IMMICH_DELETE_ASSETS", kwargs["env"])
                self.assertNotIn("IMMICH_DELETE_DUPLICATES", kwargs["env"])
                stage.unlink()
            return subprocess.CompletedProcess(argv, 0)

        with (
            patch("catabolic.importers.shutil.which", return_value="/fake/tool"),
            patch("catabolic.importers.subprocess.run", side_effect=importer),
            patch.dict(
                os.environ,
                {
                    "IMMICH_API_KEY": "secret",
                    "IMMICH_DELETE_ASSETS": "true",
                    "IMMICH_DELETE_DUPLICATES": "true",
                },
            ),
        ):
            for target in ("calibre", "calibre-web", "immich"):
                result = import_catalog(
                    self.app,
                    document,
                    target,
                    "https://photos.example/api" if target == "immich" else destination,
                    apply=True,
                )
                self.assertTrue(result["complete"], result)
                self.assertTrue(result["applied"])
                self.assertEqual(len(result["completed"]), 2)
                self.assertNotIn("secret", json.dumps(result))
        self.assertEqual(len(calls), 4)
        self.assertEqual(self.source_state(), self.before)

    def test_import_missing_dependency_failure_and_changed_source(self):
        document = self.document()
        destination = str(self.root / "calibre-library")
        with (
            patch("catabolic.importers.shutil.which", return_value=None),
            self.assertRaisesRegex(CatabolicError, "install the official"),
        ):
            import_catalog(self.app, document, "calibre", destination, apply=True)
        with (
            patch("catabolic.importers.shutil.which", return_value="/fake/calibredb"),
            patch(
                "catabolic.importers.subprocess.run",
                return_value=subprocess.CompletedProcess([], 7),
            ),
        ):
            result = import_catalog(
                self.app, document, "calibre", destination, apply=True
            )
            self.assertFalse(result["complete"])
            self.assertEqual(result["completed"], [])
        (self.root / "media/book.epub").write_bytes(b"changed")
        with self.assertRaisesRegex(CatabolicError, "changed"):
            import_catalog(self.app, document, "calibre", destination)

    def test_cli_discovery_exports_and_import_preview(self):
        self.store.close()
        prefix = [sys.executable, "-m", "catabolic", "--db", str(self.path)]
        for args in (
            ["target", "list"],
            ["target", "show", "immich"],
            ["export", "--format", "opds", "--base-url", "https://books.example/"],
            [
                "target",
                "import",
                "calibre-web",
                "--destination",
                str(self.root / "calibre"),
            ],
            ["export", "--format", "nfo", "--output", str(self.root / "cli-bundle")],
        ):
            process = subprocess.run([*prefix, *args], capture_output=True, text=True)
            self.assertEqual(
                process.returncode, 0, (args, process.stdout, process.stderr)
            )
            self.assertIsInstance(json.loads(process.stdout), dict)
        process = subprocess.run(
            [*prefix, "export", "--format", "xspf", "--raw"],
            capture_output=True,
            text=True,
        )
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertEqual(
            ET.fromstring(process.stdout).tag, "{http://xspf.org/ns/0/}playlist"
        )


class TargetDefinitionTest(unittest.TestCase):
    def test_profiles_are_independent_and_reject_unsupported_versions(self):
        for definition in definitions().values():
            validate_layout(definition)
            definition["profile"]["version"] = 2
            with self.assertRaisesRegex(CatabolicError, "unsupported layout profile"):
                validate_layout(definition)
        self.assertNotIn("immich", PRESETS)
        self.assertIn("plex-v1", PRESETS)
