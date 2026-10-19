# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Selected SIGTERM boundary controls, using disposable numeric-loopback work.

The historical frozen blocked-HTTP TERM witness is retained in the external-rule
receipt. These native controls require cleanup rather than reproducing its
surviving helper. The callable protocol remains an independently owned boundary.
"""

import json
import signal
import sqlite3
import subprocess
import threading
import unittest
from contextlib import closing

from tests import process_tree
from tests import test_rust_legacy_external_rule as external
from tests.test_native_cli_callable_contract import load


class LegacySigtermContract(unittest.TestCase):
    def fixture(self, *, receipts=False):
        fixture = external.NativeLegacyExternalRuleTest()
        if receipts:
            # The existing seed contains tiny files; this control accepts only
            # metadata pointing to one of them, never physical output creation.
            def clean():
                fixture.assertEqual(
                    {p.name: p.read_bytes() for p in fixture.source.iterdir()},
                    fixture.source_before,
                )
                fixture.assertEqual(fixture.server.errors, [])
                for flag in (True, False):
                    state = fixture.snapshot(flag)
                    for table in ("processing_artifacts", "journal"):
                        fixture.assertEqual(state[table], [], table)
                    fixture.assert_writer_available(flag)
                    with closing(sqlite3.connect(fixture.db[flag])) as db:
                        fixture.assertEqual(
                            db.execute("PRAGMA foreign_key_check").fetchall(), []
                        )
                        fixture.assertEqual(
                            db.execute("PRAGMA integrity_check").fetchall(), [("ok",)]
                        )

            fixture.assert_fixture_clean = clean
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        return fixture

    def assert_reaped(self, descendants):
        self.assertTrue(descendants, "no supervised helper was observed")
        self.assertEqual(process_tree.present(descendants), [])

    def terminate(self, fixture, child):
        descendants = fixture.descendants(child.pid)
        child.send_signal(signal.SIGTERM)
        stdout, stderr = child.communicate(timeout=15)
        self.assertEqual((child.returncode, stdout, stderr), (-signal.SIGTERM, "", ""))
        self.assert_reaped(descendants)
        fixture.assert_writer_available(False)

    def test_rule_sigterm_has_no_error_envelope_and_reaps_helper_all_modes(self):
        for mode in (None, "--json", "--machine"):
            with self.subTest(mode=mode):
                fixture = self.fixture()
                before = fixture.snapshot(False)
                args = fixture.run_args(mode="--json")
                args = ([mode] if mode else []) + args[1:]
                child, gate = fixture.start_blocked(False, args)
                fixture.assert_writer_available(False)
                original_request = list(fixture.server.requests)
                self.terminate(fixture, child)
                after = fixture.snapshot(False)
                for table in before.keys() - external.MUTABLE_WORKER_TABLES:
                    self.assertEqual(after[table], before[table], table)
                job = next(
                    row
                    for row in after["processor_jobs"]
                    if row["id"] == external.JOB_IDS[0]
                )
                self.assertEqual((job["state"], job["generation"]), ("leased", 1))
                self.assertEqual(after["processor_attempts"][0]["state"], "leased")
                gate.release.set()
                # A reply was never accepted, so the same leased generation
                # resends its original PUT, rather than polling a submitted job.
                fixture.server.reset([{"state": "running"}])
                result = fixture.run_cli(False, fixture.run_args())
                self.assertEqual(result.returncode, 3, result)
                self.assertEqual(
                    fixture.server.requests,
                    original_request,
                )

    @staticmethod
    def receipt(fixture, body):
        receipt = {
            key: body[key]
            for key in (
                "database_id",
                "profile",
                "producer",
                "instance",
                "job_id",
                "attempt",
                "configuration_digest",
                "source",
            )
        }
        receipt.update(
            version=1,
            tools={"fixture": "1"},
            outcome="complete",
            outputs=[
                {
                    "location": "fixture",
                    "path": "3.txt",
                    "definition_id": fixture.ids["output"],
                    "size": len(fixture.source_before["3.txt"]),
                }
            ],
        )
        return receipt

    def test_rule_sigterm_before_valid_receipt_response_keeps_lease(self):
        fixture = self.fixture()
        child, gate = fixture.start_blocked(False, fixture.run_args(mode="--machine"))
        gate.result = {
            "state": "complete",
            "receipt": self.receipt(fixture, fixture.server.requests[-1][2]),
        }
        before = fixture.snapshot(False)
        self.terminate(fixture, child)
        self.assertEqual(fixture.snapshot(False), before)
        for table in ("external_receipts", "receipt_outputs", "media_outputs"):
            self.assertEqual(before[table], [])
        gate.release.set()

    def test_rule_sigterm_retains_receipt_committed_before_next_network_step(self):
        fixture = self.fixture(receipts=True)
        child, first = fixture.start_blocked(
            False, fixture.run_args(batch=2, mode="--machine")
        )
        receipt = self.receipt(fixture, fixture.server.requests[-1][2])
        first.result = {"state": "complete", "receipt": receipt}
        second = external.Gate()
        fixture.server.responses.append(second)
        fixture.server.gates.append(second)
        first.release.set()
        self.assertTrue(
            second.entered.wait(15), "second job did not reach its network wait"
        )
        before = fixture.snapshot(False)
        jobs = {row["id"]: row for row in before["processor_jobs"]}
        self.assertEqual(
            [jobs[i]["state"] for i in external.JOB_IDS[:3]],
            ["complete", "leased", "queued"],
        )
        self.assertEqual(len(before["external_receipts"]), 1)
        self.assertEqual(len(before["receipt_outputs"]), 1)
        self.assertEqual(len(before["media_outputs"]), 1)
        self.terminate(fixture, child)
        self.assertEqual(
            fixture.snapshot(False),
            before,
            "termination changed an already durable outcome",
        )
        second.release.set()

    def test_callable_sigterm_retains_acknowledged_130_result_and_reaps_helper(self):
        fixture = self.fixture()
        protocol = load()
        gate = external.Gate()
        fixture.server.reset([gate])
        child = subprocess.Popen(
            [str(external.BINARY), "__catabolic-python-cli-v1"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=fixture.env,
        )
        self.addCleanup(fixture.retire, child, gate)
        watchdog = threading.Timer(
            20, lambda: child.kill() if child.poll() is None else None
        )
        self.addCleanup(watchdog.cancel)
        watchdog.start()
        protocol._send(
            child.stdin,
            {
                "version": 1,
                "kind": "argv",
                "argv": [
                    "--db",
                    str(fixture.db[False]),
                    *fixture.run_args(mode="--machine"),
                ],
            },
        )
        frame = protocol._receive(child.stdout)
        self.assertEqual(frame["kind"], "dispatch")
        expected = frame["interrupted"]
        protocol._send(child.stdin, {"kind": "ack"})
        self.assertTrue(gate.entered.wait(15))
        descendants = fixture.descendants(child.pid)
        child.send_signal(signal.SIGTERM)
        output = {"stdout": "", "stderr": ""}
        while True:
            frame = protocol._receive(child.stdout)
            if frame["kind"] == "return":
                self.assertEqual(frame["code"], 130)
                break
            if frame["kind"] == "write":
                output[frame["stream"]] += frame["text"]
            else:
                self.assertIn(frame["kind"], ("handling-error", "flush"))
            protocol._send(child.stdin, {"kind": "ack"})
        self.assertEqual(child.wait(timeout=15), 0)
        watchdog.cancel()
        self.assertEqual(child.stderr.read(), b"")
        self.assertEqual(output, {key: expected[key] for key in output})
        self.assertEqual(
            json.loads(output["stdout"])["data"]["error"]["type"], "KeyboardInterrupt"
        )
        self.assert_reaped(descendants)
        fixture.assert_writer_available(False)
        gate.release.set()


if __name__ == "__main__":
    unittest.main()
