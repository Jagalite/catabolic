# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import unittest
from unittest.mock import patch

from catabolic import rendering
from catabolic.app import Application
from catabolic.artifacts import Artifacts
from catabolic.processing import Processing
from catabolic.store import Store
from tests.test_artifacts import ArtifactTest


class ExecutionClaimsTest(unittest.TestCase):
    setUpClass = classmethod(ArtifactTest.setUpClass.__func__)
    tearDownClass = classmethod(ArtifactTest.tearDownClass.__func__)
    setUp = ArtifactTest.setUp
    enqueue = ArtifactTest.enqueue

    def test_render_releases_writer_and_recovery_preserves_live_work(self):
        self.enqueue("thumbnail")
        render = rendering.render

        def checked(*args):
            with Store(self.path, writable=True) as store:
                app = Application(store)
                app.put_item(
                    "movie", {"fixture": "other-writer"}, {"title": "Concurrent write"}
                )
                self.assertEqual(Artifacts(app).recover()["recovered"], [])
            return render(*args)

        with patch.object(rendering, "render", side_effect=checked):
            result = self.a.run()
        self.assertTrue(result["complete"], result)

    def test_cancel_during_render_cannot_publish(self):
        _, queued = self.enqueue("thumbnail")
        job = queued["job_id"]
        render = rendering.render

        def cancelled(*args):
            with Store(self.path, writable=True) as store:
                self.assertTrue(Processing(Application(store)).cancel(job)["cancelled"])
            return render(*args)

        with patch.object(rendering, "render", side_effect=cancelled):
            result = self.a.run()
        self.assertFalse(result["complete"])
        self.a.recover()
        self.assertEqual(
            self.store.rows("SELECT state FROM processing_jobs WHERE id=?", (job,))[0][
                "state"
            ],
            "cancelled",
        )
        self.assertEqual(
            self.store.rows("SELECT id FROM processing_artifacts WHERE state='ready'"),
            [],
        )

    def test_expired_worker_cannot_publish(self):
        self.enqueue("thumbnail")
        render = rendering.render

        def expired(*args):
            with Store(self.path, writable=True) as store:
                with store.transaction() as db:
                    db.execute(
                        "UPDATE execution_claims SET lease_until=0,generation=generation+1"
                    )
            return render(*args)

        with patch.object(rendering, "render", side_effect=expired):
            result = self.a.run()
        self.assertFalse(result["complete"])
        self.assertEqual(
            self.store.rows("SELECT id FROM processing_artifacts WHERE state='ready'"),
            [],
        )


del ArtifactTest
