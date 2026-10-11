# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native hardlink safety with frozen workflow assertions and cross-engine recovery."""

import os
import unittest
from unittest.mock import patch

from catabolic.domain import CatabolicError
from catabolic.reconcile import Reconciler
from tests import test_hardlinks as reference
from tests.test_application import Interrupted
from tests.test_rust_migration import BINARY
from tests.test_rust_observation_filesystem import NativeReconciler, native


@unittest.skipUnless(BINARY.is_file(), "build catabolic-cli before qualification")
class NativeHardlinkTest(unittest.TestCase):
    retained = reference.HardlinkTest.retained
    disable = reference.HardlinkTest.disable
    fail_stage = reference.HardlinkTest.fail_stage

    def setUp(self):
        reference.HardlinkTest.setUp(self)
        self.reconciler = NativeReconciler(self)

    def test_exact_preview_and_verification_reports(self):
        expected = Reconciler(self.app).preview()
        before = self.database.read_bytes()
        actual = self.reconciler.preview()
        self.assertEqual(actual, expected)
        self.assertEqual(before, self.database.read_bytes())
        self.assertTrue(self.reconciler.apply()["healthy"])
        self.assertEqual(self.reconciler.verify(), Reconciler(self.app).verify())
        self.disable()
        self.assertEqual(self.reconciler.preview(), Reconciler(self.app).preview())
        self.assertTrue(self.reconciler.apply()["healthy"])
        self.assertEqual(self.reconciler.verify(), Reconciler(self.app).verify())

    def native_crash(self, stage):
        with self.store.detached():
            native(
                self.database,
                "outputs",
                "apply",
                expected=86,
                failpoint=stage,
                crash=True,
            )

    def test_native_created_link_survives_python_recovery(self):
        self.native_crash("after_link")
        inode = self.link.stat().st_ino
        self.assertTrue(self.store.rows("SELECT * FROM journal"))
        Reconciler(self.app).recover()
        self.assertEqual(inode, self.link.stat().st_ino)
        self.assertTrue(os.path.samefile(self.link, self.media))
        self.assertTrue(self.reconciler.verify()["healthy"])
        generation = self.store.rows("SELECT generation FROM publication_generations")[
            0
        ]
        self.reconciler.recover()
        self.assertEqual(
            self.store.rows("SELECT generation FROM publication_generations")[0],
            generation,
        )

    def test_native_retirement_preserves_final_reference_for_python_recovery(self):
        self.reconciler.apply()
        self.disable()
        self.native_crash("after_retention")
        self.media.unlink()
        Reconciler(self.app).recover()
        retained = self.output / self.retained()[0]["path"]
        self.assertEqual(retained.stat().st_nlink, 1)
        self.assertEqual(retained.read_bytes(), b"fixture media\n")
        self.assertFalse(self.link.exists())

    def test_python_created_link_survives_native_recovery(self):
        with (
            patch(
                "catabolic.hardlinks.checkpoint",
                side_effect=self.fail_stage("after_link"),
            ),
            self.assertRaises(Interrupted),
        ):
            Reconciler(self.app).apply()
        inode = self.link.stat().st_ino
        self.reconciler.recover()
        self.assertEqual(inode, self.link.stat().st_ino)
        self.assertTrue(self.reconciler.verify()["healthy"])

    def test_python_retirement_and_stale_replacement_recover_natively(self):
        self.reconciler.apply()
        os.link(self.media, self.source / "backup.mkv")
        self.media.unlink()
        self.media.write_bytes(b"second inode")
        self.app.scan()
        with (
            patch(
                "catabolic.hardlinks.checkpoint",
                side_effect=self.fail_stage("after_retention"),
            ),
            self.assertRaises(Interrupted),
        ):
            Reconciler(self.app).apply()
        self.media.unlink()
        self.media.write_bytes(b"third inode")
        self.app.scan()
        result = self.reconciler.recover()
        self.assertEqual(len(result["cancelled"]), 1)
        self.assertEqual(len(self.retained()), 1)
        self.assertTrue(self.reconciler.apply()["healthy"])
        self.assertEqual(self.link.read_bytes(), b"third inode")

    def test_staged_intent_rechecks_final_reference_and_can_cancel(self):
        self.reconciler.apply()
        self.disable()
        self.native_crash("after_journal")
        self.media.unlink()
        with self.assertRaisesRegex(CatabolicError, "last hardlink"):
            self.reconciler.recover()
        self.assertEqual(self.link.read_bytes(), b"fixture media\n")
        self.assertEqual(self.link.stat().st_nlink, 1)
        result = self.reconciler.recover(cancel_unapplied=True)
        self.assertEqual(len(result["cancelled"]), 1)
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])

    def test_native_crash_all_ack_boundaries_keeps_exactly_one_generation(self):
        # Each case uses a fresh fixture, so unchanged first-run success cannot
        # hide an unexecuted failpoint.
        for stage in ("after_filesystem", "before_commit", "after_commit"):
            with self.subTest(stage=stage):
                case = NativeHardlinkTest("runTest")
                case.setUp()
                try:
                    # Prepare ownership first; target failpoints then refer to
                    # the data-link journal rather than the empty-root marker.
                    from uuid import uuid4

                    from catabolic.filesystem import claim_output, root_handle

                    with root_handle(case.app.binding("output", "global")) as root:
                        claim_output(
                            root, Reconciler(case.app).owner("global"), str(uuid4())
                        )
                    case.native_crash(stage)
                    case.reconciler.recover()
                    self.assertTrue(os.path.samefile(case.media, case.link))
                    self.assertEqual(
                        case.store.rows(
                            "SELECT generation FROM publication_generations"
                        ),
                        [{"generation": 1}],
                    )
                    case.reconciler.recover()
                    self.assertEqual(
                        case.store.rows(
                            "SELECT generation FROM publication_generations"
                        ),
                        [{"generation": 1}],
                    )
                finally:
                    case.doCleanups()

    def test_native_preview_read_only_and_unowned_collision(self):
        before = self.database.read_bytes()
        self.assertTrue(self.reconciler.preview()["safe"])
        self.assertEqual(self.database.read_bytes(), before)
        self.assertEqual(list(self.output.iterdir()), [])
        self.link.parent.mkdir(parents=True)
        self.link.write_bytes(b"foreign final reference")
        self.assertFalse(self.reconciler.apply()["safe"])
        self.assertEqual(self.link.read_bytes(), b"foreign final reference")
        self.assertEqual(self.store.rows("SELECT * FROM journal"), [])

    def test_completed_historical_effect_does_not_verify_changed_desired_mapping(self):
        self.native_crash("after_link")
        other = self.source / "other.mkv"
        other.write_bytes(b"new desired bytes")
        self.app.scan()
        other_id = self.store.rows("SELECT id FROM files WHERE path='other.mkv'")[0][
            "id"
        ]
        # Simulate desired-state change after the historical effect; ordinary
        # mutation admission refuses this while recovery is pending.
        with self.store.transaction() as db:
            db.execute(
                "UPDATE mappings SET file_id=? WHERE id=?",
                (other_id, self.mapping["id"]),
            )
        self.reconciler.recover()
        self.assertTrue(os.path.samefile(self.link, self.media))
        self.assertFalse(self.reconciler.verify()["healthy"])
        self.assertEqual(
            self.store.rows("SELECT generation,verified FROM publication_generations"),
            [{"generation": 1, "verified": 0}],
        )
        self.assertTrue(self.reconciler.apply()["healthy"])
        self.assertTrue(os.path.samefile(self.link, other))
        self.assertEqual(
            self.store.rows("SELECT generation,verified FROM publication_generations"),
            [{"generation": 2, "verified": 2}],
        )
        self.assertEqual(len(self.retained()), 1)

    def test_changed_source_metadata_after_effect_requires_fresh_scan(self):
        self.native_crash("after_link")
        self.media.write_bytes(b"changed shared bytes")
        self.reconciler.recover()
        self.assertFalse(self.reconciler.verify()["healthy"])
        self.assertEqual(
            self.store.rows("SELECT verified FROM publication_generations"),
            [{"verified": 0}],
        )
        self.assertEqual(self.link.read_bytes(), b"changed shared bytes")
        self.app.scan()
        self.assertTrue(self.reconciler.apply()["healthy"])


for _name in (
    "test_creation_repeat_warnings_and_readonly_preview",
    "test_retirement_preserves_data_and_is_reported",
    "test_final_pointer_blocks_disabled_and_confirmed_missing_cleanup",
    "test_owned_inode_substitution_blocks_mutation",
    "test_symlink_substitution_and_retention_parent_tampering_are_blocked",
    "test_replacement_retains_old_inode",
    "test_replacement_of_last_old_reference_is_blocked",
    "test_link_mode_and_rebinding_guards_include_retained_data",
    "test_manifest_records_mode_and_inode_without_symlink_claims",
):
    setattr(NativeHardlinkTest, _name, getattr(reference.HardlinkTest, _name))
