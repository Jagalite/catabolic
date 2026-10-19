# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Hermetic qualification retry checks; never contact a server or run a journey."""

import ast
import fcntl
import json
import os
import runpy
import signal
import subprocess
import tempfile
import types
import unittest
import urllib.error
import urllib.request
from email.message import Message
from pathlib import Path
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
BUSY = {"code": "catalog_busy", "retryable": True}


class Response:
    def __init__(self, status, value, retry_after="0.01"):
        self.status = status
        self.value = value
        self.headers = Message()
        self.headers["Content-Type"] = "application/problem+json"
        self.headers["Retry-After"] = retry_after

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False

    def read(self):
        return json.dumps(self.value).encode()


class QualificationRetryFixtureTest(unittest.TestCase):
    def python_request(self, status=503, problem=None, retry_after="0.01"):
        # Extract only the request closure. Fixture creation, subprocess work,
        # the main journey and persisted logs are never executed or inspected.
        source = ast.parse((ROOT / "scripts/qualify_native_http.py").read_text())
        main = next(
            node
            for node in source.body
            if isinstance(node, ast.FunctionDef) and node.name == "main"
        )
        request = next(
            node
            for node in main.body
            if isinstance(node, ast.FunctionDef) and node.name == "request"
        )
        for index, node in enumerate(request.body):
            if isinstance(node, ast.Nonlocal):
                request.body[index] = ast.copy_location(
                    ast.Global(names=node.names), node
                )
        module = ast.fix_missing_locations(ast.Module(body=[request], type_ignores=[]))
        calls, delays = [], []
        clock = [0.0]

        def fetch(req, timeout):
            calls.append((req.get_method(), req.data, dict(req.header_items())))
            return (
                Response(status, BUSY if problem is None else problem, retry_after)
                if len(calls) == 1
                else Response(200, {"ok": True})
            )

        def sleep(seconds):
            delays.append(seconds)
            clock[0] += seconds

        state = {
            "commands": 0,
            "busy_retries": 0,
            "busy_attempts": [],
            "base": "http://127.0.0.1:12345",
            "token": "PRIVATE-AUTH-TOKEN",
            "json": json,
            "urllib": types.SimpleNamespace(
                request=types.SimpleNamespace(
                    Request=urllib.request.Request, urlopen=fetch
                ),
                error=urllib.error,
            ),
            "time": types.SimpleNamespace(monotonic=lambda: clock[0], sleep=sleep),
        }
        exec(compile(module, "<isolated qualification request>", "exec"), state)
        return state, calls, delays

    def test_python_retries_only_replay_safe_requests(self):
        for path, body, headers, safe in (
            ("/v1/items", None, {}, True),
            ("/v1/query/graphql", {"query": "{profile}"}, {}, True),
            ("/v1/query/sql", {"query": "SELECT 1"}, {}, True),
            ("/v1/queries/revision/runs", {}, {}, True),
            (
                "/v1/rendition-requests",
                {"item_id": "item"},
                {"Idempotency-Key": "same-key"},
                True,
            ),
            ("/v1/rendition-requests", {}, {}, False),
            ("/v1/content-access", {}, {}, False),
            ("/v1/items/snapshots", {}, {}, False),
            ("/v1/content-access/ticket/revoke", {}, {}, False),
            ("/v1/rendition-requests/id/cancel", {}, {}, False),
            ("/v1/rendition-requests/id/retry", {}, {}, False),
            ("/v1/files/id/content?ticket=PRIVATE-TICKET", None, {}, False),
            ("/v1/playback-sessions/id/segments/0.ts", None, {}, False),
        ):
            with self.subTest(path=path):
                state, calls, delays = self.python_request()
                code, _ = state["request"](path, body, headers=headers)
                self.assertEqual(code, 200 if safe else 503)
                self.assertEqual(len(calls), 2 if safe else 1)
                self.assertEqual(len(delays), int(safe))
                self.assertEqual(state["busy_attempts"][0]["retried"], safe)
                self.assertNotIn("PRIVATE-", json.dumps(state["busy_attempts"]))
                if safe:
                    self.assertEqual(calls[0], calls[1])

    def test_python_nonbusy_and_nonstrict_retryable_are_not_replayed(self):
        for status, problem in (
            (500, BUSY),
            (503, {"code": "other", "retryable": True}),
            (503, {"code": "catalog_busy", "retryable": False}),
            (503, {"code": "catalog_busy", "retryable": "true"}),
        ):
            with self.subTest(status=status, problem=problem):
                state, calls, _ = self.python_request(status, problem)
                self.assertEqual(state["request"]("/v1/items")[0], status)
                self.assertEqual(len(calls), 1)

    def test_python_malformed_retry_after_is_bounded(self):
        for value in ("invalid", "NaN", "Infinity", "-1", "99999", "0"):
            with self.subTest(value=value):
                state, calls, delays = self.python_request(retry_after=value)
                self.assertEqual(state["request"]("/v1/items")[0], 200)
                self.assertEqual(len(calls), 2)
                self.assertTrue(0.01 <= delays[0] <= 1)

    def test_generated_timeout_preserves_partial_busy_evidence(self):
        source = ast.parse((ROOT / "scripts/qualify_native_http.py").read_text())
        block = next(
            node
            for node in ast.walk(source)
            if isinstance(node, ast.If)
            and isinstance(node.test, ast.Attribute)
            and isinstance(node.test.value, ast.Name)
            and node.test.value.id == "args"
            and node.test.attr == "generated_client"
        )
        module = ast.fix_missing_locations(ast.Module(body=[block], type_ignores=[]))
        event = {
            "type": "catalog_busy_attempt",
            "method": "POST",
            "path": "/v1/items/snapshots",
            "status": 503,
            "attempt": 1,
            "retried": False,
        }
        partial = "ordinary client output\n" + json.dumps(event) + '\n{"type":'
        for binary in (False, True):
            with (
                self.subTest(binary=binary),
                tempfile.TemporaryDirectory() as temporary,
            ):
                root = Path(temporary)

                def timeout(*arguments, binary=binary):
                    raise subprocess.TimeoutExpired(
                        ["node", "client.js"],
                        30,
                        output=partial.encode() if binary else partial,
                        stderr=b"fixture error\xff" if binary else "fixture error",
                    )

                state = {
                    "args": types.SimpleNamespace(generated_client=Path("client.js")),
                    "root": root,
                    "base": "http://127.0.0.1:12345",
                    "token": "PRIVATE-AUTH-TOKEN",
                    "state": {},
                    "query_id": "query",
                    "env": {},
                    "commands": 12,
                    "busy_attempts": [{"method": "GET", "retried": True}],
                    "generated_busy_attempts": [],
                    "generated_phases": [],
                    "isolate_worker": Mock(),
                    "run_generated_client": timeout,
                    "worker_isolation": {"reaped": True},
                    "contention_check": {"complete": True},
                    "json": json,
                    "subprocess": types.SimpleNamespace(
                        run=timeout,
                        TimeoutExpired=subprocess.TimeoutExpired,
                        CompletedProcess=subprocess.CompletedProcess,
                    ),
                }
                with self.assertRaisesRegex(RuntimeError, "generated client"):
                    exec(compile(module, "<isolated client timeout>", "exec"), state)
                report = json.loads((root / "report.json").read_text())
                self.assertFalse(report["complete"])
                self.assertEqual(report["error"], "generated_client_timed_out")
                self.assertEqual(report["generated_client_busy_attempts"], [event])
                self.assertEqual(
                    report["catalog_busy_attempts"], state["busy_attempts"]
                )
                self.assertEqual(
                    (root / "generated-client.stdout").read_text(), partial
                )
                self.assertTrue(
                    (root / "generated-client.stderr")
                    .read_text()
                    .startswith("fixture error")
                )
                self.assertNotIn("PRIVATE-AUTH-TOKEN", json.dumps(report))

    def test_generated_client_retry_policy_and_identical_request_replay(self):
        helper = ROOT / "tests/clients/v1_7/build/busy.js"
        self.assertTrue(helper.is_file(), "compile the 1.7 client first")
        script = r"""
import assert from 'node:assert/strict';
const { busyFetch, replaySafe } = await import(process.argv[1]);
for (const [method,path,key,safe] of [
 ['GET','/v1/items',null,true],['HEAD','/v1/me',null,true],
 ['POST','/v1/query/graphql',null,true],['POST','/v1/query/sql',null,true],
 ['POST','/v1/queries/revision/runs',null,true],
 ['POST','/v1/rendition-requests','same-key',true],
 ['POST','/v1/rendition-requests',null,false],
 ['POST','/v1/content-access',null,false],['POST','/v1/items/snapshots',null,false],
 ['POST','/v1/content-access/id/revoke',null,false],
 ['POST','/v1/rendition-requests/id/retry',null,false],
 ['POST','/v1/rendition-requests/id/cancel',null,false],
 ['GET','/v1/files/id/content?ticket=PRIVATE-TICKET',null,false],
 ['HEAD','/v1/files/id/content?ticket=PRIVATE-TICKET',null,false],
 ['GET','/v1/playback-sessions/id/segments/0.ts',null,false],
 ['GET','/v1/items?%74icket=PRIVATE-TICKET',null,false],
]) {
 const events=[], calls=[], delays=[];
 const request=new Request('http://127.0.0.1'+path,{method,
   headers:{Authorization:'Bearer PRIVATE-AUTH',...(key?{'Idempotency-Key':key}:{})},
   ...(method==='POST'?{body:'{"unchanged":true}'}:{})});
 assert.equal(replaySafe(request),safe,path);
 const fetcher=async req=>{calls.push([req.method,await req.text(),[...req.headers]]);
   return new Response(JSON.stringify(calls.length===1?{code:'catalog_busy',retryable:true}:{ok:true}),
     {status:calls.length===1?503:200,headers:{'Retry-After':'invalid'}});};
 const response=await busyFetch(e=>events.push(e),fetcher,async ms=>{delays.push(ms)})(request);
 assert.equal(response.status,safe?200:503,path); assert.equal(calls.length,safe?2:1,path);
 assert.equal(events.length,1,path);assert.equal(events[0].retried,safe,path);
 assert.ok(!JSON.stringify(events).includes('PRIVATE-'),path);
 if(safe){assert.deepEqual(calls[0],calls[1]);assert.deepEqual(delays,[1000]);}
}
for (const [status,problem] of [[500,{code:'catalog_busy',retryable:true}],
 [503,{code:'other',retryable:true}],[503,{code:'catalog_busy',retryable:false}],
 [503,{code:'catalog_busy',retryable:'true'}]]){
 let calls=0;
 const response=await busyFetch(()=>{},async()=>{calls++;return new Response(JSON.stringify(problem),{status})},async()=>{})('http://127.0.0.1/v1/items');
 assert.equal(response.status,status);assert.equal(calls,1);
}
console.log('mock retry checks passed');
"""
        result = subprocess.run(
            ["node", "--input-type=module", "-e", script, helper.as_uri()],
            capture_output=True,
            text=True,
            timeout=15,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


class QualificationIsolationFixtureTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.qualifier = runpy.run_path(str(ROOT / "scripts/qualify_native_http.py"))

    def test_worker_is_reaped_before_isolation_receipt(self):
        worker = Mock(pid=123, **{"poll.return_value": None, "wait.return_value": 0})
        receipt = self.qualifier["stop_owned_worker"](worker)
        worker.terminate.assert_called_once_with()
        worker.wait.assert_called_once_with(timeout=10)
        self.assertTrue(receipt["reaped"])
        self.assertEqual(receipt["returncode"], 0)

    def test_dead_worker_cannot_claim_isolation_success(self):
        worker = Mock(**{"poll.return_value": 1})
        with self.assertRaisesRegex(RuntimeError, "exited before"):
            self.qualifier["stop_owned_worker"](worker)
        worker.terminate.assert_not_called()
        worker.wait.assert_not_called()

    def test_shutdown_failure_or_timeout_cannot_claim_isolation_success(self):
        for code in (1, -signal.SIGTERM, 128 + signal.SIGTERM):
            worker = Mock(**{"poll.return_value": None, "wait.return_value": code})
            with (
                self.subTest(code=code),
                self.assertRaisesRegex(RuntimeError, "shutdown failed"),
            ):
                self.qualifier["stop_owned_worker"](worker)
        worker.wait.side_effect = subprocess.TimeoutExpired("owned-worker", 10)
        with self.assertRaises(subprocess.TimeoutExpired):
            self.qualifier["stop_owned_worker"](worker)

    def test_separate_contention_is_held_for_one_sentinel_request_only(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lock_path = root / "catalog.sqlite3.lock"
            lock_path.touch()
            calls = []

            def request(path, body):
                calls.append((path, body))
                with lock_path.open("r+b") as competing:
                    with self.assertRaises(BlockingIOError):
                        fcntl.flock(competing, fcntl.LOCK_EX | fcntl.LOCK_NB)
                return 503, BUSY

            receipt = self.qualifier["check_writer_contention"](root, request)
            self.assertTrue(receipt["complete"])
            self.assertEqual(
                calls,
                [
                    (
                        "/v1/content-access",
                        {
                            "file_id": "qualification-contended-admission",
                            "revision": "never-replayed",
                        },
                    )
                ],
            )
            with lock_path.open("r+b") as released:
                fcntl.flock(released, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_contention_failures_are_not_replayed_and_release_lock(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lock_path = root / "catalog.sqlite3.lock"
            lock_path.touch()
            for response in (
                (200, {}),
                (503, {"code": "other"}),
                (503, {"code": "catalog_busy", "retryable": False}),
            ):
                with self.subTest(response=response):
                    request = Mock(return_value=response)
                    with self.assertRaises(AssertionError):
                        self.qualifier["check_writer_contention"](root, request)
                    request.assert_called_once()
                    with lock_path.open("r+b") as released:
                        fcntl.flock(released, fcntl.LOCK_EX | fcntl.LOCK_NB)
            request = Mock(side_effect=OSError("fixture failure"))
            with self.assertRaises(OSError):
                self.qualifier["check_writer_contention"](root, request)
            request.assert_called_once()
            with lock_path.open("r+b") as released:
                fcntl.flock(released, fcntl.LOCK_EX | fcntl.LOCK_NB)


class QualificationClientPhaseTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.qualifier = runpy.run_path(str(ROOT / "scripts/qualify_native_http.py"))

    def run_client(self, root, program, isolate, phases, timeout=5):
        client = root / "client.mjs"
        client.write_text(
            "import {readFileSync,readSync,writeFileSync} from 'node:fs';\n"
            "const fixture=JSON.parse(readFileSync(0,'utf8'));\n"
            "const mark=phase=>console.log(JSON.stringify({type:'qualification_phase',phase}));\n"
            "const release=()=>{const b=Buffer.alloc(1); const n=readSync(fixture.phase_fd,b,0,1,null);"
            "if(n===1&&b.toString()==='1')writeFileSync('released','yes');else process.exit(9)};\n"
            + program
        )
        children = []
        descriptors = []
        self.release_writes = []
        popen = subprocess.Popen
        pipe, write = os.pipe, os.write

        def create_pipe():
            pair = pipe()
            descriptors.extend(pair)
            return pair

        def send(descriptor, data):
            self.release_writes.append((descriptor, data))
            return write(descriptor, data)

        def start(*args, **kwargs):
            child = popen(*args, **kwargs)
            children.append(child)
            return child

        try:
            with (
                patch("subprocess.Popen", side_effect=start),
                patch("os.pipe", side_effect=create_pipe),
                patch("os.write", side_effect=send),
            ):
                return self.qualifier["run_generated_client"](
                    client,
                    {"token": "PRIVATE-AUTH"},
                    root,
                    None,
                    isolate,
                    phases,
                    timeout,
                )
        finally:
            self.assertEqual(len(children), 1)
            self.assertIsNotNone(children[0].poll(), "client process was not reaped")
            self.assertTrue(children[0].stdin.closed)
            for descriptor in descriptors:
                with self.assertRaises(OSError):
                    os.fstat(descriptor)

    def test_mutations_wait_for_acknowledged_reap(self):
        with tempfile.TemporaryDirectory() as temporary:
            root, phases = Path(temporary), []

            def isolate():
                self.assertEqual(phases, ["processing_ready"])
                self.assertFalse((root / "released").exists())
                return {"reaped": True}

            result = self.run_client(
                root,
                "mark('processing_ready');release();mark('isolated_complete');",
                isolate,
                phases,
            )
            self.assertEqual(result.returncode, 0)
            self.assertTrue((root / "released").exists())
            self.assertEqual(phases, ["processing_ready", "isolated_complete"])
            self.assertEqual([value for _, value in self.release_writes], [b"1"])
            self.assertNotIn("PRIVATE-AUTH", result.stdout + result.stderr)

    def test_failed_shutdown_never_sends_release(self):
        for isolate in (
            Mock(side_effect=RuntimeError("shutdown failed")),
            Mock(return_value={"reaped": False}),
        ):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                with self.assertRaises(RuntimeError):
                    self.run_client(
                        root, "mark('processing_ready');release();", isolate, []
                    )
                self.assertFalse((root / "released").exists())
                self.assertEqual(self.release_writes, [])
                isolate.assert_called_once_with()

    def test_missing_duplicate_and_out_of_order_markers_fail(self):
        for program in (
            "console.log('no phases');",
            "mark('isolated_complete');",
            "mark('processing_ready');release();mark('processing_ready');",
            "mark('processing_ready');release();mark('isolated_complete');mark('isolated_complete');",
        ):
            with (
                self.subTest(program=program),
                tempfile.TemporaryDirectory() as temporary,
            ):
                with self.assertRaisesRegex(RuntimeError, "phase"):
                    self.run_client(
                        Path(temporary),
                        program,
                        Mock(return_value={"reaped": True}),
                        [],
                    )

    def test_premature_failure_does_not_isolate_worker(self):
        with tempfile.TemporaryDirectory() as temporary:
            isolate = Mock()
            result = self.run_client(Path(temporary), "process.exit(3);", isolate, [])
            self.assertEqual(result.returncode, 3)
            isolate.assert_not_called()

    def test_timeout_preserves_partial_busy_output_and_reaps_client(self):
        with tempfile.TemporaryDirectory() as temporary:
            root, isolate = Path(temporary), Mock()
            program = "console.log(JSON.stringify({type:'catalog_busy_attempt',retried:false}));process.stdout.write('partial');setInterval(()=>{},1000);"
            with self.assertRaises(subprocess.TimeoutExpired) as raised:
                self.run_client(root, program, isolate, [], timeout=1)
            isolate.assert_not_called()
            self.assertIn('"retried":false', raised.exception.stdout)
            self.assertTrue(raised.exception.stdout.endswith("partial"))
            self.assertEqual(
                (root / "generated-client.stdout").read_text(), raised.exception.stdout
            )

    def test_timeout_after_processing_reap_still_does_not_release(self):
        import time

        with tempfile.TemporaryDirectory() as temporary:
            root, phases = Path(temporary), []

            def slow_shutdown():
                time.sleep(1)
                return {"reaped": True}

            with self.assertRaises(subprocess.TimeoutExpired):
                self.run_client(
                    root,
                    "mark('processing_ready');release();",
                    slow_shutdown,
                    phases,
                    timeout=1,
                )
            self.assertEqual(phases, ["processing_ready"])
            self.assertEqual(self.release_writes, [])
            self.assertFalse((root / "released").exists())


if __name__ == "__main__":
    unittest.main()
