# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Numbered, checked migrations with a verified backup and a rehearsal."""

from __future__ import annotations

import hashlib
import json
import os
import re
import sqlite3
import tempfile
from dataclasses import dataclass
from datetime import datetime, timezone
from functools import lru_cache
from importlib.resources import files
from pathlib import Path
from uuid import UUID, uuid4

from .database_io import connect_database, database_path, writer_lock
from .domain import CatabolicError

SCHEMA_VERSION = 20
HISTORY_TABLE = "schema_migrations"


@dataclass(frozen=True)
class Migration:
    version: int
    filename: str
    sql: str

    @property
    def checksum(self) -> str:
        return hashlib.sha256(self.sql.encode("utf-8")).hexdigest()

    def description(self) -> dict:
        return {
            "version": self.version,
            "filename": self.filename,
            "checksum": self.checksum,
        }


def validate_sequence(migrations: tuple[Migration, ...]):
    if not migrations or [step.version for step in migrations] != list(
        range(1, len(migrations) + 1)
    ):
        raise CatabolicError("migration versions must start at 001 and be contiguous")
    for step in migrations:
        if not re.fullmatch(rf"{step.version:03d}_[a-z0-9_]+\.sql", step.filename):
            raise CatabolicError(f"invalid migration filename: {step.filename}")


@lru_cache(maxsize=1)
def load_migrations() -> tuple[Migration, ...]:
    result = []
    for resource in sorted(
        files("catabolic").joinpath("migrations").iterdir(), key=lambda item: item.name
    ):
        if not resource.name.endswith(".sql"):
            continue
        match = re.fullmatch(r"([0-9]{3})_[a-z0-9_]+\.sql", resource.name)
        if not match:
            raise CatabolicError(f"invalid migration resource: {resource.name}")
        result.append(
            Migration(
                int(match[1]), resource.name, resource.read_text(encoding="utf-8")
            )
        )
    migrations = tuple(result)
    validate_sequence(migrations)
    if migrations[-1].version != SCHEMA_VERSION:
        raise CatabolicError(
            "packaged migrations do not match the supported schema version"
        )
    return migrations


def _statements(sql: str):
    buffer = []
    for character in sql:
        buffer.append(character)
        if character == ";" and sqlite3.complete_statement("".join(buffer)):
            yield "".join(buffer)
            buffer = []
    remainder = "".join(buffer).strip()
    if remainder:
        if not sqlite3.complete_statement(remainder + "\n;"):
            raise CatabolicError("incomplete SQL at end of migration")
        yield remainder


def _authorize(action, arg1, arg2, _database, _trigger):
    # The runner, not migration SQL, owns transactions, attached databases,
    # connection settings, version changes, and migration-history records.
    if action in (
        sqlite3.SQLITE_TRANSACTION,
        sqlite3.SQLITE_SAVEPOINT,
        sqlite3.SQLITE_ATTACH,
        sqlite3.SQLITE_DETACH,
        sqlite3.SQLITE_PRAGMA,
    ):
        return sqlite3.SQLITE_DENY
    if arg1 == HISTORY_TABLE and action in (
        sqlite3.SQLITE_INSERT,
        sqlite3.SQLITE_UPDATE,
        sqlite3.SQLITE_DELETE,
        sqlite3.SQLITE_DROP_TABLE,
        sqlite3.SQLITE_ALTER_TABLE,
    ):
        return sqlite3.SQLITE_DENY
    if action == sqlite3.SQLITE_ALTER_TABLE and arg2 == HISTORY_TABLE:
        return sqlite3.SQLITE_DENY
    return sqlite3.SQLITE_OK


def execute_migration(db: sqlite3.Connection, step: Migration):
    query_upgrade = (
        step.version == 15 and step.filename == "015_queries_projections.sql"
    )
    if query_upgrade:
        # Frozen v1 normalization for adopted definitions, independent of future
        # query validators. Keep original rule/layout JSON untouched.
        def query_v1(raw, profile):
            value = json.loads(raw)
            value["selection"] = {"profile": profile, **value["selection"]}
            return json.dumps(
                value, sort_keys=True, ensure_ascii=False, allow_nan=False
            )

        db.create_function("catabolic_query_v1", 2, query_v1, deterministic=True)
        db.create_function(
            "catabolic_sha256_v1",
            1,
            lambda raw: hashlib.sha256(raw.encode()).hexdigest(),
            deterministic=True,
        )
    rebuild = step.version == 16 and step.filename == "016_operation_rules.sql"
    if rebuild:
        db.execute("PRAGMA defer_foreign_keys=ON")

    def authorize(*args):
        # SQLite 3.37+ validates ADD COLUMN CHECK with this internal read-only
        # pragma. No migration SQL gains transaction or connection control.
        if rebuild and args[:3] == (
            sqlite3.SQLITE_PRAGMA,
            "quick_check",
            "processing_recipes",
        ):
            return sqlite3.SQLITE_OK
        return _authorize(*args)

    db.set_authorizer(authorize)
    try:
        # executescript() can implicitly commit. Execute complete statements
        # individually, preserving the runner's transaction boundary.
        for statement in _statements(step.sql):
            db.execute(statement)
    except sqlite3.Error as exc:
        raise CatabolicError(f"migration {step.filename} failed: {exc}") from exc
    finally:
        db.set_authorizer(None)
        if query_upgrade:
            db.create_function("catabolic_query_v1", 2, None)
            db.create_function("catabolic_sha256_v1", 1, None)
    if rebuild:
        # SQLite retains deferred DROP TABLE violations even when a replacement
        # restores every referenced row. Check the final graph before clearing
        # that bookkeeping; the caller still owns the atomic transaction.
        if db.execute("PRAGMA foreign_key_check").fetchone() is not None:
            raise CatabolicError("operation migration violated a foreign key")
        db.execute("PRAGMA defer_foreign_keys=OFF")


def _schema(db: sqlite3.Connection) -> tuple:
    return tuple(
        (row[0], row[1], re.sub(r"\s+", " ", row[2]).strip())
        for row in db.execute(
            "SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY type,name"
        )
    )


@lru_cache(maxsize=32)
def _expected_schema(migrations: tuple[Migration, ...], version: int) -> tuple:
    db = sqlite3.connect(":memory:", isolation_level=None)
    try:
        db.execute("PRAGMA foreign_keys=ON")
        db.execute("BEGIN")
        for step in migrations[:version]:
            execute_migration(db, step)
        return _schema(db)
    finally:
        db.close()


def validate_database(
    db: sqlite3.Connection, migrations: tuple[Migration, ...], *, full: bool = False
) -> dict:
    version = db.execute("PRAGMA user_version").fetchone()[0]
    latest = migrations[-1].version
    if version < 1 or version > latest:
        raise CatabolicError(
            f"unsupported database schema {version}; this application supports upgrades from 1 through {latest}"
        )
    if _schema(db) != _expected_schema(migrations, version):
        raise CatabolicError(
            f"database structure does not match known schema {version}; refusing to modify it"
        )
    row = db.execute("SELECT value FROM meta WHERE key='database_id'").fetchone()
    try:
        database_id = str(UUID(row[0]))
    except (ValueError, TypeError, AttributeError) as exc:
        raise CatabolicError("database identity is missing or invalid") from exc
    history = []
    if version >= 2:
        history = [
            dict(
                zip(
                    ("version", "filename", "checksum", "origin", "applied_at"),
                    tuple(row),
                    strict=True,
                )
            )
            for row in db.execute(
                "SELECT version,filename,checksum,origin,applied_at FROM schema_migrations ORDER BY version"
            )
        ]
        if [row["version"] for row in history] != list(range(1, version + 1)):
            raise CatabolicError("migration history is incomplete or inconsistent")
        for row, step in zip(history, migrations[:version], strict=True):
            if row["filename"] != step.filename or row["checksum"] != step.checksum:
                raise CatabolicError(
                    f"released migration was changed or history is damaged: {step.filename}"
                )
    if full:
        integrity = [row[0] for row in db.execute("PRAGMA integrity_check")]
        if integrity != ["ok"]:
            raise CatabolicError(f"database integrity check failed: {integrity[:3]}")
        if db.execute("PRAGMA foreign_key_check").fetchone() is not None:
            raise CatabolicError("database foreign-key check failed")
        if (
            version >= 3
            and db.execute("""SELECT 1 FROM mappings m WHERE m.active=1
            AND NOT EXISTS (SELECT 1 FROM item_files a WHERE a.file_id=m.file_id
                AND a.item_id=m.item_id AND a.active=1) LIMIT 1""").fetchone()
        ):
            raise CatabolicError(
                "an active catalog mapping has no active file identification"
            )
    pending_operations = db.execute("SELECT count(*) FROM journal").fetchone()[0]
    return {
        "database_id": database_id,
        "schema": version,
        "latest_schema": latest,
        "history": history,
        "pending_operations": pending_operations,
        "pending_migrations": [step.description() for step in migrations[version:]],
    }


def _identifier(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def _digest_table(db: sqlite3.Connection, table: str, columns: list[str]) -> dict:
    selected = ",".join(_identifier(column) for column in columns)
    digest = hashlib.sha256()
    count = 0
    for row in db.execute(
        f"SELECT {selected} FROM {_identifier(table)} ORDER BY {selected}"
    ):
        typed = [
            [type(value).__name__, value.hex() if isinstance(value, bytes) else value]
            for value in row
        ]
        digest.update(
            json.dumps(
                typed, ensure_ascii=True, allow_nan=False, separators=(",", ":")
            ).encode()
            + b"\n"
        )
        count += 1
    return {"columns": columns, "rows": count, "sha256": digest.hexdigest()}


def data_snapshot(db: sqlite3.Connection) -> dict:
    result = {}
    for row in db.execute(
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
    ):
        table = row[0]
        if table == HISTORY_TABLE:
            continue
        columns = [
            column[1]
            for column in db.execute(f"PRAGMA table_info({_identifier(table)})")
        ]
        result[table] = _digest_table(db, table, columns)
    return result


def validate_preservation(db: sqlite3.Connection, before: dict):
    for table, expected in before.items():
        columns = [
            column[1]
            for column in db.execute(f"PRAGMA table_info({_identifier(table)})")
        ]
        if not set(expected["columns"]).issubset(columns):
            raise CatabolicError(
                f"migration removed existing table or columns: {table}"
            )
        if _digest_table(db, table, expected["columns"]) != expected:
            raise CatabolicError(
                f"migration changed existing data in {table}; preservation validation failed"
            )


def _record_history(
    db: sqlite3.Connection,
    migrations: tuple[Migration, ...],
    version: int,
    *,
    initializing: bool,
):
    if version < 2:
        return
    for step in migrations[:version]:
        origin = (
            "initialized"
            if initializing
            else "baseline"
            if step.version == 1
            else "migrated"
        )
        db.execute(
            "INSERT OR IGNORE INTO schema_migrations(version,filename,checksum,origin) VALUES (?,?,?,?)",
            (step.version, step.filename, step.checksum, origin),
        )


def _checkpoint(stage: str, db: sqlite3.Connection, version: int):
    """Fault-injection seam for tests; production does not install callbacks."""


def _apply_chain(
    db: sqlite3.Connection, migrations: tuple[Migration, ...], current: int, role: str
):
    if not db.in_transaction:
        raise CatabolicError("migration runner requires an explicit transaction")
    try:
        for step in migrations[current:]:
            before = data_snapshot(db)
            _checkpoint(f"{role}:before_migration", db, step.version)
            execute_migration(db, step)
            _record_history(db, migrations, step.version, initializing=False)
            db.execute(f"PRAGMA user_version={step.version}")
            # Include effects of any triggers fired by history bookkeeping.
            validate_preservation(db, before)
            validate_database(db, migrations, full=True)
            _checkpoint(f"{role}:after_migration", db, step.version)
        _checkpoint(f"{role}:before_commit", db, migrations[-1].version)
        db.commit()
        _checkpoint(f"{role}:after_commit", db, migrations[-1].version)
    except BaseException:
        if db.in_transaction:
            db.rollback()
        raise


def initialize_database(path: str | Path) -> dict:
    path = Path(path).absolute()
    path = path.parent.resolve() / path.name
    if not path.parent.is_dir():
        raise CatabolicError("database parent must already exist")
    migrations = load_migrations()
    fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    os.close(fd)
    database_id = str(uuid4())
    with writer_lock(path):
        db = connect_database(path, writable=True)
        try:
            db.execute("BEGIN IMMEDIATE")
            for step in migrations:
                execute_migration(db, step)
                if step.version == 1:
                    db.execute(
                        "INSERT INTO meta VALUES ('database_id',?)", (database_id,)
                    )
                    db.execute("INSERT INTO profiles VALUES ('default')")
                    db.execute("INSERT INTO catalogs VALUES ('global')")
                _record_history(db, migrations, step.version, initializing=True)
                db.execute(f"PRAGMA user_version={step.version}")
            validate_database(db, migrations, full=True)
            db.commit()
        finally:
            db.close()
    return {
        "database": str(path),
        "database_id": database_id,
        "schema": migrations[-1].version,
    }


def inspect_database(
    path: str | Path,
    *,
    full: bool = False,
    migrations: tuple[Migration, ...] | None = None,
) -> dict:
    migrations = migrations or load_migrations()
    validate_sequence(migrations)
    path = database_path(path)
    db = connect_database(path)
    try:
        db.execute("BEGIN")
        result = validate_database(db, migrations, full=full)
        return {
            "database": str(path),
            **result,
            "safe": not result["pending_operations"]
            or not result["pending_migrations"],
        }
    finally:
        db.close()


def _fsync(path: Path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _write_manifest(directory: Path, manifest: dict):
    temporary = directory / f"manifest-{uuid4().hex}.tmp"
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as stream:
        json.dump(manifest, stream, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, directory / "manifest.json")
    _fsync(directory)


def _copy_database(source: sqlite3.Connection, path: Path):
    fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    os.close(fd)
    destination = connect_database(path, writable=True)
    try:
        source.backup(destination, pages=256)
        destination.execute("PRAGMA journal_mode=DELETE")
    finally:
        destination.close()


def _backup(
    db: sqlite3.Connection,
    path: Path,
    parent: Path,
    info: dict,
    snapshot: dict,
    migrations: tuple[Migration, ...],
) -> tuple[Path, dict]:
    parent = parent.resolve()
    directory = (
        parent
        / f"{datetime.now(timezone.utc):%Y%m%dT%H%M%SZ}-v{info['schema']}-to-v{info['latest_schema']}-{uuid4().hex[:12]}"
    )
    for row in db.execute("SELECT root FROM bindings"):
        bound = Path(row[0])
        if directory.is_relative_to(bound) or bound.is_relative_to(directory):
            raise CatabolicError(
                "backup directory overlaps a registered source or catalog output"
            )
    if path.is_relative_to(directory):
        raise CatabolicError("backup directory cannot contain the active database")
    parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    directory.mkdir(mode=0o700)
    _fsync(parent)
    partial = directory / "snapshot.sqlite3.partial"
    _copy_database(db, partial)
    backup_db = connect_database(partial)
    try:
        validated = validate_database(backup_db, migrations, full=True)
        if (
            validated["database_id"] != info["database_id"]
            or validated["schema"] != info["schema"]
        ):
            raise CatabolicError("backup identity or schema mismatch")
        if data_snapshot(backup_db) != snapshot:
            raise CatabolicError("backup contents differ from the source snapshot")
    finally:
        backup_db.close()
    _fsync(partial)
    backup = directory / "snapshot.sqlite3"
    partial.rename(backup)
    _fsync(directory)
    with backup.open("rb") as stream:
        checksum = hashlib.file_digest(stream, "sha256").hexdigest()
    manifest = {
        "status": "backup_verified",
        "database": str(path),
        "database_id": info["database_id"],
        "from_schema": info["schema"],
        "to_schema": info["latest_schema"],
        "backup": str(backup),
        "sha256": checksum,
        "tables_before": snapshot,
        "migrations": info["pending_migrations"],
    }
    _write_manifest(directory, manifest)
    return directory, manifest


def upgrade_database(
    path: str | Path,
    *,
    dry_run: bool = False,
    backup_dir: str | Path | None = None,
    migrations: tuple[Migration, ...] | None = None,
) -> dict:
    migrations = migrations or load_migrations()
    validate_sequence(migrations)
    path = database_path(path)
    if dry_run:
        return {
            **inspect_database(path, full=True, migrations=migrations),
            "dry_run": True,
        }
    with writer_lock(path):
        db = connect_database(path, writable=True)
        directory = None
        manifest = None
        try:
            db.execute("BEGIN")
            info = validate_database(db, migrations, full=True)
            if not info["pending_migrations"]:
                return {
                    "database": str(path),
                    "schema": info["schema"],
                    "upgraded": False,
                    "backup": None,
                }
            if info["pending_operations"]:
                raise CatabolicError(
                    "recover pending link operations in every profile before upgrading"
                )
            before = data_snapshot(db)
            directory, manifest = _backup(
                db,
                path,
                Path(backup_dir) if backup_dir else Path(str(path) + ".backups"),
                info,
                before,
                migrations,
            )
            db.rollback()
            _checkpoint("upgrade:after_backup", db, info["schema"])
            with tempfile.TemporaryDirectory(
                prefix="rehearsal-", dir=directory
            ) as temporary:
                snapshot_db = connect_database(Path(manifest["backup"]))
                rehearsal_path = Path(temporary) / "rehearsal.sqlite3"
                try:
                    _copy_database(snapshot_db, rehearsal_path)
                finally:
                    snapshot_db.close()
                rehearsal = connect_database(rehearsal_path, writable=True)
                try:
                    rehearsal.execute("BEGIN IMMEDIATE")
                    _apply_chain(rehearsal, migrations, info["schema"], "rehearsal")
                finally:
                    rehearsal.close()
            manifest["status"] = "rehearsed"
            _write_manifest(directory, manifest)
            _checkpoint("upgrade:after_rehearsal", db, info["schema"])
            # Acquire SQLite's writer reservation before comparing the live
            # database with the backup, closing the backup-to-upgrade race.
            db.execute("BEGIN IMMEDIATE")
            live = validate_database(db, migrations, full=True)
            if live["schema"] != info["schema"] or data_snapshot(db) != before:
                raise CatabolicError(
                    "database changed after backup; upgrade cancelled, rerun db upgrade"
                )
            _apply_chain(db, migrations, info["schema"], "upgrade")
            manifest["status"] = "committed"
            warnings = []
            try:
                _write_manifest(directory, manifest)
            except OSError as error:
                warnings.append(
                    f"upgrade committed, but backup manifest could not be updated: {error}"
                )
            return {
                "database": str(path),
                "database_id": info["database_id"],
                "from_schema": info["schema"],
                "schema": migrations[-1].version,
                "upgraded": True,
                "backup": manifest["backup"],
                "applied": info["pending_migrations"],
                "warnings": warnings,
            }
        except BaseException as error:
            if db.in_transaction:
                db.rollback()
            if directory and manifest:
                manifest["status"] = "failed"
                manifest["error"] = f"{type(error).__name__}: {error}"
                try:
                    _write_manifest(directory, manifest)
                except OSError:
                    pass
            raise
        finally:
            db.close()
