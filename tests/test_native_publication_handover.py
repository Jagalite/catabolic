# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Publication handover between engines at every crash state (track C6).

Direction A: the native CLI is killed at each publication crash point (the
item 1 failpoints) and frozen Python recovers. Direction B: frozen Python is
interrupted before or after each filesystem effect and native code recovers.

Frozen recovery may succeed or refuse; a refusal must leave the output tree
and journal exactly as it found them. In every case native recovery and a
fresh apply then converge: healthy, an empty journal, no leftover private or
temporary entries, and the source bytes unchanged. Each outcome, including
every non-destructive residual the other engine left, is written to
``$CATABOLIC_HANDOVER_REPORT`` when set.
"""

import hashlib
import json
import os
import subprocess
import unittest
from unittest.mock import patch

from catabolic.domain import CatabolicError
from tests import test_application as reference
from tests.test_rust_migration import BINARY
from tests.test_rust_observation_filesystem import native

# Crash points each one-operation native plan can reach, per kind. Replace
# and remove use exchange where the filesystem supports it and otherwise
# quarantine; stages of the other mode are simply not reached.
STAGES = {
    "create": [
        "after_journal",
        "after_marker_create",
        "after_marker_write",
        "after_filesystem",
        "before_commit",
        "after_commit",
    ],
    "replace": [
        "after_journal",
        "after_temporary",
        "after_exchange",
        "after_quarantine",
        "after_publish",
        "after_private_create",
        "after_private_record",
        "after_private_move",
        "after_private_mark",
        "after_private_remove",
        "after_filesystem",
        "before_commit",
        "after_commit",
    ],
    "remove": [
        "after_journal",
        "after_quarantine",
        "after_private_create",
        "after_private_record",
        "after_private_move",
        "after_private_mark",
        "after_private_remove",
        "after_filesystem",
        "before_commit",
        "after_commit",
    ],
}
REPORT = []
# Native crash points after which frozen recovery acknowledges the operation
# but cannot know native's quarantine link or private directory: these inert
# entries remain, and native recovery (which never adopts unjournaled names)
# leaves them for the documented operator cleanup.
HANDOVER_RESIDUAL_STAGES = {
    ("replace", stage)
    for stage in (
        "after_publish",
        "after_private_create",
        "after_private_record",
        "after_private_move",
        "after_private_mark",
    )
} | {
    ("remove", stage)
    for stage in (
        "after_quarantine",
        "after_private_create",
        "after_private_record",
        "after_private_move",
        "after_private_mark",
    )
}


def tearDownModule():
    path = os.environ.get("CATABOLIC_HANDOVER_REPORT")
    if path:
        with open(path, "w", encoding="utf-8") as handle:
            json.dump(REPORT, handle, indent=1, sort_keys=True)
            handle.write("\n")


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class PublicationHandoverTest(unittest.TestCase):
    def setUp(self):
        reference.CatalogTest.setUp(self)
        self.original = hashlib.sha256(self.media.read_bytes()).hexdigest()

    def native(self, action, expected=0, **args):
        with self.store.detached():
            return native(self.database, "outputs", action, expected=expected, **args)

    def tree(self):
        entries = []
        for root, directories, files in os.walk(self.output):
            for name in sorted(directories + files):
                path = os.path.join(root, name)
                relative = os.path.relpath(path, self.output)
                kind = (
                    "link"
                    if os.path.islink(path)
                    else "dir"
                    if os.path.isdir(path)
                    else "file"
                )
                target = os.readlink(path) if kind == "link" else None
                entries.append((relative, kind, target))
        return sorted(entries)

    def residuals(self):
        # Private discard directories, quarantine/temporary links and staged
        # entries are dot-prefixed or `.private`; the ownership marker is not.
        return [
            entry
            for entry in self.tree()
            if any(
                part.startswith(".catabolic-") or part.endswith(".private")
                for part in entry[0].split(os.sep)
            )
            and os.path.basename(entry[0]) != ".catabolic-owner.json"
        ]

    def journal(self):
        return self.store.rows("SELECT * FROM journal ORDER BY rowid")

    def desire(self, kind):
        """Publish once, then change the desired state for `kind`."""
        if kind == "create":
            return
        self.native("apply")
        if kind == "replace":
            second = self.source / "second.mkv"
            second.write_bytes(b"replacement source")
            self.app.scan()
            file_id = self.store.rows("SELECT id FROM files WHERE path='second.mkv'")[
                0
            ]["id"]
            with self.store.transaction() as db:
                db.execute(
                    "UPDATE mappings SET file_id=? WHERE id=?",
                    (file_id, self.mapping["id"]),
                )
        else:
            self.app.disable_mapping(self.mapping["id"])

    def operator_step(self, error, record):
        """The documented operator action for an explicit block, taken only
        when its subject is the empty entry a crash left behind."""
        message = str(error)
        if "invalid output ownership marker" in message:
            marker = self.output / ".catabolic-owner.json"
            self.assertEqual(marker.read_bytes(), b"", record)
            marker.unlink()
        elif (
            "was not created by this operation" in message
            or "present after its removal began" in message
        ):
            (private,) = [
                path
                for path in self.output.rglob("*.private")
                if path.is_dir() and not path.is_symlink()
            ]
            self.assertEqual(list(private.iterdir()), [], record)
            private.rmdir()
        else:
            raise error
        record.setdefault("operator_steps", []).append(message)

    def inert_cleanup(self, record):
        """The documented operator cleanup of handover residuals: remove a
        reserved-name symlink, and a private directory that is empty or holds
        only its `entry` symlink. Anything else would fail this test."""
        for relative, kind, _ in sorted(self.residuals(), reverse=True):
            path = self.output / relative
            if kind == "link":
                path.unlink()
            elif kind == "dir" and path.name.endswith(".private"):
                children = list(path.iterdir())
                self.assertTrue(
                    all(c.name == "entry" and c.is_symlink() for c in children),
                    record,
                )
                for child in children:
                    child.unlink()
                path.rmdir()
            else:
                self.fail(record)

    def converge(self, record):
        """Native recovery and a fresh apply reach the healthy desired state,
        after any documented operator step for an explicit block."""
        for _ in range(3):
            try:
                self.native("recover")
                break
            except CatabolicError as error:
                self.operator_step(error, record)
        else:
            self.fail(record)
        self.native("apply")
        residual = self.residuals()
        if residual:
            # Only the known handover states may leave anything.
            self.assertIn(
                (record["kind"], record["stage"]), HANDOVER_RESIDUAL_STAGES, record
            )
            record["handover_residuals"] = residual
            self.inert_cleanup(record)
        self.assertTrue(self.reconciler.verify()["healthy"], record)
        self.assertEqual(self.journal(), [], record)
        self.assertEqual(self.residuals(), [], record)
        self.assertEqual(
            hashlib.sha256(self.media.read_bytes()).hexdigest(), self.original, record
        )

    def frozen_recover(self, record):
        before = (self.tree(), self.journal())
        try:
            result = self.reconciler.recover()
        except CatabolicError as error:
            record["frozen_recover"] = {"refused": str(error)}
            # A refusal is never destructive.
            self.assertEqual((self.tree(), self.journal()), before, record)
            return
        record["frozen_recover"] = {
            "recovered": len(result["recovered"]),
            "cancelled": len(result["cancelled"]),
        }
        record["residuals_after_frozen"] = self.residuals()

    def test_native_crash_states_recovered_by_frozen_python(self):
        for kind, stages in STAGES.items():
            for stage in stages:
                with self.subTest(kind=kind, stage=stage):
                    self.setUp()
                    self.desire(kind)
                    record = {
                        "direction": "native→frozen",
                        "kind": kind,
                        "stage": stage,
                    }
                    code = self.crash(stage)
                    if code == 0:
                        record["outcome"] = "stage not reached in this mode"
                        REPORT.append(record)
                        self.converge(record)
                        continue
                    self.assertEqual(code, 86, record)
                    record["journal_after_crash"] = len(self.journal())
                    record["residuals_after_crash"] = self.residuals()
                    self.frozen_recover(record)
                    self.converge(record)
                    REPORT.append(record)

    def crash(self, stage):
        with self.store.detached():
            run = subprocess.run(
                [
                    str(BINARY),
                    "--db",
                    str(self.database),
                    "outputs",
                    "apply",
                    "--options",
                    json.dumps(
                        {
                            "catalog": "global",
                            "disposable_fixture": True,
                            "failpoint": stage,
                            "crash": True,
                        }
                    ),
                ],
                capture_output=True,
                text=True,
                timeout=60,
            )
        return run.returncode

    def test_frozen_interruptions_recovered_natively(self):
        cases = []
        for kind in ("create", "replace", "remove"):
            cases.append((kind, "after effect"))
            cases.append((kind, "before effect"))
        failing = {
            "create": "catabolic.reconcile.os.symlink",
            "replace": "catabolic.reconcile.os.replace",
            "remove": "catabolic.reconcile.os.unlink",
        }
        for kind, moment in cases:
            with self.subTest(kind=kind, moment=moment):
                self.setUp()
                self.desire(kind)
                record = {"direction": "frozen→native", "kind": kind, "stage": moment}

                def interrupt(operation, operation_kind=kind):
                    if operation["kind"] == operation_kind:
                        raise RuntimeError("simulated process loss")

                if moment == "after effect":
                    with self.assertRaisesRegex(RuntimeError, "process loss"):
                        self.reconciler.apply(after_filesystem=interrupt)
                else:
                    with patch(failing[kind], side_effect=OSError("interrupted")):
                        try:
                            self.reconciler.apply()
                        except (OSError, CatabolicError) as error:
                            record["frozen_error"] = str(error)
                record["journal_after_interruption"] = len(self.journal())
                record["residuals_after_interruption"] = self.residuals()
                self.converge(record)
                REPORT.append(record)


if __name__ == "__main__":
    unittest.main()
