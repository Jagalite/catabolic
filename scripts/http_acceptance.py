# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Installed HTTP/worker/client journey using only disposable local media."""

import argparse
import json
import os
import socket
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path

BOOTSTRAP = r"""
import json,os,sys,time
from pathlib import Path
from catabolic.store import Store
from catabolic.app import Application
from catabolic.artifacts import Artifacts
from catabolic.access import principal_put,grant_put,issue
from catabolic.api_cli import save_secret
from catabolic.content_access import revision_of
from catabolic.saved_queries import Queries
root=Path(sys.argv[1])
database=root/'catalog.sqlite3'
Store.initialize(database)
with Store(database,writable=True) as store:
 app=Application(store)
 app.bind('source','originals',str(root/'originals'))
 app.scan()
 file=store.rows('SELECT id FROM files')[0]['id']
 item=app.put_item('movie',{'fixture':'http-acceptance'},{'title':'Acceptance fixture'})['id']
 app.media.associate(file,item)
 artifacts=Artifacts(app)
 artifacts.bind('browser',root/'browser')
 recipe=artifacts.recipe('browser-thumbnail','thumbnail',{'max_output_bytes':16777216})
 with store.transaction() as db:
  db.execute("INSERT INTO api_operations(id,profile,recipe_id,location) VALUES ('thumbnail','default',?,'browser')",(recipe['id'],))
  db.execute("INSERT INTO api_sources VALUES ('default','browser')")
 principal_put(store,'default','website')
 query=Queries(store).put('website-items',{'mode':'selection','selection':{'language':'sql','query':'SELECT id AS item_id FROM items WHERE id=:item','params':{'item':item}}})
 g=grant_put(store,'website',{'actions':['metadata:read','processing:request','events:read'],'item_ids':[item],'file_ids':[file],'operation_ids':['thumbnail'],'report_ids':[query['id']]})
 derivative=grant_put(store,'website',{'actions':['metadata:read','content:read'],'item_ids':[item],'derivatives':True})
 save_secret(store,root/'credential.json',issue(store,'website',[g['grant_id'],derivative['grant_id']]))
 state={'item_id':item,'source_file_id':file,'source_revision':revision_of(store,'default',file),'operation_id':'thumbnail','query_id':query['id']}
 (root/'state.json').write_text(json.dumps(state))
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--python", required=True, help="clean installed environment interpreter"
    )
    parser.add_argument(
        "--root", type=Path, required=True, help="new disposable directory"
    )
    parser.add_argument(
        "--generated-client",
        type=Path,
        help="Compiled generated-client acceptance entrypoint",
    )
    args = parser.parse_args()
    root = args.root.resolve()
    root.mkdir(parents=True, exist_ok=False)
    for name in ("originals", "browser"):
        (root / name).mkdir()
    subprocess.run(
        [
            "ffmpeg",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=5:duration=1",
            "-c:v",
            "mpeg4",
            str(root / "originals/sample.mkv"),
        ],
        check=True,
        capture_output=True,
    )
    interpreter = str(Path(args.python).absolute())
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in ("PYTHONPATH", "CATABOLIC_DB", "CATABOLIC_PROFILE")
    }
    subprocess.run(
        [interpreter, "-c", BOOTSTRAP, str(root)],
        check=True,
        cwd=root,
        env=env,
        capture_output=True,
    )
    state = json.loads((root / "state.json").read_text())
    query_id = state["query_id"]
    token = json.loads((root / "credential.json").read_text())["token"]
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    commands = 0

    def request(path, body=None, *, authorized=True, headers=None):
        nonlocal commands
        values = {"Authorization": "Bearer " + token} if authorized else {}
        values.update(headers or {})
        raw = json.dumps(body).encode() if body is not None else None
        if raw is not None:
            values["Content-Type"] = "application/json"
        req = urllib.request.Request(base + path, data=raw, headers=values)
        try:
            response = urllib.request.urlopen(req, timeout=15)
        except urllib.error.HTTPError as exc:
            response = exc
        with response:
            content = response.read()
            commands += 1
            return response.status, json.loads(
                content
            ) if response.headers.get_content_type() == "application/json" else content

    processes = []
    logs = []

    def start(operation):
        log = (root / (operation + ".log")).open("ab")
        logs.append(log)
        argv = [
            interpreter,
            "-m",
            "catabolic",
            "--db",
            str(root / "catalog.sqlite3"),
            "api",
            operation,
        ]
        if operation == "serve":
            argv += ["--host", "127.0.0.1", "--port", str(port), "--enable-processing"]
        child = subprocess.Popen(argv, stdout=log, stderr=log, cwd=root, env=env)
        processes.append(child)
        return child

    def wait_ready():
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            try:
                code, body = request("/v1/capabilities")
                if code == 200 and body["worker_ready"]:
                    return
            except (OSError, urllib.error.URLError):
                pass
            time.sleep(0.1)
        raise RuntimeError("HTTP server or worker did not become ready")

    server = start("serve")
    worker = start("worker")
    try:
        wait_ready()
        assert request("/v1/me", authorized=False)[0] == 401
        code, query = request(
            "/v1/query/graphql", {"query": "{ items { nodes { id title } } }"}
        )
        assert (
            code == 200 and query["data"]["items"]["nodes"][0]["id"] == state["item_id"]
        ), query
        code, selection = request("/v1/queries/" + state.pop("query_id") + "/runs", {})
        assert code == 200 and selection["ids"] == [state["item_id"]], selection
        code, demand = request(
            "/v1/rendition-requests",
            state,
            headers={"Idempotency-Key": "installed-journey"},
        )
        assert code in (200, 202), demand
        replay = request("/v1/events")[1]["cursor"]
        server.terminate()
        server.wait(timeout=10)
        worker.terminate()
        worker.wait(timeout=10)
        server = start("serve")
        worker = start("worker")
        wait_ready()
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            code, result = request(demand["status_url"])
            assert code == 200, result
            if result["state"] == "ready":
                break
            if result["state"] == "failed":
                request(demand["status_url"] + "/retry", {})
            time.sleep(0.1)
        assert result["state"] == "ready", result
        ready = result["result"]
        code, ticket = request(
            "/v1/content-access",
            {"file_id": ready["file_id"], "revision": ready["revision"]},
        )
        assert code == 200, ticket
        code, data = request(
            ticket["content_path"], authorized=False, headers={"Range": "bytes=0-7"}
        )
        assert code == 206 and data == b"\x89PNG\r\n\x1a\n"
        assert (
            request(
                f"/v1/files/{state['source_file_id']}/content?revision={state['source_revision']}"
            )[0]
            == 404
        )
        code, events = request("/v1/events?cursor=" + replay)
        assert code == 200 and any(e["state"] == "ready" for e in events["events"]), (
            events
        )
        if args.generated_client:
            subprocess.run(
                ["node", str(args.generated_client.resolve())],
                input=json.dumps(
                    {"base": base, "token": token, "state": state, "query_id": query_id}
                ),
                text=True,
                check=True,
                cwd=root,
                env=env,
                timeout=30,
            )
        report = {
            "complete": True,
            "commands": commands,
            "generated_client": bool(args.generated_client),
            "installed_python": interpreter,
            "checks": [
                "authenticated_graphql",
                "saved_selection",
                "durable_rendition",
                "server_worker_restart",
                "opaque_ticket",
                "exact_range",
                "original_denied",
                "event_replay",
            ],
        }
        (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report))
        return 0
    finally:
        for process in processes:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        for log in logs:
            log.close()


if __name__ == "__main__":
    raise SystemExit(main())
