# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native v2 folder mapping apply and recover against frozen Python (track C4).

Both engines run the same CLI command on identical copies of one catalog at
the same paths: frozen first, then the output root and catalog are restored
and native runs. Results, event history, owned links, journals and the output
tree are compared after normalizing event identities and clocks.
"""

import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from contextlib import closing
from pathlib import Path

from tests.test_rust_legacy_review import BINARY, ORACLE, PYTHON, REFERENCE

SEED = r"""
from catabolic.store import Store
from catabolic.app import Application
from catabolic.saved_queries import Queries
import json, sys
database, output, source = sys.argv[1:4]
with Store(database, writable=True) as store:
    app = Application(store)
    app.bind('output', 'global', output)
    app.bind('source', 'media', source)
    app.scan()
    for file in app.files()['files']:
        name = file['path'].rsplit('/', 1)[-1].rsplit('.', 1)[0]
        item = app.put_item('book', {'fixture': name}, {'title': 'Title ' + name + ' Café', 'author': 'Author'})
        with store.transaction() as db:
            db.execute("INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES (?,?,?,'primary','explicit')", ('a-' + name, file['id'], item['id']))
    q = Queries(store).put('selected', {'mode':'selection','selection':{'language':'sql','query':'SELECT item_id FROM catalog_items'}})
    print(q['id'])
"""

INTERRUPTED = r"""
from catabolic.store import Store
from catabolic.app import Application
from catabolic import publication_mappings as p
from uuid import uuid4
import json, sys
database, definition = sys.argv[1], json.loads(sys.argv[2])
with Store(database, writable=True) as store:
    app = Application(store)
    plan = p.prepare(app, definition)
    identifier = str(uuid4())
    with store.transaction():
        staged = p.stage_folder(app, definition)
        assert staged == plan["data"]
        event = p.event_base(definition, plan, definition["destination"]["catalog"])
        event["data"] = staged
        p.record(store, "default", identifier, event, create=True)
    print(identifier)
"""

# Per-run identities and clocks; decisions and paths are compared exactly.
VOLATILE = {"id", "event_id", "elapsed_seconds", "operation_id", "journal_id"}


def normalized(value):
    if isinstance(value, dict):
        return {
            key: normalized(child)
            for key, child in value.items()
            if key not in VOLATILE and not key.endswith("_at")
        }
    if isinstance(value, list):
        return [normalized(child) for child in value]
    if isinstance(value, str) and value[:1] in "{[":
        try:
            return normalized(json.loads(value))
        except ValueError:
            return value
    return value


def payload(stdout):
    """The `--machine` envelope's data, or the document itself."""
    value = json.loads(stdout)
    return value.get("data", value)


class FolderHarness(unittest.TestCase):
    """Sequential frozen/native harness; holds no tests itself."""

    def setUp(self):
        for path in (PYTHON, REFERENCE / "catabolic/publication_mappings.py", BINARY):
            self.assertTrue(path.is_file(), str(path))
        temporary = tempfile.TemporaryDirectory(prefix="publication-folder-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.db = {True: self.root / "python.db", False: self.root / "native.db"}
        self.source, self.output = self.root / "source", self.root / "output"
        self.source.mkdir()
        self.output.mkdir()
        for n in range(3):
            (self.source / f"book{n}.epub").write_bytes(f"book {n}\n".encode())
        self.env = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith("CATABOLIC_") and k not in ("PYTHONPATH", "PYTHONHOME")
        }
        self.env.update(
            HOME=str(self.root / "home"),
            COLUMNS="80",
            TERM="dumb",
            NO_COLOR="1",
            PYTHONUTF8="1",
            PYTHONHASHSEED="0",
        )
        result = self.cli(True, ["init"])
        self.assertEqual(result.returncode, 0, result.stderr)
        result = self.python(
            SEED, str(self.db[True]), str(self.output), str(self.source)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.definition = {
            "version": 2,
            "id": "books-café",
            "query": result.stdout.strip(),
            "destination": {
                "adapter": "komga",
                "operation": "folder",
                "catalog": "global",
            },
        }
        shutil.copyfile(self.db[True], self.db[False])

    def python(self, code, *args):
        return subprocess.run(
            [
                str(PYTHON),
                "-c",
                "import sys;sys.path.insert(0,sys.argv.pop(1));" + code,
                str(REFERENCE),
                *args,
            ],
            env=self.env,
            text=True,
            capture_output=True,
            timeout=60,
        )

    def cli(self, oracle, args):
        command = (
            [str(PYTHON), "-c", ORACLE, str(REFERENCE)] if oracle else [str(BINARY)]
        )
        return subprocess.run(
            command + ["--db", str(self.db[oracle])] + args,
            env=self.env,
            text=True,
            capture_output=True,
            timeout=60,
        )

    def write(self, definition=None):
        path = self.root / "definition.json"
        path.write_text(
            json.dumps(self.definition if definition is None else definition)
        )
        return str(path)

    def tree(self):
        return sorted(
            (
                str(p.relative_to(self.output)),
                os.readlink(p) if p.is_symlink() else None,
            )
            for p in self.output.rglob("*")
        )

    def rows(self, oracle, table, order="rowid"):
        with closing(sqlite3.connect(self.db[oracle])) as db:
            db.row_factory = sqlite3.Row
            return normalized(
                [dict(r) for r in db.execute(f"SELECT * FROM {table} ORDER BY {order}")]
            )

    def state(self, oracle):
        return {
            "events": self.rows(oracle, "consumer_events"),
            "owned_links": self.rows(oracle, "owned_links", "catalog,path"),
            "journal": self.rows(oracle, "journal"),
            "mappings": self.rows(oracle, "mappings", "id"),
            "tree": self.tree(),
        }

    def empty_output(self):
        # The output root's identity is bound: replace only its contents.
        for child in self.output.iterdir():
            if child.is_dir() and not child.is_symlink():
                shutil.rmtree(child)
            else:
                child.unlink()

    def output_snapshot(self):
        copy = self.root / "output-snapshot"
        if copy.exists():
            shutil.rmtree(copy)
        shutil.copytree(self.output, copy, symlinks=True)
        return copy

    def restore_output(self, copy):
        self.empty_output()
        shutil.copytree(copy, self.output, symlinks=True, dirs_exist_ok=True)

    def sequential(self, args, prepare=None):
        """Run `args` on frozen, restore, run on native; return both outcomes."""
        outcome = {}
        snapshot = self.output_snapshot()
        for oracle in (True, False):
            self.restore_output(snapshot)
            if prepare:
                prepare(oracle)
            result = self.cli(oracle, args)
            outcome[oracle] = (result, self.state(oracle))
        return outcome

    def assertSame(self, outcome, *, digests=()):
        (old, old_state), (new, new_state) = outcome[True], outcome[False]
        self.assertEqual(
            new.returncode,
            old.returncode,
            (old.stdout, old.stderr, new.stdout, new.stderr),
        )
        try:
            expected, actual = json.loads(old.stdout), json.loads(new.stdout)
        except ValueError:
            self.assertEqual(
                new.stdout.replace(str(self.db[False]), "<DB>"),
                old.stdout.replace(str(self.db[True]), "<DB>"),
            )
        else:
            data = [value.get("data", value) for value in (actual, expected)]
            for key in digests:
                # Digests over per-engine event ids: equal shape, not value.
                for value in data:
                    self.assertRegex(value[key], "^[0-9a-f]{64}$")
                    value[key] = "<digest>"
            self.assertEqual(normalized(actual), normalized(expected))
        self.assertEqual(new_state, old_state)
        return payload(old.stdout) if old.stdout.startswith("{") else None

    def preview_plan(self):
        ids = set()
        for oracle in (True, False):
            result = self.cli(
                oracle,
                [
                    "--machine",
                    "projection",
                    "mapping-preview",
                    "--definition",
                    self.write(),
                ],
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            ids.add(payload(result.stdout)["plan_id"])
        self.assertEqual(len(ids), 1, ids)
        return ids.pop()

    def apply_args(self, plan, *extra):
        return [
            "--machine",
            "projection",
            "mapping-apply",
            "--definition",
            self.write(),
            "--expected-plan",
            plan,
            *extra,
        ]


class NativeFolderMappingTest(FolderHarness):
    def test_folder_apply_publishes_records_and_repeats_like_frozen(self):
        plan = self.preview_plan()
        result = self.assertSame(self.sequential(self.apply_args(plan)))
        self.assertTrue(result["applied"], result)
        self.assertEqual(result["results"][0]["state"], "verified")
        self.assertEqual(sum(1 for _, target in self.tree() if target), 3)
        # A repeat preview is unchanged and a repeat apply publishes nothing.
        plan = self.preview_plan()
        result = self.assertSame(self.sequential(self.apply_args(plan)))
        self.assertEqual(result["execution"]["applied"], [])

    def test_unbounded_removal_budget_publishes_like_frozen(self):
        # Frozen integers are unbounded; a budget beyond u64 is accepted and
        # can never be exceeded (Sol round 5).
        plan = self.preview_plan()
        args = self.apply_args(plan, "--max-removals", str(2**64))
        result = self.assertSame(self.sequential(args))
        self.assertTrue(result["applied"], result)
        self.assertEqual(sum(1 for _, target in self.tree() if target), 3)

    def test_stale_plan_budget_and_missing_plan_match_frozen(self):
        plan = self.preview_plan()
        for args in (
            self.apply_args("stale"),
            self.apply_args(""),
            self.apply_args(plan, "--max-removals", "-1"),
        ):
            with self.subTest(args=args[-2:]):
                self.assertSame(self.sequential(args))
        self.assertEqual(self.tree(), [])

    def test_interrupted_event_recovers_only_authorized_actions(self):
        # Frozen's own run() up to the event record: staging and the started
        # event commit, and the process is lost before Reconciler.apply.
        result = self.python(
            INTERRUPTED, str(self.db[True]), json.dumps(self.definition)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        event = result.stdout.strip()
        self.assertEqual(self.tree(), [])
        base = self.root / "recover-base.db"
        shutil.copyfile(self.db[True], base)

        def reset(oracle):
            shutil.copyfile(base, self.db[oracle])

        for flags in ([], ["--apply"]):
            with self.subTest(flags=flags):
                result = self.assertSame(
                    self.sequential(
                        ["--machine", "projection", "mapping-recover", event, *flags],
                        reset,
                    )
                )
                self.assertEqual(result["recorded"], bool(flags), result)
                self.assertEqual(
                    result["state"], "verified" if flags else "uncertain", result
                )
        self.assertEqual(sum(1 for _, target in self.tree() if target), 3)

    def test_failed_publication_leaves_an_uncertain_event_then_recovers(self):
        plan = self.preview_plan()
        # The output root refuses writes, so publication fails after the
        # event is recorded; both engines swallow the error into `uncertain`.
        self.output.chmod(0o555)
        self.addCleanup(self.output.chmod, 0o755)
        outcome = self.sequential(self.apply_args(plan))
        self.output.chmod(0o755)
        result = self.assertSame(outcome)
        self.assertEqual(result["state"], "uncertain", result)
        self.assertEqual(result["safe_to_retry"], False, result)
        self.assertFalse(result["applied"], result)
        event = result["event_id"]
        # Each engine's own event id is recorded; recover each with its own.
        events = {}
        for oracle in (True, False):
            with closing(sqlite3.connect(self.db[oracle])) as db:
                (events[oracle],) = db.execute(
                    "SELECT id FROM consumer_events WHERE event='mapping_publish'"
                ).fetchone()
        self.assertEqual(events[True], event)
        outcome = {}
        snapshot = self.output_snapshot()
        for oracle in (True, False):
            self.restore_output(snapshot)
            command = [
                "--machine",
                "projection",
                "mapping-recover",
                events[oracle],
                "--apply",
            ]
            outcome[oracle] = (self.cli(oracle, command), self.state(oracle))
        result = self.assertSame(outcome, digests=("plan_id",))
        self.assertEqual(result["state"], "verified", result)
        self.assertEqual(sum(1 for _, target in self.tree() if target), 3)

    def test_recovery_refuses_unauthorized_or_changed_folders_like_frozen(self):
        plan = self.preview_plan()
        result = self.cli(True, self.apply_args(plan))
        self.assertEqual(result.returncode, 0, result.stderr)
        event = payload(result.stdout)["results"][0]["event_id"]
        with closing(sqlite3.connect(self.db[True])) as db:
            (subject,) = db.execute(
                "SELECT subject FROM consumer_events WHERE id=?", (event,)
            ).fetchone()
            subject = json.loads(subject)
            subject["state"] = "uncertain"
            db.execute(
                "UPDATE consumer_events SET subject=?,severity='warning' WHERE id=?",
                (json.dumps(subject), event),
            )
            db.commit()
        # A link lost after publication is a create with a recorded previous
        # target, which the staged plan never authorized.
        next(p for p in self.output.rglob("*") if p.is_symlink()).unlink()
        base = self.root / "recover-base.db"
        shutil.copyfile(self.db[True], base)

        def reset(oracle):
            shutil.copyfile(base, self.db[oracle])

        args = ["--machine", "projection", "mapping-recover", event, "--apply"]
        self.assertSame(self.sequential(args, reset))
        with closing(sqlite3.connect(base)) as db:
            db.execute(
                "DELETE FROM mappings WHERE rowid=(SELECT min(rowid) FROM mappings)"
            )
            db.commit()
        self.assertSame(self.sequential(args, reset))

    def test_verified_and_unknown_events_match_frozen(self):
        plan = self.preview_plan()
        result = self.cli(True, self.apply_args(plan))
        event = payload(result.stdout)["results"][0]["event_id"]
        shutil.copyfile(self.db[True], self.db[False])
        for args in (
            ["--machine", "projection", "mapping-recover", event],
            ["--machine", "projection", "mapping-recover", event, "--apply"],
            ["--machine", "projection", "mapping-recover", "absent", "--apply"],
            [
                "--machine",
                "projection",
                "mapping-recover",
                event,
                "--resolution",
                "submitted",
            ],
        ):
            with self.subTest(args=args[3:]):
                self.assertSame(self.sequential(args))


if __name__ == "__main__":
    unittest.main()
