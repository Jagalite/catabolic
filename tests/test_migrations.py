import errno
import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path
from unittest.mock import patch

from catabolic.database_io import connect_database, writer_lock
from catabolic.domain import CatabolicError
from catabolic.migration import (
    SCHEMA_VERSION,
    Migration,
    data_snapshot,
    inspect_database,
    load_migrations,
    upgrade_database,
    validate_preservation,
    validate_sequence,
)
from catabolic.store import Store

DATABASE_ID = "00000000-0000-4000-8000-000000000001"
OLD_SCHEMA = Path(__file__).parent / "fixtures" / "schema_v1.sql"


def create_legacy(root: Path) -> Path:
    """Create a populated version-1 database independently of current migrations."""
    source = root / "media"
    output = root / "plex"
    source.mkdir()
    output.mkdir()
    media = source / "movie.mkv"
    media.write_bytes(b"synthetic legacy media")
    (output / "Movies").mkdir()
    target = os.path.relpath(media, output / "Movies")
    (output / "Movies/Test.mkv").symlink_to(target)
    (output / ".catabolic-owner.json").write_text(
        json.dumps(
            {
                "format": 1,
                "database_id": DATABASE_ID,
                "profile": "default",
                "catalog": "global",
            }
        )
    )
    path = root / "catalog.sqlite3"
    db = sqlite3.connect(path)
    db.executescript(OLD_SCHEMA.read_text())
    db.execute("PRAGMA foreign_keys=ON")
    db.execute("INSERT INTO meta VALUES ('database_id',?)", (DATABASE_ID,))
    db.execute(
        "INSERT INTO meta VALUES ('user-extension',?)",
        ('{"opaque":[null,true,"keep me"]}',),
    )
    db.executemany("INSERT INTO profiles VALUES (?)", [("default",), ("laptop",)])
    db.execute("INSERT INTO locations VALUES ('media')")
    db.executemany("INSERT INTO catalogs VALUES (?)", [("global",), ("favorites",)])
    for kind, owner, binding in (
        ("source", "media", source),
        ("output", "global", output),
    ):
        st = binding.stat()
        db.execute(
            "INSERT INTO bindings VALUES (?,?,?,?,?,?)",
            ("default", kind, owner, str(binding), st.st_dev, st.st_ino),
        )
    db.execute(
        "INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan-1','default','media',1,1,'[]')"
    )
    db.executemany(
        "INSERT INTO files VALUES (?,?,?)",
        [("file-a", "media", "movie.mkv"), ("file-b", "media", "missing.mkv")],
    )
    st = media.stat()
    db.execute(
        "INSERT INTO observations VALUES (?,?,?,?,?,?,?,?)",
        (
            "default",
            "file-a",
            st.st_size,
            st.st_mtime_ns,
            st.st_dev,
            st.st_ino,
            "present",
            "scan-1",
        ),
    )
    db.execute(
        "INSERT INTO observations VALUES ('default','file-b',10,1,1,1,'missing','scan-1')"
    )
    db.execute(
        "INSERT INTO items VALUES ('item-a','movie',?)",
        ('{"title":"Café","unknown":{"values":[1,null,true]}}',),
    )
    db.execute("INSERT INTO identities VALUES ('fixture.movie','1','item-a')")
    db.executemany(
        "INSERT INTO mappings VALUES (?,?,?,?,?,?)",
        [
            ("mapping-a", "global", "file-a", "item-a", "Movies/Test.mkv", 1),
            ("mapping-disabled", "global", "file-b", "item-a", "Movies/Missing.mkv", 0),
        ],
    )
    db.execute(
        "INSERT INTO owned_links VALUES ('default','global','Movies/Test.mkv',?)",
        (target,),
    )
    db.commit()
    db.close()
    return path


def contents(path):
    db = connect_database(path)
    try:
        return data_snapshot(db)
    finally:
        db.close()


class MigrationTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.path = create_legacy(self.root)
        self.before = contents(self.path)

    def assert_legacy_intact(self):
        # Opening writable allows SQLite to recover a hot journal after a crash.
        db = connect_database(self.path, writable=True)
        try:
            self.assertEqual(db.execute("PRAGMA user_version").fetchone()[0], 1)
            self.assertIsNone(
                db.execute(
                    "SELECT name FROM sqlite_schema WHERE name='schema_migrations'"
                ).fetchone()
            )
            self.assertEqual(data_snapshot(db), self.before)
        finally:
            db.close()

    def test_upgrade_preserves_catalog_and_verifies_backup(self):
        link = self.root / "plex/Movies/Test.mkv"
        before_link = link.lstat()
        source = self.root / "media/movie.mkv"
        source_before = source.read_bytes()
        result = upgrade_database(self.path)
        self.assertTrue(result["upgraded"])
        self.assertEqual(result["schema"], SCHEMA_VERSION)
        self.assertEqual(result["database_id"], DATABASE_ID)
        self.assertEqual(
            {k: v for k, v in contents(self.path).items() if k in self.before},
            self.before,
        )
        self.assertEqual(link.lstat().st_ino, before_link.st_ino)
        self.assertEqual(link.lstat().st_mtime_ns, before_link.st_mtime_ns)
        self.assertEqual(source.read_bytes(), source_before)
        backup = Path(result["backup"])
        manifest = json.loads((backup.parent / "manifest.json").read_text())
        self.assertEqual(
            manifest["sha256"], hashlib.sha256(backup.read_bytes()).hexdigest()
        )
        self.assertEqual(manifest["status"], "committed")
        self.assertEqual(inspect_database(backup, full=True)["schema"], 1)
        self.assertEqual(contents(backup), self.before)
        self.assertEqual(backup.stat().st_mode & 0o777, 0o600)
        self.assertEqual(list(backup.parent.glob("rehearsal-*")), [])
        history = inspect_database(self.path)["history"]
        self.assertEqual(
            [row["origin"] for row in history],
            ["baseline"] + ["migrated"] * (SCHEMA_VERSION - 1),
        )
        with Store(self.path) as store:
            self.assertEqual(store.database_id, DATABASE_ID)

    def test_dry_run_and_status_leave_database_and_directory_unchanged(self):
        before_bytes = self.path.read_bytes()
        before_entries = set(self.root.iterdir())
        for info in (
            inspect_database(self.path),
            upgrade_database(self.path, dry_run=True),
        ):
            self.assertEqual(
                [step["filename"] for step in info["pending_migrations"]],
                [step.filename for step in load_migrations()[1:]],
            )
        self.assertEqual(self.path.read_bytes(), before_bytes)
        self.assertEqual(set(self.root.iterdir()), before_entries)

    def test_normal_commands_refuse_legacy_without_migrating(self):
        with self.assertRaisesRegex(CatabolicError, "db upgrade"):
            Store(self.path)
        self.assert_legacy_intact()

    def test_repeated_upgrade_does_not_create_another_backup(self):
        first = upgrade_database(self.path)
        directory = Path(first["backup"]).parent.parent
        before = set(directory.iterdir())
        self.assertFalse(upgrade_database(self.path)["upgraded"])
        self.assertEqual(set(directory.iterdir()), before)

    def test_future_unknown_and_damaged_schemas_are_refused(self):
        for statement in (
            "PRAGMA user_version=99",
            "PRAGMA user_version=0",
            "CREATE TABLE unknown_data(value TEXT)",
        ):
            with (
                self.subTest(statement=statement),
                tempfile.TemporaryDirectory(dir=self.root) as temporary,
            ):
                path = create_legacy(Path(temporary))
                db = sqlite3.connect(path)
                db.execute(statement)
                db.commit()
                db.close()
                before = path.read_bytes()
                with self.assertRaises(CatabolicError):
                    upgrade_database(path)
                self.assertEqual(path.read_bytes(), before)
                self.assertFalse(Path(str(path) + ".backups").exists())

    def test_history_tampering_and_released_file_edits_are_detected(self):
        upgrade_database(self.path)
        migrations = load_migrations()
        altered = (
            *migrations[:1],
            replace(
                migrations[1], sql=migrations[1].sql + "\n-- changed after release\n"
            ),
            *migrations[2:],
        )
        with self.assertRaisesRegex(CatabolicError, "released migration"):
            upgrade_database(self.path, migrations=altered)
        db = sqlite3.connect(self.path)
        db.execute(
            "UPDATE schema_migrations SET checksum=? WHERE version=1", ("0" * 64,)
        )
        db.commit()
        db.close()
        with self.assertRaisesRegex(CatabolicError, "released migration"):
            upgrade_database(self.path)
        with self.assertRaises(CatabolicError):
            Store(self.path)

    def test_missing_history_is_not_silently_reconstructed(self):
        upgrade_database(self.path)
        db = sqlite3.connect(self.path)
        db.execute("DELETE FROM schema_migrations WHERE version=1")
        db.commit()
        db.close()
        with self.assertRaisesRegex(CatabolicError, "history is incomplete"):
            upgrade_database(self.path)

    def test_pending_operations_in_another_profile_block_upgrade(self):
        db = sqlite3.connect(self.path)
        db.execute(
            "INSERT INTO journal VALUES ('pending','laptop','global','pending.mkv','create','target',NULL)"
        )
        db.commit()
        db.close()
        self.assertFalse(upgrade_database(self.path, dry_run=True)["safe"])
        with self.assertRaisesRegex(CatabolicError, "every profile"):
            upgrade_database(self.path)
        self.assertFalse(Path(str(self.path) + ".backups").exists())

    def test_backup_failure_cannot_modify_the_database(self):
        with patch(
            "catabolic.migration._copy_database",
            side_effect=OSError(errno.ENOSPC, "disk full"),
        ):
            with self.assertRaises(OSError):
                upgrade_database(self.path)
        self.assert_legacy_intact()

    def test_bad_backup_is_rejected_before_rehearsal(self):
        from catabolic.migration import _copy_database

        def corrupt_copy(source, path):
            _copy_database(source, path)
            db = sqlite3.connect(path)
            db.execute("UPDATE items SET metadata='{}'")
            db.commit()
            db.close()

        with patch("catabolic.migration._copy_database", side_effect=corrupt_copy):
            with self.assertRaisesRegex(CatabolicError, "backup contents differ"):
                upgrade_database(self.path)
        self.assert_legacy_intact()

    def test_backup_destination_cannot_overlap_sources_or_outputs(self):
        for directory in (self.root / "media/backups", self.root / "plex/backups"):
            with (
                self.subTest(directory=directory),
                self.assertRaisesRegex(CatabolicError, "overlaps"),
            ):
                upgrade_database(self.path, backup_dir=directory)
            self.assertFalse(directory.exists())
        self.assert_legacy_intact()

    def test_custom_backup_destination(self):
        result = upgrade_database(self.path, backup_dir=self.root / "safe-backups")
        self.assertTrue(
            Path(result["backup"]).is_relative_to(self.root / "safe-backups")
        )

    def test_rehearsal_rejects_data_loss_and_keeps_original(self):
        for sql in (
            "DROP TABLE identities;",
            "UPDATE items SET metadata='{}';",
            "DELETE FROM owned_links;",
        ):
            with self.subTest(sql=sql):
                migrations = (
                    *load_migrations(),
                    Migration(
                        SCHEMA_VERSION + 1,
                        f"{SCHEMA_VERSION + 1:03d}_bad_change.sql",
                        sql,
                    ),
                )
                with self.assertRaisesRegex(CatabolicError, "existing|preservation"):
                    upgrade_database(self.path, migrations=migrations)
                self.assert_legacy_intact()

    def test_skipped_versions_apply_every_missing_migration(self):
        migrations = (
            *load_migrations(),
            Migration(
                SCHEMA_VERSION + 1,
                f"{SCHEMA_VERSION + 1:03d}_add_notes.sql",
                "ALTER TABLE items ADD COLUMN note TEXT;",
            ),
            Migration(
                SCHEMA_VERSION + 2,
                f"{SCHEMA_VERSION + 2:03d}_add_audit.sql",
                "CREATE TABLE audit(id INTEGER PRIMARY KEY, item_id TEXT REFERENCES items(id));",
            ),
        )
        result = upgrade_database(self.path, migrations=migrations)
        self.assertEqual(
            [row["version"] for row in result["applied"]],
            list(range(2, SCHEMA_VERSION + 3)),
        )
        self.assertEqual(
            inspect_database(self.path, migrations=migrations, full=True)["schema"],
            SCHEMA_VERSION + 2,
        )
        db = connect_database(self.path)
        try:
            validate_preservation(db, self.before)
        finally:
            db.close()

    def test_history_trigger_cannot_bypass_data_preservation(self):
        migrations = (
            *load_migrations(),
            Migration(
                SCHEMA_VERSION + 1,
                f"{SCHEMA_VERSION + 1:03d}_bad_trigger.sql",
                """CREATE TRIGGER change_metadata AFTER INSERT ON schema_migrations
                BEGIN UPDATE items SET metadata='{}'; END;""",
            ),
        )
        with self.assertRaisesRegex(CatabolicError, "preservation"):
            upgrade_database(self.path, migrations=migrations)
        self.assert_legacy_intact()

    def test_migration_cannot_commit_or_attach_another_database(self):
        for sql in (
            "COMMIT;",
            "PRAGMA user_version=999;",
            "ATTACH ':memory:' AS other;",
        ):
            with self.subTest(sql=sql):
                migrations = (
                    *load_migrations(),
                    Migration(
                        SCHEMA_VERSION + 1, f"{SCHEMA_VERSION + 1:03d}_unsafe.sql", sql
                    ),
                )
                with self.assertRaises(CatabolicError):
                    upgrade_database(self.path, migrations=migrations)
                self.assert_legacy_intact()

    def test_semicolons_in_triggers_and_strings_are_parsed_correctly(self):
        migrations = (
            *load_migrations(),
            Migration(
                SCHEMA_VERSION + 1,
                f"{SCHEMA_VERSION + 1:03d}_trigger.sql",
                """CREATE TABLE notes(value TEXT DEFAULT 'a;b');
        CREATE TRIGGER keep_notes AFTER INSERT ON notes BEGIN SELECT 1; SELECT 2; END;
        -- trailing comment with a semicolon;
        """,
            ),
        )
        self.assertTrue(upgrade_database(self.path, migrations=migrations)["upgraded"])
        self.assertEqual(
            inspect_database(self.path, migrations=migrations)["schema"],
            SCHEMA_VERSION + 1,
        )

    def test_foreign_key_damage_is_rejected_before_backup(self):
        db = sqlite3.connect(self.path)
        db.execute("UPDATE mappings SET item_id='missing-item'")
        db.commit()
        db.close()
        with self.assertRaisesRegex(CatabolicError, "foreign-key"):
            upgrade_database(self.path)
        self.assertFalse(Path(str(self.path) + ".backups").exists())

    def test_other_writer_blocks_upgrade(self):
        with (
            writer_lock(self.path),
            self.assertRaisesRegex(CatabolicError, "another Catabolic writer"),
        ):
            upgrade_database(self.path)
        self.assert_legacy_intact()

    def test_backup_includes_committed_wal_data(self):
        connection = sqlite3.connect(self.path)
        try:
            self.assertEqual(
                connection.execute("PRAGMA journal_mode=WAL").fetchone()[0], "wal"
            )
            connection.execute("PRAGMA wal_autocheckpoint=0")
            connection.execute(
                "UPDATE items SET metadata=?", ('{"title":"committed in WAL"}',)
            )
            connection.commit()
            self.assertGreater(Path(str(self.path) + "-wal").stat().st_size, 0)
            expected = contents(self.path)
            result = upgrade_database(self.path)
            self.assertEqual(contents(Path(result["backup"])), expected)
            self.assertEqual(
                {k: v for k, v in contents(self.path).items() if k in expected},
                expected,
            )
            self.assertEqual(
                inspect_database(self.path, full=True)["schema"], SCHEMA_VERSION
            )
        finally:
            connection.close()

    def test_committed_upgrade_survives_a_manifest_write_failure(self):
        from catabolic.migration import _write_manifest

        def fail_final_manifest(directory, manifest):
            if manifest["status"] == "committed":
                raise OSError(errno.ENOSPC, "backup device full")
            _write_manifest(directory, manifest)

        with patch(
            "catabolic.migration._write_manifest", side_effect=fail_final_manifest
        ):
            result = upgrade_database(self.path)
        self.assertTrue(result["upgraded"])
        self.assertEqual(len(result["warnings"]), 1)
        self.assertEqual(
            inspect_database(self.path, full=True)["schema"], SCHEMA_VERSION
        )
        self.assertEqual(contents(Path(result["backup"])), self.before)

    def test_changes_after_backup_cancel_upgrade_without_losing_the_edit(self):
        def change_live(stage, _db, _version):
            if stage == "upgrade:after_rehearsal":
                external = sqlite3.connect(self.path)
                external.execute(
                    "UPDATE items SET metadata=?", ('{"external":"new value"}',)
                )
                external.commit()
                external.close()

        with patch("catabolic.migration._checkpoint", side_effect=change_live):
            with self.assertRaisesRegex(CatabolicError, "changed after backup"):
                upgrade_database(self.path)
        db = connect_database(self.path)
        try:
            self.assertEqual(db.execute("PRAGMA user_version").fetchone()[0], 1)
            self.assertEqual(
                db.execute("SELECT metadata FROM items").fetchone()[0],
                '{"external":"new value"}',
            )
        finally:
            db.close()

    def test_sqlite_full_rolls_back_the_actual_upgrade(self):
        def limit_pages(stage, db, _version):
            if stage == "upgrade:before_migration":
                pages = db.execute("PRAGMA page_count").fetchone()[0]
                db.execute(f"PRAGMA max_page_count={pages}")

        with patch("catabolic.migration._checkpoint", side_effect=limit_pages):
            with self.assertRaisesRegex(CatabolicError, "full"):
                upgrade_database(self.path)
        self.assert_legacy_intact()

    def test_process_exit_is_atomic_before_and_after_commit(self):
        worker = """
import os,sys
import catabolic.migration as migration
def stop(stage, db, version):
    if stage == sys.argv[2]:
        os._exit(86)
migration._checkpoint=stop
migration.upgrade_database(sys.argv[1])
"""
        for stage, expected in (
            ("upgrade:after_migration", 1),
            ("upgrade:before_commit", 1),
            ("upgrade:after_commit", SCHEMA_VERSION),
        ):
            with (
                self.subTest(stage=stage),
                tempfile.TemporaryDirectory(dir=self.root) as temporary,
            ):
                path = create_legacy(Path(temporary))
                before = contents(path)
                child = subprocess.run(
                    [sys.executable, "-c", worker, str(path), stage],
                    capture_output=True,
                    text=True,
                    timeout=30,
                )
                self.assertEqual(child.returncode, 86, child.stderr)
                db = connect_database(path, writable=True)
                try:
                    self.assertEqual(
                        db.execute("PRAGMA user_version").fetchone()[0], expected
                    )
                    self.assertEqual(
                        {k: v for k, v in data_snapshot(db).items() if k in before},
                        before,
                    )
                finally:
                    db.close()
                backups_before = list(Path(str(path) + ".backups").iterdir())
                self.assertEqual(len(backups_before), 1)
                result = upgrade_database(path)
                self.assertEqual(result["upgraded"], expected == 1)
                self.assertEqual(
                    inspect_database(path, full=True)["schema"], SCHEMA_VERSION
                )

    def test_exit_between_versions_rolls_back_the_whole_chain(self):
        worker = """
import os, sys
import catabolic.migration as migration
steps = (*migration.load_migrations(), migration.Migration(
    migration.SCHEMA_VERSION + 1, f'{migration.SCHEMA_VERSION + 1:03d}_add_notes.sql', 'ALTER TABLE items ADD COLUMN note TEXT;'))
def stop(stage, db, version):
    if stage == 'upgrade:after_migration' and version == 2:
        os._exit(86)
migration._checkpoint = stop
migration.upgrade_database(sys.argv[1], migrations=steps)
"""
        child = subprocess.run(
            [sys.executable, "-c", worker, str(self.path)],
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(child.returncode, 86, child.stderr)
        self.assert_legacy_intact()
        steps = (
            *load_migrations(),
            Migration(
                SCHEMA_VERSION + 1,
                f"{SCHEMA_VERSION + 1:03d}_add_notes.sql",
                "ALTER TABLE items ADD COLUMN note TEXT;",
            ),
        )
        result = upgrade_database(self.path, migrations=steps)
        self.assertEqual(
            [row["version"] for row in result["applied"]],
            list(range(2, SCHEMA_VERSION + 2)),
        )
        self.assertEqual(
            inspect_database(self.path, migrations=steps, full=True)["schema"],
            SCHEMA_VERSION + 1,
        )

    def test_fresh_initialization_and_upgrade_have_the_same_schema(self):
        upgrade_database(self.path)
        fresh = self.root / "fresh.sqlite3"
        Store.initialize(fresh)

        def schema(path):
            db = connect_database(path)
            try:
                return [
                    tuple(row)
                    for row in db.execute(
                        "SELECT type,name,sql FROM sqlite_schema ORDER BY name"
                    )
                ]
            finally:
                db.close()

        self.assertEqual(schema(self.path), schema(fresh))
        self.assertEqual(
            [row["origin"] for row in inspect_database(fresh)["history"]],
            ["initialized"] * SCHEMA_VERSION,
        )

    def test_cli_upgrade_and_legacy_recovery(self):
        target = os.path.relpath(
            self.root / "media/movie.mkv", self.root / "plex/Recovery"
        )
        db = sqlite3.connect(self.path)
        db.execute(
            "INSERT INTO mappings VALUES ('recovery-map','global','file-a','item-a','Recovery/Test.mkv',1)"
        )
        db.execute(
            "INSERT INTO journal VALUES ('pending','default','global','Recovery/Test.mkv','create',?,NULL)",
            (target,),
        )
        db.commit()
        db.close()

        def run(*args, expected=0):
            result = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "catabolic",
                    "--json",
                    "--db",
                    str(self.path),
                    *args,
                ],
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertEqual(result.returncode, expected, result.stderr)
            return json.loads(result.stdout if expected in (0, 3) else result.stderr)

        run("status", expected=2)
        self.assertEqual(run("db", "status", expected=3)["schema"], 1)
        run("db", "upgrade", "--dry-run", expected=3)
        run("db", "upgrade", expected=2)
        self.assertEqual(run("recover")["recovered"], ["pending"])
        self.assertTrue(run("db", "upgrade")["upgraded"])
        self.assertTrue(run("verify")["healthy"])
        self.assertFalse(run("db", "upgrade")["upgraded"])

    def test_migration_number_gaps_and_duplicate_versions_are_rejected(self):
        for steps in (
            (load_migrations()[1],),
            (load_migrations()[0], load_migrations()[0]),
        ):
            with self.assertRaises(CatabolicError):
                validate_sequence(steps)


if __name__ == "__main__":
    unittest.main()
