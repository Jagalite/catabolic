# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Official import planning and real subprocesses on disposable source copies."""

import copy
import json
import os
import subprocess
import sys
import time
import unittest
from pathlib import Path
from unittest.mock import patch

from catabolic.app import Application
from catabolic.domain import CatabolicError
from catabolic.importers import import_catalog
from catabolic.store import Store
from tests import test_targets as reference

BINARY = Path(
    os.environ.get("CATABOLIC_NATIVE_BINARY", "target/debug/catabolic-native")
).resolve()


@unittest.skipUnless(BINARY.is_file(), "build native executable explicitly")
class NativeImportTest(unittest.TestCase):
    setUp = reference.TargetTest.setUp
    source_state = reference.TargetTest.source_state
    document = reference.TargetTest.document

    def command(self, document, target="calibre", destination=None, **request):
        return [
            str(BINARY),
            "--db",
            str(self.path),
            "operation",
            "import",
            "catalog",
            "--options",
            json.dumps(
                dict(
                    document=document,
                    target=target,
                    destination=destination or str(self.root / "import-library"),
                    **request,
                )
            ),
        ]

    def native(self, document, *, env=None, **request):
        process = subprocess.run(
            self.command(document, **request),
            env=env,
            capture_output=True,
            text=True,
            timeout=20,
        )
        if process.returncode:
            raise CatabolicError(process.stderr.strip().removeprefix("catabolic: "))
        return json.loads(process.stdout)

    def oracle(self, document, target="calibre", destination=None, **request):
        with Store(self.path, writable=True) as store:
            return import_catalog(
                Application(store),
                document,
                target,
                destination or str(self.root / "import-library"),
                **request,
            )

    def tools(self, body):
        directory = self.root / "tools"
        directory.mkdir(exist_ok=True)
        for name in ("calibredb", "immich"):
            path = directory / name
            path.write_text("#!" + sys.executable + "\n" + body)
            path.chmod(0o755)
        return os.environ | {
            "PATH": str(directory) + os.pathsep + os.environ.get("PATH", ""),
            "IMMICH_API_KEY": "disposable-fixture-value",
            "IMMICH_DELETE_ASSETS": "true",
            "CATABOLIC_IMPORT_TEST_LOG": str(self.root / "import.log"),
        }

    @staticmethod
    def normalize(result):
        result = copy.deepcopy(result)
        for attempt in result["attempts"]:
            attempt["id"] = "attempt"
        return result

    def test_previews_grouping_limits_and_manifest_revision_match(self):
        document = self.document()
        self.store.close()
        for target in ("calibre", "calibre-web", "immich"):
            destination = "https://photos.example/api" if target == "immich" else None
            args = dict(target=target, destination=destination)
            self.assertEqual(
                self.native(document, **args), self.oracle(document, **args)
            )
        for request in (
            {"limit": 1},
            {"limit": True},
            {"timeout": 0},
            {"timeout": 3601},
            {"target": "plex"},
        ):
            with self.subTest(request=request):
                with self.assertRaises(CatabolicError) as expected:
                    self.oracle(document, **request)
                with self.assertRaises(CatabolicError) as actual:
                    self.native(document, **request)
                self.assertEqual(str(actual.exception), str(expected.exception))
        changed = copy.deepcopy(document)
        for file in changed["content"]["files"]:
            file["size"] = str(int(file["size"]) + 1)
        with self.assertRaises(CatabolicError) as expected:
            self.oracle(changed)
        with self.assertRaises(CatabolicError) as actual:
            self.native(changed)
        self.assertEqual(str(actual.exception), str(expected.exception))
        self.assertEqual(self.source_state(), self.before)

    def test_real_official_cli_shapes_receive_only_private_copies(self):
        document = self.document()
        self.store.close()
        environment = self.tools(
            "import json,os,pathlib,sys\n"
            "p=pathlib.Path(sys.argv[-1]); files=list(p.iterdir()) if p.is_dir() else [p]\n"
            "assert all('catabolic-import-' in str(f) for f in files)\n"
            "assert 'IMMICH_DELETE_ASSETS' not in os.environ\n"
            "record={'argv':sys.argv[1:-1], 'files':sorted(f.name for f in files)}\n"
            "with open(os.environ['CATABOLIC_IMPORT_TEST_LOG'],'a') as f: f.write(json.dumps(record)+'\\n')\n"
            "for f in files: f.unlink()\n"
        )
        for target in ("calibre", "calibre-web", "immich"):
            destination = "https://photos.example/api" if target == "immich" else None
            args = dict(target=target, destination=destination, apply=True)
            with patch.dict(os.environ, environment, clear=True):
                expected = self.oracle(document, **args)
            actual = self.native(document, env=environment, **args)
            self.assertEqual(self.normalize(actual), self.normalize(expected))
            self.assertTrue(actual["complete"])
            self.assertNotIn("disposable-fixture-value", json.dumps(actual))
        records = [
            json.loads(line)
            for line in (self.root / "import.log").read_text().splitlines()
        ]
        self.assertEqual(len(records), 8)
        self.assertEqual(records[:1], records[1:2])
        self.assertEqual(records[2:3], records[3:4])
        self.assertEqual(records[4:6], records[6:8])
        self.assertEqual(self.source_state(), self.before)

    def test_failed_external_outcome_is_not_safe_to_retry(self):
        document = self.document()
        self.store.close()
        environment = self.tools("import sys\nsys.exit(7)\n")
        with patch.dict(os.environ, environment, clear=True):
            expected = self.oracle(document, apply=True)
        actual = self.native(document, env=environment, apply=True)
        self.assertEqual(self.normalize(actual), self.normalize(expected))
        self.assertIsNone(actual["applied"])
        self.assertEqual(actual["outcome"], "external_outcome_unknown")
        self.assertFalse(actual["safe_to_retry"])
        self.assertEqual(self.source_state(), self.before)

    def test_external_wait_releases_catalog_writer(self):
        document = self.document()
        self.store.close()
        environment = self.tools(
            "import os,pathlib,time\n"
            "pathlib.Path(os.environ['CATABOLIC_IMPORT_TEST_LOG']).write_text('started')\n"
            "time.sleep(1)\n"
        )
        child = subprocess.Popen(
            self.command(document, apply=True),
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(lambda: child.poll() is None and child.kill())
        deadline = time.monotonic() + 10
        while not (self.root / "import.log").exists() and time.monotonic() < deadline:
            self.assertIsNone(child.poll())
            time.sleep(0.01)
        self.assertIsNone(child.poll())
        with Store(self.path, writable=True) as store, store.transaction() as db:
            db.execute("INSERT INTO meta VALUES('import-writer-progress','yes')")
        output, error = child.communicate(timeout=10)
        self.assertEqual(child.returncode, 0, error)
        self.assertTrue(json.loads(output)["complete"])
        self.assertEqual(self.source_state(), self.before)

    def test_changed_source_and_missing_tool_refuse(self):
        document = self.document()
        self.store.close()
        for environment in (
            {"PATH": ""},
            self.tools("pass\n") | {"IMMICH_API_KEY": ""},
        ):
            request = (
                {"target": "immich", "destination": "https://photos.example/"}
                if environment.get("IMMICH_API_KEY") == ""
                else {}
            )
            with patch.dict(os.environ, environment, clear=True):
                with self.assertRaises(CatabolicError) as expected:
                    self.oracle(document, apply=True, **request)
            with self.assertRaises(CatabolicError) as actual:
                self.native(document, env=environment, apply=True, **request)
            self.assertEqual(str(actual.exception), str(expected.exception))
        (self.root / "media/book.epub").write_bytes(b"changed")
        with self.assertRaises(CatabolicError) as expected:
            self.oracle(document)
        with self.assertRaises(CatabolicError) as actual:
            self.native(document)
        self.assertEqual(str(actual.exception), str(expected.exception))

    def test_falsey_metadata_matches_official_cli_arguments(self):
        document = self.document()
        self.store.close()
        environment = self.tools(
            "import json,os,sys\n"
            "with open(os.environ['CATABOLIC_IMPORT_TEST_LOG'],'a') as f: f.write(json.dumps(sys.argv[1:-1])+'\\n')\n"
        )
        log = self.root / "import.log"
        for value in ([], {}, 0.0, False, ""):
            with self.subTest(value=value):
                changed = copy.deepcopy(document)
                for item in changed["content"]["items"]:
                    item["metadata"]["title"] = value
                    item["metadata"]["author"] = value
                log.unlink(missing_ok=True)
                with patch.dict(os.environ, environment, clear=True):
                    self.oracle(changed, apply=True)
                self.native(changed, env=environment, apply=True)
                calls = [json.loads(line) for line in log.read_text().splitlines()]
                self.assertEqual(len(calls), 2)
                self.assertEqual(calls[0], calls[1])
        self.assertEqual(self.source_state(), self.before)

    def test_sigint_reports_unknown_external_outcome(self):
        import signal

        document = self.document()
        self.store.close()
        staging = self.root / "staging"
        staging.mkdir()
        environment = self.tools(
            "import os,pathlib,time\n"
            "pathlib.Path(os.environ['CATABOLIC_IMPORT_TEST_LOG']).write_text(str(os.getpid()))\n"
            "time.sleep(2)\n"
        ) | {"TMPDIR": str(staging)}
        child = subprocess.Popen(
            self.command(document, apply=True),
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(lambda: child.poll() is None and child.kill())
        deadline = time.monotonic() + 10
        while not (self.root / "import.log").exists() and time.monotonic() < deadline:
            self.assertIsNone(child.poll())
            time.sleep(0.01)
        self.assertTrue((self.root / "import.log").exists())
        self.assertIsNone(child.poll())
        child.send_signal(signal.SIGINT)
        output, error = child.communicate(timeout=10)
        self.assertEqual(child.returncode, 0, error)
        result = json.loads(output)
        self.assertFalse(result["complete"])
        self.assertIsNone(result["applied"])
        self.assertEqual(result["outcome"], "external_outcome_unknown")
        self.assertFalse(result["safe_to_retry"])
        self.assertTrue(result["interrupted"])
        self.assertEqual(
            result["error"],
            "import interrupted; inspect the destination before retrying",
        )
        self.assertEqual(result["completed"], [])
        self.assertEqual(len(result["attempts"]), 1)
        self.assertEqual(result["failed_files"], result["attempts"][0]["file_ids"])
        self.assertEqual(list(staging.iterdir()), [])
        with self.assertRaises(ProcessLookupError):
            os.kill(int((self.root / "import.log").read_text()), 0)
        self.assertEqual(self.source_state(), self.before)

    def test_second_external_failure_preserves_completed_files(self):
        document = self.document()
        self.store.close()
        environment = self.tools(
            "import os,pathlib,sys\n"
            "p=pathlib.Path(os.environ['CATABOLIC_IMPORT_TEST_LOG'])\n"
            "n=int(p.read_text()) if p.exists() else 0\n"
            "p.write_text(str(n+1))\n"
            "sys.exit(0 if n==0 else 7)\n"
        )
        request = dict(
            target="immich", destination="https://photos.example/api", apply=True
        )
        with patch.dict(os.environ, environment, clear=True):
            expected = self.oracle(document, **request)
        (self.root / "import.log").unlink()
        actual = self.native(document, env=environment, **request)
        self.assertEqual(self.normalize(actual), self.normalize(expected))
        self.assertTrue(actual["applied"])
        self.assertFalse(actual["complete"])
        self.assertFalse(actual["safe_to_retry"])
        self.assertEqual(len(actual["completed"]), 1)
        self.assertEqual(len(actual["attempts"]), 2)
        self.assertEqual(self.source_state(), self.before)

    def test_sigint_during_copy_does_not_launch_tool(self):
        import signal

        large = self.root / "media/book.pdf"
        with large.open("wb") as source:
            source.truncate(512 * 1024 * 1024)
        self.app.scan()
        document = self.document()
        self.store.close()
        before = large.stat()
        staging = self.root / "staging"
        staging.mkdir()
        environment = self.tools(
            "import os,pathlib\n"
            "pathlib.Path(os.environ['CATABOLIC_IMPORT_TEST_LOG']).write_text('unexpected launch')\n"
        ) | {"TMPDIR": str(staging)}
        child = subprocess.Popen(
            self.command(document, apply=True),
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(lambda: child.poll() is None and child.kill())
        deadline = time.monotonic() + 10
        copying = False
        while time.monotonic() < deadline:
            self.assertIsNone(child.poll())
            copying = any(
                path.stat().st_size > 0
                for path in staging.glob("catabolic-import-*/asset.pdf")
            )
            if copying:
                break
            time.sleep(0.001)
        self.assertTrue(copying, "private staging did not start")
        child.send_signal(signal.SIGINT)
        output, error = child.communicate(timeout=10)
        self.assertEqual(child.returncode, 0, error)
        result = json.loads(output)
        self.assertFalse(result["complete"])
        self.assertFalse(result["applied"])
        self.assertEqual(result["outcome"], "failed_before_launch")
        self.assertTrue(result["safe_to_retry"])
        self.assertTrue(result["interrupted"])
        self.assertFalse((self.root / "import.log").exists())
        self.assertEqual(list(staging.iterdir()), [])
        after = large.stat()
        self.assertEqual((after.st_ino, after.st_size), (before.st_ino, before.st_size))
