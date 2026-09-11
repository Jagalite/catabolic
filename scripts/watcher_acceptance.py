# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Installed three-watcher journey on disposable real-render fallback fixtures."""

import argparse
import json
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

SETUP = r"""
import json,sys
from pathlib import Path
from catabolic.app import Application
from catabolic.store import Store
from catabolic.fallback_policies import Policies
from catabolic.fallback_projection import FallbackProjection,binding
from catabolic.plans import projection_digest
from catabolic.saved_queries import Queries
from catabolic.watchers import Watchers
root=Path(sys.argv[1])
with Store(root/'catalog.sqlite3',writable=True) as store:
 app=Application(store);watchers=Watchers(app)
 if (root/'C-offline').exists():
  (root/'C-offline').rename(root/'C')
 from catabolic.source_events import invalidate
 invalidate(app,'C')
 from catabolic.artifacts import Artifacts
 state=json.loads((root/'state.json').read_text())
 (root/'D').mkdir()
 artifacts=Artifacts(app);artifacts.bind('D',root/'D')
 recipe=store.rows('SELECT id FROM processing_recipes LIMIT 1')[0]['id']
 artifacts.enqueue(state['files']['A'],recipe,'D',state['item'])
 assert artifacts.run()['complete']
 secondary=Queries(store).put('secondary-browser',{'selection':{'language':'sql','query':"SELECT id AS file_id FROM files WHERE location='D'"}})['id']
 config=binding(store,'default','global')
 source_policy=Policies(store).get(config['policy_id'])['definition']
 browser_policy={**source_policy,'fallbacks':[source_policy['fallbacks'][2],{'name':'secondary-browser','query_id':secondary}],'failback':{'mode':'immediate'}}
 policy=Policies(store).put('browser',browser_policy)['id']
 (root/'browser').mkdir();app.bind('output','browser',str(root/'browser'))
 FallbackProjection(app).bind('browser',policy,config['query_id'],config['layout'])
 for name,catalog,seconds in [('plex','global',5),('browser','browser',300)]:
  watchers.put(name,{'plan':{'kind':'projection','catalog':catalog,'binding_digest':projection_digest(app,catalog)},'reaction':'projection','schedule':{'kind':'interval','seconds':seconds},'max_removals':1})
  watchers.enable(name,transfer=True)
 audit=Queries(store).put('backup-audit',{'mode':'rows','selection':{'language':'sql','query':"SELECT source,state,started_at,completed_at FROM catalog_source_observations ORDER BY source,generation"}})['id']
 watchers.put('audit',{'plan':{'kind':'query','query_id':audit},'schedule':{'kind':'interval','seconds':3600}})
 watchers.enable('audit')
 from catabolic.access import principal_put,grant_put,issue
 from catabolic.api_cli import save_secret
 principal_put(store,'default','watcher-operator')
 grant=grant_put(store,'watcher-operator',{'actions':['*'],'operator':True})
 secret=issue(store,'watcher-operator',[grant['grant_id']])
 save_secret(store,root/'watcher-operator.json',secret)
 (root/'watcher-query.txt').write_text(audit)
"""
STEP = r"""
import json,sys
from pathlib import Path
import catabolic
from catabolic.app import Application
from catabolic.store import Store
from catabolic.watchers import Watchers
from catabolic.fallback_worker import tick
from catabolic.source_events import invalidate
root=Path(sys.argv[1]);watcher=sys.argv[2]
with Store(root/'catalog.sqlite3',writable=True) as store:
 app=Application(store)
 if len(sys.argv)>3:
  invalidate(app,sys.argv[3])
 before=store.rows("SELECT count(*) n FROM observation_jobs WHERE state IN ('complete','unavailable')")[0]['n']
 report=Watchers(app).run(watcher)
 after=store.rows("SELECT count(*) n FROM observation_jobs WHERE state IN ('complete','unavailable')")[0]['n']
 assert report['complete'],report
 print(json.dumps({'module':catabolic.__file__,'watcher':watcher,'observations_created':after-before,'winner':report['reaction']['desired'][0]['file_id'] if report['reaction'] else None,'run_id':report['run_id']}))
assert tick(root/'catalog.sqlite3')['processed']==0
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python", required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--generated-client", type=Path)
    args = parser.parse_args()
    root = args.root.resolve()
    command = [
        sys.executable,
        str(Path(__file__).with_name("fallback_acceptance.py")),
        "--python",
        args.python,
        "--root",
        str(root),
    ]
    if args.generated_client:
        command.extend(["--generated-client", str(args.generated_client)])
    subprocess.run(command, check=True)
    subprocess.run([args.python, "-c", SETUP, str(root)], check=True, cwd="/tmp")

    def step(watcher, source=None):
        output = subprocess.check_output(
            [
                args.python,
                "-c",
                STEP,
                str(root),
                watcher,
                *([source] if source else []),
            ],
            text=True,
            cwd="/tmp",
        )
        return json.loads(output)

    results = [step("plex"), step("browser"), step("audit")]
    assert (
        results[1]["observations_created"] == results[2]["observations_created"] == 0
    ), results
    state = json.loads((root / "state.json").read_text())
    assert results[0]["winner"] == state["files"]["A"]
    assert results[1]["winner"] == state["files"]["C"]
    (root / "A").rename(root / "A-offline")
    results.append(step("plex", "A"))
    assert results[-1]["winner"] == state["files"]["B"], results[-1]
    results.append(step("browser"))
    assert results[-1]["winner"] == state["files"]["C"]
    (root / "A-offline").rename(root / "A")
    results.append(step("plex", "A"))
    assert results[-1]["winner"] == state["files"]["B"]
    time.sleep(3.2)
    results.append(step("plex"))
    assert results[-1]["winner"] == state["files"]["A"]
    if args.generated_client:
        generated = args.generated_client.resolve().with_name("watchers.js")
        with socket.socket() as socket_handle:
            socket_handle.bind(("127.0.0.1", 0))
            port = socket_handle.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        server = subprocess.Popen(
            [
                args.python,
                "-m",
                "catabolic",
                "--db",
                str(root / "catalog.sqlite3"),
                "api",
                "serve",
                "--enable-processing",
                "--port",
                str(port),
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        try:
            token = json.loads((root / "watcher-operator.json").read_text())["token"]
            deadline = time.monotonic() + 10
            while True:
                try:
                    request = urllib.request.Request(
                        base + "/v1/me", headers={"Authorization": "Bearer " + token}
                    )
                    with urllib.request.urlopen(request, timeout=1):
                        break
                except OSError:
                    if time.monotonic() > deadline:
                        raise
                    time.sleep(0.1)
            subprocess.run(
                ["node", str(generated)],
                input=json.dumps(
                    {
                        "base": base,
                        "token": token,
                        "query": (root / "watcher-query.txt").read_text(),
                    }
                ),
                text=True,
                check=True,
            )
            for _attempt in range(5):
                subprocess.run(
                    [
                        args.python,
                        "-m",
                        "catabolic",
                        "--db",
                        str(root / "catalog.sqlite3"),
                        "--json",
                        "supervise",
                        "--once",
                    ],
                    stdout=subprocess.DEVNULL,
                    check=False,
                )
                request = urllib.request.Request(
                    base + "/v1/operator/watchers/generated-audit/history",
                    headers={"Authorization": "Bearer " + token},
                )
                with urllib.request.urlopen(request, timeout=2) as response:
                    history = json.load(response)["runs"]
                if any(run["state"] == "complete" for run in history):
                    break
                time.sleep(1.1)
            assert any(run["state"] == "complete" for run in history), history
        finally:
            server.terminate()
            server.wait(timeout=10)
    print(
        json.dumps(
            {
                "complete": True,
                "journey": results,
                "checks": [
                    "independent_preferences",
                    "shared_observations",
                    "audit_reuse",
                    "offline_primary",
                    "restart_between_runs",
                    "legacy_owner_skip",
                ],
            }
        )
    )


if __name__ == "__main__":
    main()
