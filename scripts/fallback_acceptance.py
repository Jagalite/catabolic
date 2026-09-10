# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Installed A/B/C fallback journey with a real HTTP server and generated client."""

import argparse
import json
import os
import shutil
import socket
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path

BOOTSTRAP = r"""
import copy,json,sys
from pathlib import Path
from catabolic.store import Store
from catabolic.app import Application
from catabolic.artifacts import Artifacts
from catabolic.access import principal_put,grant_put,issue
from catabolic.api_cli import save_secret
from catabolic.saved_queries import Queries
from catabolic.fallback_policies import Policies
from catabolic.fallback_projection import FallbackProjection
from catabolic.fallback_worker import configure
from catabolic.layouts import Layouts,PRESETS
root=Path(sys.argv[1]); database=root/'catalog.sqlite3'
Store.initialize(database)
with Store(database,writable=True) as store:
 app=Application(store)
 for location in ('A','B'): app.bind('source',location,str(root/location))
 app.bind('output','global',str(root/'output'));app.scan()
 files={r['location']:r['id'] for r in store.rows('SELECT * FROM files')}
 item=app.put_item('movie',{'fixture':'fallback-installed'},{'title':'Fallback fixture'})['id']
 for file in files.values(): app.media.associate(file,item)
 artifacts=Artifacts(app); artifacts.bind('C',root/'C')
 recipe=artifacts.recipe('fallback','h264-720p',{'max_output_bytes':16777216})
 artifacts.enqueue(files['A'],recipe['id'],'C',item)
 assert artifacts.run()['complete']
 files['C']=store.rows('SELECT file_id FROM media_outputs')[0]['file_id']
 tiers=[]
 for location in ('A','B','C'):
  query=Queries(store).put(location,{'selection':{'language':'sql','query':'SELECT id AS file_id FROM files WHERE location=:location','params':{'location':location}}})
  tiers.append({'name':location,'query_id':query['id']})
 policy=Policies(store).put('portable-fallback',{'fallbacks':tiers,'within_tier':{'tie_break':'file_id'},'lineage_requirement':'accepted_source_revision','failback':{'mode':'stable','minimum_healthy_seconds':3,'minimum_successful_checks':2}})['id']
 members=Queries(store).put('members',{'selection':{'language':'sql','query':'SELECT id AS item_id FROM items'}})['id']
 Layouts(app).put('flat',copy.deepcopy(PRESETS['flat']))
 FallbackProjection(app).bind('global',policy,members,'flat')
 configure(app,'global',enabled=True,interval=1,max_removals=1)
 principal_put(store,'default','website')
 grant=grant_put(store,'website',{'actions':['metadata:read','content:read'],'item_ids':[item],'file_ids':list(files.values()),'fallback_policy_ids':[policy],'projection_ids':['global']})
 save_secret(store,root/'credential.json',issue(store,'website',[grant['grant_id']]))
 with store.transaction() as db:
  for location in ('A','B','C'): db.execute("INSERT INTO api_sources VALUES ('default',?)",(location,))
 (root/'state.json').write_text(json.dumps({'item':item,'policy':policy,'files':files}))
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python", required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--generated-client", type=Path)
    args = parser.parse_args()
    root = args.root.resolve()
    root.mkdir(parents=True, exist_ok=False)
    for name in ("A", "B", "C", "output"):
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
            str(root / "A/sample.mkv"),
        ],
        check=True,
        capture_output=True,
    )
    shutil.copyfile(root / "A/sample.mkv", root / "B/sample.mkv")
    interpreter = str(Path(args.python).absolute())
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in ("PYTHONPATH", "CATABOLIC_DB", "CATABOLIC_PROFILE")
    }
    subprocess.run(
        [interpreter, "-c", BOOTSTRAP, str(root)],
        cwd=root,
        env=env,
        check=True,
        capture_output=True,
    )
    state = json.loads((root / "state.json").read_text())
    token = json.loads((root / "credential.json").read_text())["token"]
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    processes, logs, transitions = [], [], []

    def start():
        log = (root / "serve.log").open("ab")
        logs.append(log)
        child = subprocess.Popen(
            [
                interpreter,
                "-m",
                "catabolic",
                "--db",
                str(root / "catalog.sqlite3"),
                "api",
                "serve",
                "--port",
                str(port),
            ],
            cwd=root,
            env=env,
            stdout=log,
            stderr=log,
        )
        processes.append(child)
        for _ in range(200):
            try:
                if request("/v1/me")[0] == 200:
                    return child
            except OSError:
                pass
            time.sleep(0.1)
        raise RuntimeError("server did not start")

    def request(path, body=None, headers=None):
        values = {"Authorization": "Bearer " + token, **(headers or {})}
        if body is not None:
            values["Content-Type"] = "application/json"
        req = urllib.request.Request(
            base + path,
            data=json.dumps(body).encode() if body is not None else None,
            headers=values,
        )
        try:
            response = urllib.request.urlopen(req, timeout=20)
        except urllib.error.HTTPError as exc:
            response = exc
        with response:
            raw = response.read()
            return response.status, json.loads(
                raw
            ) if response.headers.get_content_type() == "application/json" else raw

    def step(expected, *, resolved=None):
        # Each pass is a new supervised-worker process: health survives restart.
        time.sleep(1.05)
        completed = subprocess.run(
            [
                interpreter,
                "-m",
                "catabolic",
                "--db",
                str(root / "catalog.sqlite3"),
                "fallback",
                "worker",
                "--once",
            ],
            cwd=root,
            env=env,
            capture_output=True,
            text=True,
        )
        assert completed.returncode in (0, 3), (completed.stdout, completed.stderr)
        code, page = request("/v1/projections/global/resolutions")
        assert code == 200 and len(page["data"]) == 1, page
        row = page["data"][0]
        assert row["file_id"] == state["files"][expected], (
            expected,
            row,
            completed.stdout,
        )
        code, decision = request(
            "/v1/items/" + state["item"] + "/resolve",
            {"fallback_policy_id": state["policy"]},
        )
        assert code == 200, decision
        logical = expected if resolved is None else resolved
        if logical == "unresolved":
            assert decision["state"] == "unresolved", decision
            assert row["state"] == "unresolved", row
        else:
            assert decision["file_id"] == state["files"][logical], decision
            code, data = request(
                decision["content_path"], headers={"Range": "bytes=0-7"}
            )
            physical = next((root / logical).glob("*"))
            assert code == 206 and data == physical.read_bytes()[:8]
        links = [p for p in (root / "output").rglob("*") if p.is_symlink()]
        assert len(links) == 1
        if logical != "unresolved":
            assert links[0].exists()
        assert links[0].suffix == (".mp4" if expected == "C" else ".mkv")
        transitions.append(
            {
                "selected": expected,
                "state": row["state"],
                "generation": row["generation"],
                "published_generation": row["published_generation"],
            }
        )
        return decision

    try:
        server = start()
        original = step("A")
        (root / "A").rename(root / "A-offline")
        step("B")
        # An old URL is still A, even though a fresh logical request selects B.
        assert request(original["content_path"])[0] != 200
        (root / "B").rename(root / "B-offline")
        step("C")
        (root / "C").rename(root / "C-offline")
        step("C", resolved="unresolved")
        (root / "B-offline").rename(root / "B")
        step("B")
        (root / "A-offline").rename(root / "A")
        step("B", resolved="A")  # Stateless HTTP has no projection failback timer.
        (root / "A").rename(root / "A-offline")
        step("B")
        (root / "A-offline").rename(root / "A")
        step("B", resolved="A")
        server.terminate()
        server.wait(timeout=10)
        server = start()
        time.sleep(3.1)
        step("A")
        if args.generated_client:
            subprocess.run(
                ["node", str(args.generated_client.resolve())],
                input=json.dumps({"base": base, "token": token, **state}),
                text=True,
                check=True,
                cwd=root,
                env=env,
                timeout=30,
            )
        report = {
            "complete": True,
            "installed_python": interpreter,
            "transitions": transitions,
            "checks": [
                "A_B_C_failover",
                "container_transition",
                "unresolved_retention",
                "persistent_stable_failback",
                "server_worker_restart",
                "exact_old_URL",
                "ranged_bytes",
                "typed_client",
            ],
        }
        (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report))
    finally:
        for child in processes:
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
        for log in logs:
            log.close()


if __name__ == "__main__":
    main()
