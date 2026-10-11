# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Hermetic qualification retry checks; never contact a server or run a journey."""

import ast
import json
import subprocess
import tempfile
import types
import unittest
import urllib.error
import urllib.request
from email.message import Message
from pathlib import Path

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

                def timeout(command, *, binary=binary, **kwargs):
                    raise subprocess.TimeoutExpired(
                        command,
                        kwargs["timeout"],
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


if __name__ == "__main__":
    unittest.main()
