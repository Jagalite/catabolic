# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Frozen Python rollback check for the explicit native schema-38 upgrade.

The native CLI upgrades a disposable schema-32 catalog with a verified backup.
The backup snapshot is restored to a new path after its manifest SHA-256 is
checked, and frozen Python must read it (``db status`` and ``status``). Frozen
Python must refuse the upgraded schema-38 file without changing it. This is a
rollback-to-backup check, not a downgrade: writes made after the upgrade are
not carried back.

CATABOLIC_PYTHON_ORACLE selects the frozen interpreter, CATABOLIC_PYTHON_REFERENCE
the frozen ``src/`` directory and CATABOLIC_NATIVE_CLI the ``catabolic-native``
executable. The tests skip with the missing names when any is unset. The
retained schema-32 fixture is also used when CATABOLIC_REHEARSAL_FIXTURES or the
checkout's ``.local-tests`` copy provides it.
"""

import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
# The latest native target. Frozen Python supports schemas 1 through 32.
NATIVE_SCHEMA = 38
REQUIRED = (
    "CATABOLIC_PYTHON_ORACLE",
    "CATABOLIC_PYTHON_REFERENCE",
    "CATABOLIC_NATIVE_CLI",
)
ORACLE = (
    "import sys;sys.path.insert(0,sys.argv.pop(1));"
    "from catabolic.cli import main;raise SystemExit(main(sys.argv[1:]))"
)
QUIESCENT = (
    "--assert-quiescent",
    "admission-stopped",
    "--assert-quiescent",
    "readers-and-workers-stopped",
    "--assert-quiescent",
    "exclusive-operational-ownership",
)
FIXTURES = Path(
    os.environ.get(
        "CATABOLIC_REHEARSAL_FIXTURES",
        ROOT / ".local-tests/rust-migration/m0-completion",
    )
)
SCHEMA_32_FIXTURE = FIXTURES / "legacy-fixtures/schema-32/before.sqlite3"


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


class NativeUpgradeRollbackTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        missing = [name for name in REQUIRED if not os.environ.get(name)]
        if missing:
            raise unittest.SkipTest("frozen rollback check needs " + ", ".join(missing))
        cls.python = Path(os.environ["CATABOLIC_PYTHON_ORACLE"]).absolute()
        cls.reference = Path(os.environ["CATABOLIC_PYTHON_REFERENCE"]).resolve()
        cls.native = Path(os.environ["CATABOLIC_NATIVE_CLI"]).resolve()
        # Configured but unusable prerequisites are failures, not skips.
        for path in (cls.python, cls.native):
            if not os.access(path, os.X_OK):
                raise AssertionError(f"not executable: {path}")
        if not (cls.reference / "catabolic/migration.py").is_file():
            raise AssertionError(f"not a frozen reference src/: {cls.reference}")

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def frozen(self, database, *args):
        environment = dict(os.environ, PYTHONDONTWRITEBYTECODE="1")
        return subprocess.run(
            [str(self.python), "-B", "-c", ORACLE, str(self.reference)]
            + ["--db", str(database), "--json", *args],
            capture_output=True,
            text=True,
            env=environment,
            timeout=300,
        )

    def frozen_json(self, database, *args):
        result = self.frozen(database, *args)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def native_json(self, database, *args):
        result = subprocess.run(
            [str(self.native), "--db", str(database), *args],
            capture_output=True,
            text=True,
            timeout=900,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def catalogs(self):
        """Yield disposable schema-32 catalogs: frozen init and the fixture."""
        created = self.root / "initialized" / "catalog.sqlite3"
        created.parent.mkdir()
        self.frozen_json(created, "init")
        yield "frozen-init", created
        if SCHEMA_32_FIXTURE.is_file():
            copied = self.root / "fixture" / "catalog.sqlite3"
            copied.parent.mkdir()
            copied.write_bytes(SCHEMA_32_FIXTURE.read_bytes())
            copied.chmod(0o600)
            yield "retained-schema-32", copied

    def upgrade(self, database):
        plan = database.parent / "plan.json"
        planned = self.native_json(database, "db", "native-plan", "--out", str(plan))
        self.assertEqual(planned["from_schema"], 32)
        self.assertEqual(planned["to_schema"], NATIVE_SCHEMA)
        report = self.native_json(
            database,
            "db",
            "native-upgrade",
            "--plan",
            str(plan),
            "--backup-dir",
            str(database.parent / "backups"),
            *QUIESCENT,
        )
        self.assertTrue(report["upgraded"])
        self.assertEqual(report["schema"], NATIVE_SCHEMA)
        return report

    def test_restored_backup_is_read_by_frozen_python(self):
        for name, database in self.catalogs():
            with self.subTest(name):
                status = self.frozen_json(database, "db", "status")
                self.assertEqual(status["schema"], 32)
                counts = self.frozen_json(database, "status")["counts"]
                report = self.upgrade(database)
                backup = Path(report["backup"])
                receipt = json.loads((backup.parent / "manifest.json").read_text())
                self.assertEqual(receipt["status"], "committed")
                self.assertEqual(receipt["sha256"], sha256(backup))
                restored = database.parent / "restored.sqlite3"
                self.assertFalse(restored.exists())
                shutil.copyfile(backup, restored)
                self.assertEqual(sha256(restored), receipt["sha256"])

                restored_status = self.frozen_json(restored, "db", "status")
                self.assertEqual(restored_status["schema"], 32)
                self.assertEqual(restored_status["latest_schema"], 32)
                self.assertEqual(restored_status["database_id"], status["database_id"])
                self.assertEqual(restored_status["history"], status["history"])
                self.assertEqual(restored_status["pending_migrations"], [])
                self.assertEqual(self.frozen_json(restored, "status")["counts"], counts)
                # Frozen reads leave the restored bytes exactly as verified.
                self.assertEqual(sha256(restored), receipt["sha256"])

    def test_frozen_python_refuses_the_upgraded_catalog(self):
        for name, database in self.catalogs():
            with self.subTest(name):
                self.upgrade(database)
                before = sha256(database)
                for command in (("db", "status"), ("status",)):
                    result = self.frozen(database, *command)
                    self.assertNotEqual(result.returncode, 0, command)
                    self.assertIn(
                        f"unsupported database schema {NATIVE_SCHEMA}",
                        result.stdout + result.stderr,
                        command,
                    )
                self.assertEqual(sha256(database), before)
                inspected = self.native_json(database, "db", "native-inspect")
                self.assertEqual(inspected["schema"], NATIVE_SCHEMA)


if __name__ == "__main__":
    unittest.main()
