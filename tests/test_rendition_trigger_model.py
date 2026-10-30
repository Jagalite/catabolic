# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""SQLite trigger behavior using the actual migration and a small synthetic schema.

This exercises committed trigger effects, not the native request adapter or real
catalog concurrency. The independent expected ledger tracks one physical job,
two caller demands, and one physical reservation.
"""
import itertools
import sqlite3
import tempfile
import unittest
from pathlib import Path

MIGRATION = Path(__file__).resolve().parents[1] / "crates/catabolic-store/resources/migrations/022_http_access.sql"


class RenditionTriggerModel(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(":memory:")
        # Unrelated parent tables are deliberately outside this trigger fixture.
        self.db.executescript("""
            PRAGMA foreign_keys=OFF;
            CREATE TABLE processing_jobs(id TEXT PRIMARY KEY, state TEXT NOT NULL);
            CREATE TABLE processing_artifacts(job_id TEXT, state TEXT NOT NULL);
            CREATE TABLE api_events(profile TEXT, resource_type TEXT, resource_id TEXT, state TEXT);
        """)
        self.db.executescript(MIGRATION.read_text())
        with self.db:
            self.db.execute("INSERT INTO processing_jobs VALUES ('job', 'queued')")
            self.db.execute("INSERT INTO api_reservations VALUES ('job', 1, 50)")
            for request in ('a', 'b'):
                self.db.execute("""INSERT INTO api_requests
                    (id,profile,principal,token_id,idempotency_key,body,job_id,state,reserved_bytes,reservation_device)
                    VALUES (?, 'profile', ?, 'token', ?, '{}', 'job', 'queued', 50, 1)""", (request, request, request))

    def tearDown(self):
        self.db.close()

    def ledger(self):
        demands = dict(self.db.execute("SELECT id,state FROM api_requests ORDER BY id"))
        reserved = self.db.execute("SELECT count(*) FROM api_reservations WHERE job_id='job'").fetchone()[0]
        amounts = dict(self.db.execute("SELECT id,reserved_bytes FROM api_requests ORDER BY id"))
        job = self.db.execute("SELECT state FROM processing_jobs WHERE id='job'").fetchone()[0]
        return job, demands, reserved, amounts

    def test_cancel_one_then_complete_and_repeat_completion(self):
        with self.db:
            self.db.execute("UPDATE processing_jobs SET state='running' WHERE id='job'")
            self.db.execute("UPDATE api_requests SET state='cancelled' WHERE id='a'")
        self.assertEqual(self.ledger(), ('running', {'a': 'cancelled', 'b': 'running'}, 1, {'a': 50, 'b': 50}))
        with self.db:
            self.db.execute("UPDATE processing_jobs SET state='complete' WHERE id='job'")
        expected = ('complete', {'a': 'cancelled', 'b': 'ready'}, 0, {'a': 0, 'b': 0})
        self.assertEqual(self.ledger(), expected)
        events = self.db.execute("SELECT count(*) FROM api_events").fetchone()[0]
        with self.db:
            self.db.execute("UPDATE processing_jobs SET state='complete' WHERE id='job'")
        self.assertEqual(self.ledger(), expected)
        self.assertEqual(self.db.execute("SELECT count(*) FROM api_events").fetchone()[0], events)

    def test_artifact_validation_preserves_cancelled_sibling(self):
        with self.db:
            self.db.execute("UPDATE processing_jobs SET state='running' WHERE id='job'")
            self.db.execute("UPDATE api_requests SET state='cancelled' WHERE id='a'")
            self.db.execute("INSERT INTO processing_artifacts VALUES ('job','pending')")
            self.db.execute("UPDATE processing_artifacts SET state='validating' WHERE job_id='job'")
        self.assertEqual(self.ledger(), ('running', {'a': 'cancelled', 'b': 'validating'}, 1, {'a': 50, 'b': 50}))

    def test_rollback_does_not_publish_completion_or_release(self):
        try:
            with self.db:
                self.db.execute("UPDATE processing_jobs SET state='complete' WHERE id='job'")
                raise RuntimeError('crash before commit')
        except RuntimeError:
            pass
        self.assertEqual(self.ledger(), ('queued', {'a': 'queued', 'b': 'queued'}, 1, {'a': 50, 'b': 50}))

    def test_bounded_committed_interleavings_match_independent_ledger(self):
        # Enumerate four committed actions, including repeated cancellation and
        # validation, without inventing transitions out of a terminal job.
        actions = ('cancel_a', 'cancel_b', 'running', 'validating', 'complete', 'failed')
        for sequence in itertools.product(actions, repeat=4):
            self.db.close()
            self.setUp()
            with self.db:
                self.db.execute("INSERT INTO processing_artifacts VALUES ('job','pending')")
            job = 'queued'
            demands = {'a': 'queued', 'b': 'queued'}
            reservation = 1
            amounts = {'a': 50, 'b': 50}
            for action in sequence:
                with self.db:
                    if action.startswith('cancel_'):
                        caller = action[-1]
                        if demands[caller] in ('queued', 'running', 'validating'):
                            self.db.execute("UPDATE api_requests SET state='cancelled' WHERE id=?", (caller,))
                            demands[caller] = 'cancelled'
                    elif action == 'validating' and job == 'running':
                        self.db.execute("UPDATE processing_artifacts SET state='validating' WHERE job_id='job'")
                        for caller in demands:
                            if demands[caller] == 'running':
                                demands[caller] = 'validating'
                    elif action == 'running' and job == 'queued':
                        self.db.execute("UPDATE processing_jobs SET state='running' WHERE id='job'")
                        job = 'running'
                        for caller in demands:
                            if demands[caller] != 'cancelled':
                                demands[caller] = 'running'
                    elif action in ('complete', 'failed') and job in ('queued', 'running'):
                        self.db.execute("UPDATE processing_jobs SET state=? WHERE id='job'", (action,))
                        job = action
                        reservation = 0
                        for caller in demands:
                            amounts[caller] = 0
                            if demands[caller] != 'cancelled':
                                demands[caller] = 'ready' if action == 'complete' else 'failed'
                self.assertEqual(self.ledger(), (job, demands, reservation, amounts), (sequence, action))

    def test_ledger_detects_missing_reservation_release(self):
        # Negative control: removing the real release trigger must disagree
        # with the independently expected terminal ledger.
        self.db.execute("DROP TRIGGER api_release_reservations")
        with self.db:
            self.db.execute("UPDATE processing_jobs SET state='complete' WHERE id='job'")
        expected = ('complete', {'a': 'ready', 'b': 'ready'}, 0, {'a': 0, 'b': 0})
        self.assertNotEqual(self.ledger(), expected)
        self.assertEqual(self.ledger()[2], 1)

    def test_reopened_database_observes_only_committed_completion(self):
        # Persist the synthetic fixture and inspect it through a separate
        # connection while the worker has an uncommitted completion.
        with tempfile.TemporaryDirectory() as directory:
            path = str(Path(directory) / "catalog.sqlite")
            writer = sqlite3.connect(path)
            self.db.backup(writer)
            writer.execute("UPDATE api_requests SET state='cancelled' WHERE id='a'")
            writer.commit()
            writer.execute("UPDATE processing_jobs SET state='complete' WHERE id='job'")
            reader = sqlite3.connect(path)
            self.assertEqual(reader.execute("SELECT state FROM processing_jobs WHERE id='job'").fetchone()[0], 'queued')
            self.assertEqual(reader.execute("SELECT count(*) FROM api_reservations").fetchone()[0], 1)
            self.assertEqual(dict(reader.execute("SELECT id,state FROM api_requests")), {'a': 'cancelled', 'b': 'queued'})
            writer.rollback()
            reader.close()
            writer.close()

            writer = sqlite3.connect(path)
            self.assertEqual(writer.execute("SELECT count(*) FROM api_reservations").fetchone()[0], 1)
            writer.execute("UPDATE processing_jobs SET state='complete' WHERE id='job'")
            writer.commit()
            writer.close()
            reader = sqlite3.connect(path)
            self.assertEqual(reader.execute("SELECT state FROM processing_jobs WHERE id='job'").fetchone()[0], 'complete')
            self.assertEqual(reader.execute("SELECT count(*) FROM api_reservations").fetchone()[0], 0)
            self.assertEqual(dict(reader.execute("SELECT id,state FROM api_requests")), {'a': 'cancelled', 'b': 'ready'})
            events = reader.execute("SELECT count(*) FROM api_events").fetchone()[0]
            reader.execute("UPDATE processing_jobs SET state='complete' WHERE id='job'")
            reader.commit()
            self.assertEqual(reader.execute("SELECT count(*) FROM api_events").fetchone()[0], events)
            reader.close()

    def test_terminal_outcomes_clear_physical_and_caller_reservations(self):
        for phase, demand in [('failed','failed'), ('timeout','failed'), ('changed','stale'), ('cancelled','cancelled')]:
            with self.subTest(phase=phase):
                self.db.close()
                self.setUp()
                with self.db:
                    self.db.execute("UPDATE processing_jobs SET state=? WHERE id='job'", (phase,))
                self.assertEqual(self.ledger(), (phase, {'a': demand, 'b': demand}, 0, {'a': 0, 'b': 0}))


if __name__ == '__main__':
    unittest.main()
