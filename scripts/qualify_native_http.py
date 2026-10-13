# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Experimental native HTTP/worker/client qualification on disposable media.

Python builds the schema-32 fixture only. Every served request, render worker,
restart, ticket transfer and generated-client call uses the explicit native CLI.
This is evidence for the opt-in backend, not production/default enablement.
"""

import argparse
import fcntl
import hashlib
import json
import os
import socket
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path

if not __debug__:
    raise RuntimeError("HTTP qualification requires unoptimized Python assertions")


def artifact_hashes(binary, client):
    result = {"native_cli_sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
    if client:
        if not client.is_file():
            raise ValueError("generated client entrypoint is missing")
        result["generated_client_sha256"] = {
            str(path.relative_to(client.parent)): hashlib.sha256(
                path.read_bytes()
            ).hexdigest()
            for path in sorted(client.parent.rglob("*"))
            if path.is_file()
        }
    return result


def stop_owned_worker(worker):
    """Reap this fixture's writer before non-replayable HTTP operations."""
    if worker.poll() is not None:
        raise RuntimeError("native worker exited before fixture isolation")
    worker.terminate()
    code = worker.wait(timeout=10)
    if code != 0:
        raise RuntimeError(f"native worker shutdown failed: {code}")
    return {
        "phase": "after_processing_ready_before_snapshot_and_ticket",
        "pid": worker.pid,
        "returncode": code,
        "reaped": True,
    }


def check_writer_contention(root, request):
    """Exercise admission contention separately, without replaying a mutation.

    Store::open takes this advisory writer lock before connecting to SQLite.
    This deliberately rejected request uses a sentinel file, never the rendered
    file used in the journey. The check makes no claim about other busy errors.
    Python only controls the disposable fixture lock; native HTTP serves it.
    """
    with (root / "catalog.sqlite3.lock").open("r+b") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        try:
            code, problem = request(
                "/v1/content-access",
                {
                    "file_id": "qualification-contended-admission",
                    "revision": "never-replayed",
                },
            )
        finally:
            fcntl.flock(lock, fcntl.LOCK_UN)
    assert code == 503, problem
    assert problem["code"] == "catalog_busy" and problem["retryable"] is True, problem
    return {"phase": "forced_writer_admission", "status": code, "complete": True}


def run_generated_client(client, fixture, root, env, isolate, phases, timeout=30):
    """Release the client's mutation phase only after acknowledged worker reap.

    Credentials still enter through stdin. A separate inherited pipe carries
    one acknowledgement byte, never credentials. Output is preserved even when
    the client, phase protocol, shutdown, or deadline fails.
    """
    command = ["node", str(client.resolve())]
    stdout_path = root / "generated-client.stdout"
    stderr_path = root / "generated-client.stderr"
    read_fd, write_fd = os.pipe()
    fixture = {**fixture, "phase_fd": read_fd}
    process = None
    failure = None
    deadline = time.monotonic() + timeout
    expected = ["processing_ready", "isolated_complete"]
    try:
        with stdout_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
            process = subprocess.Popen(
                command,
                stdin=subprocess.PIPE,
                stdout=stdout,
                stderr=stderr,
                pass_fds=(read_fd,),
                cwd=root,
                env=env,
            )
            os.close(read_fd)
            read_fd = None
            process.stdin.write(json.dumps(fixture).encode())
            process.stdin.close()
            consumed = 0
            exited = False
            while True:
                if time.monotonic() >= deadline:
                    raise subprocess.TimeoutExpired(command, timeout)
                lines = stdout_path.read_text(errors="replace").splitlines(
                    keepends=True
                )
                for line in lines[consumed:]:
                    if not line.endswith("\n"):
                        break
                    consumed += 1
                    try:
                        event = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    if (
                        not isinstance(event, dict)
                        or event.get("type") != "qualification_phase"
                    ):
                        continue
                    phase = event.get("phase")
                    if len(phases) >= len(expected) or phase != expected[len(phases)]:
                        raise RuntimeError("generated client phase order is invalid")
                    phases.append(phase)
                    if phase == "processing_ready":
                        receipt = isolate()
                        if receipt.get("reaped") is not True:
                            raise RuntimeError("worker reap was not acknowledged")
                        if time.monotonic() >= deadline:
                            raise subprocess.TimeoutExpired(command, timeout)
                        os.write(write_fd, b"1")
                        os.close(write_fd)
                        write_fd = None
                if process.poll() is not None:
                    if exited:
                        break
                    # Drain once after observing exit: the final marker may
                    # have been written between our read and this poll.
                    exited = True
                    continue
                time.sleep(0.01)
            if process.returncode == 0 and phases != expected:
                raise RuntimeError("generated client phase acknowledgement is missing")
    except BaseException as error:
        failure = error
    finally:
        for descriptor in (read_fd, write_fd):
            if descriptor is not None:
                os.close(descriptor)
        if process is not None:
            if process.stdin is not None and not process.stdin.closed:
                process.stdin.close()
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=10)
    stdout = stdout_path.read_text(errors="replace")
    stderr = stderr_path.read_text(errors="replace")
    if isinstance(failure, subprocess.TimeoutExpired):
        raise subprocess.TimeoutExpired(command, timeout, output=stdout, stderr=stderr)
    if failure is not None:
        raise failure
    return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


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
        "--native", type=Path, required=True, help="Explicit native CLI artifact"
    )
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
    native = str(args.native.resolve())
    if not Path(native).is_file():
        raise RuntimeError("native CLI artifact does not exist")
    artifacts_before = artifact_hashes(Path(native), args.generated_client)
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
    # Fixture credentials never leave this disposable local process family.
    env["CATABOLIC_NATIVE_BINARY"] = native
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    commands = 0
    busy_retries = 0
    busy_attempts = []
    worker_isolation = None
    contention_check = None
    generated_phases = []

    def request(path, body=None, *, authorized=True, headers=None):
        nonlocal commands, busy_retries
        values = {"Authorization": "Bearer " + token} if authorized else {}
        values.update(headers or {})
        raw = json.dumps(body).encode() if body is not None else None
        if raw is not None:
            values["Content-Type"] = "application/json"
        req = urllib.request.Request(base + path, data=raw, headers=values)
        deadline = time.monotonic() + 10
        pathname = path.split("?", 1)[0]
        replay_safe = (
            "%" not in pathname
            and "/content" not in pathname
            and "/segments/" not in pathname
            and "ticket=" not in path
            and (
                body is None
                or pathname in ("/v1/query/graphql", "/v1/query/sql")
                or pathname.startswith("/v1/queries/")
                and pathname.endswith("/runs")
                and len(pathname.split("/")) == 5
                or pathname == "/v1/rendition-requests"
                and bool(values.get("Idempotency-Key", "").strip())
            )
        )
        attempt = 0
        while True:
            attempt += 1
            try:
                response = urllib.request.urlopen(req, timeout=15)
            except urllib.error.HTTPError as exc:
                response = exc
            with response:
                content = response.read()
                commands += 1
                kind = response.headers.get_content_type()
                value = (
                    json.loads(content)
                    if kind in ("application/json", "application/problem+json")
                    else content
                )
                # Continuous native worker heartbeats may briefly own the
                # writer. Honor only its explicit retryable busy response;
                # never hide arbitrary failures or retry an unknown outcome.
                if (
                    response.status == 503
                    and isinstance(value, dict)
                    and value.get("code") == "catalog_busy"
                    and value.get("retryable") is True
                ):
                    retry = replay_safe and time.monotonic() < deadline
                    busy_attempts.append(
                        {
                            "method": req.get_method(),
                            "path": pathname,
                            "status": 503,
                            "attempt": attempt,
                            "retried": retry,
                        }
                    )
                    if retry:
                        busy_retries += 1
                        try:
                            delay = float(response.headers.get("Retry-After", "1"))
                        except (TypeError, ValueError):
                            delay = 1.0
                        if not 0 <= delay <= 1:
                            delay = 1.0
                        time.sleep(max(0.01, delay))
                        continue
                return response.status, value

    processes = []
    logs = []

    def start(operation):
        log = (root / (operation + ".log")).open("ab")
        logs.append(log)
        argv = [
            native,
            "--db",
            str(root / "catalog.sqlite3"),
            "api",
            operation,
        ]
        if operation == "serve":
            argv += [
                "--experimental-native-http",
                "--host",
                "127.0.0.1",
                "--port",
                str(port),
                "--enable-processing",
            ]
        child = subprocess.Popen(argv, stdout=log, stderr=log, cwd=root, env=env)
        processes.append(child)
        return child

    def isolate_worker():
        nonlocal worker_isolation
        worker_isolation = stop_owned_worker(worker)
        return worker_isolation

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

    try:
        server = start("serve")
        worker = start("worker")
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
            assert result["state"] != "failed", result
            time.sleep(0.1)
        assert result["state"] == "ready", result
        ready = result["result"]
        generated_busy_attempts = []
        if args.generated_client:
            timed_out = False
            generated_error = None
            try:
                generated = run_generated_client(
                    args.generated_client,
                    {
                        "base": base,
                        "token": token,
                        "state": state,
                        "query_id": query_id,
                    },
                    root,
                    env,
                    isolate_worker,
                    generated_phases,
                )
            except subprocess.TimeoutExpired as error:
                timed_out = True
                # TimeoutExpired can carry bytes even when text=True. Preserve
                # partial client evidence before producing the incomplete report.
                generated = subprocess.CompletedProcess(
                    error.cmd, 124, error.stdout or "", error.stderr or ""
                )
            except (RuntimeError, OSError) as error:
                generated_error = str(error)
                generated = subprocess.CompletedProcess(
                    ["node", str(args.generated_client.resolve())],
                    1,
                    (root / "generated-client.stdout").read_text(errors="replace"),
                    (root / "generated-client.stderr").read_text(errors="replace"),
                )
            stdout = (
                generated.stdout.decode(errors="replace")
                if isinstance(generated.stdout, bytes)
                else generated.stdout
            )
            stderr = (
                generated.stderr.decode(errors="replace")
                if isinstance(generated.stderr, bytes)
                else generated.stderr
            )
            (root / "generated-client.stdout").write_text(stdout)
            (root / "generated-client.stderr").write_text(stderr)
            for line in stdout.splitlines():
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if (
                    isinstance(event, dict)
                    and event.get("type") == "catalog_busy_attempt"
                ):
                    generated_busy_attempts.append(event)
            if generated.returncode:
                (root / "report.json").write_text(
                    json.dumps(
                        {
                            "complete": False,
                            "commands": commands,
                            "catalog_busy_attempts": busy_attempts,
                            "generated_client_busy_attempts": generated_busy_attempts,
                            "generated_client_phases": generated_phases,
                            "worker_isolation": worker_isolation,
                            "contention_check": contention_check,
                            "error": "generated_client_timed_out"
                            if timed_out
                            else "generated_client_phase_failed"
                            if generated_error
                            else "generated_client_failed",
                            "phase_error": generated_error,
                        },
                        indent=2,
                    )
                    + "\n"
                )
                raise RuntimeError(
                    "generated client failed; preserved output and busy evidence"
                )
        # Processing/restart is exercised with the real worker. Ticket issuance,
        # transfer and generated-client mutations then run without our background
        # heartbeat racing them. Never broaden replay safety to make this pass.
        if worker_isolation is None:
            worker_isolation = stop_owned_worker(worker)
        before_commands, before_busy = commands, len(busy_attempts)
        contention_check = check_writer_contention(root, request)
        assert commands == before_commands + 1
        assert len(busy_attempts) == before_busy + 1
        assert busy_attempts[-1]["retried"] is False
        busy_attempts[-1]["phase"] = contention_check["phase"]
        contention_check["attempts"] = 1
        contention_check["retried"] = False
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
        artifacts_after = artifact_hashes(Path(native), args.generated_client)
        if artifacts_before != artifacts_after:
            (root / "report.json").write_text(
                json.dumps(
                    {"complete": False, "error": "qualification_artifact_changed"}
                )
                + "\n"
            )
            raise RuntimeError(
                "native CLI or generated client changed during qualification"
            )
        report = {
            "complete": True,
            "commands": commands,
            "catalog_busy_retries": busy_retries,
            "catalog_busy_attempts": busy_attempts,
            "generated_client_busy_attempts": generated_busy_attempts,
            "generated_client": bool(args.generated_client),
            "generated_client_phases": generated_phases,
            "worker_isolation": worker_isolation,
            "contention_check": contention_check,
            "fixture_python": interpreter,
            "native_cli": native,
            "artifacts": artifacts_before,
            "artifacts_unchanged": True,
            "release_qualified": False,
            "release_blockers": [
                "private_unregistered_orphaned_database_sidecar_aliases",
                "remaining_platform_and_full_contract_gates",
            ],
            "checks": [
                "authenticated_graphql",
                "saved_selection",
                "durable_rendition",
                "server_worker_restart",
                "worker_isolated_after_ready",
                "generated_client_phased_isolation"
                if args.generated_client
                else "python_only_isolation",
                "separate_writer_admission_contention",
                "opaque_ticket",
                "exact_range",
                "original_denied",
                "event_replay",
            ],
        }
        (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report))
        return 0
    except Exception:
        if not (root / "report.json").exists():
            (root / "report.json").write_text(
                json.dumps(
                    {
                        "complete": False,
                        "commands": commands,
                        "catalog_busy_attempts": busy_attempts,
                        "generated_client_phases": generated_phases,
                        "worker_isolation": worker_isolation,
                        "contention_check": contention_check,
                        "error": "qualification_failed",
                    },
                    indent=2,
                )
                + "\n"
            )
        raise
    finally:
        for process in processes:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=10)
        for log in logs:
            log.close()


if __name__ == "__main__":
    raise SystemExit(main())
