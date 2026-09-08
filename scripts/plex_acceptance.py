# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Opt-in REAL Plex acceptance. Creates ONE disposable library; never deletes it.

Mount the NEW --root directory (both source and output) at --server-visible-root
on an explicitly authorized disposable Plex server. Supply a token in the named
environment variable. This does not use or select existing production libraries.
On interruption, use --resume with the same exact arguments and intent.
"""

import argparse
import json
import time
from pathlib import Path

from consumer_acceptance import Journey


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--cli", required=True)
    p.add_argument("--root", type=Path, required=True)
    p.add_argument("--endpoint", required=True)
    p.add_argument("--credential-env", required=True)
    p.add_argument("--expected-server-id", required=True)
    p.add_argument("--server-visible-root", required=True)
    p.add_argument("--library-name", required=True)
    p.add_argument(
        "--scanner", required=True, help="choice discovered from THIS server"
    )
    p.add_argument("--agent", required=True, help="choice discovered from THIS server")
    p.add_argument("--language", default="en-US")
    p.add_argument("--authorize-disposable-create", action="store_true", required=True)
    p.add_argument("--resume", action="store_true")
    p.add_argument("--timeout", type=int, default=180)
    args = p.parse_args()
    if not 1 <= args.timeout <= 600:
        p.error("--timeout must be 1..600 seconds")
    j = Journey(args.cli, args.root)
    marker = j.root / "acceptance-intent.json"
    evidence = {
        key: value
        for key, value in vars(args).items()
        if key not in ("resume", "timeout", "cli")
    }
    evidence["root"] = str(j.root)
    if args.resume:
        if json.loads(marker.read_text()) != evidence:
            p.error("resume arguments differ from the recorded disposable intent")
    else:
        j.setup()
        marker.write_text(json.dumps(evidence, indent=2))
    connection = j.call(
        "consumer",
        "connection-put",
        "fixture",
        "--application",
        "plex",
        "--endpoint",
        args.endpoint,
        "--credential-env",
        args.credential_env,
    )
    if connection["connection"]["server_id"] != args.expected_server_id:
        p.error("server identity does not match authorized disposable server")
    if (
        j.connect(args.endpoint, args.credential_env)["connection"]["server_id"]
        != args.expected_server_id
    ):
        p.error("server changed during setup")
    discovery = j.call("consumer", "discover", "fixture", "--type", "movie")
    # Populate the disposable subtree before asking Plex to validate its root.
    j.add("Catabolic Fixture One", real=True)
    j.publish()
    remote_root = args.server_visible_root.rstrip("/") + "/output/Movies"
    spec = j.root / "library.json"
    spec.write_text(
        json.dumps(
            {
                "name": args.library_name,
                "type": "movie",
                "root": remote_root,
                "scanner": args.scanner,
                "agent": args.agent,
                "language": args.language,
            }
        )
    )
    create = (
        "consumer",
        "create",
        "fixture",
        "--connection",
        "fixture",
        "--catalog",
        "movies",
        "--subtree",
        "Movies",
        "--remote-root",
        remote_root,
        "--type",
        "movie",
        "--spec",
        str(spec),
        "--automatic",
        "--initial-scan",
    )
    j.call(*create)  # Read-only preview/reconciliation inspection.
    created = j.call(*create, "--apply")
    if not created.get("complete"):
        raise AssertionError(
            "Uncertain library creation: inspect consumer creations; do not create another intent"
        )
    library = created["binding"]["library"]
    for name in ("Catabolic Fixture One", "Catabolic Fixture Two"):
        j.add(name, real=True)
        j.publish()
        deadline = time.monotonic() + args.timeout
        while True:
            j.call("consumer", "run", pending=True)
            indexed = j.call("consumer", "verify-indexing", "fixture", pending=True)
            if indexed.get("complete") and indexed.get("status") == "indexed":
                break
            if time.monotonic() >= deadline:
                raise AssertionError(
                    "Bounded real-Plex indexing remains inconclusive; inspect mounts and symlink targets"
                )
            time.sleep(5)
    before = j.call("consumer", "bindings")["bindings"][0]["generation"]
    j.call(*create, "--apply")
    j.publish()
    assert j.call("consumer", "bindings")["bindings"][0]["generation"] == before
    report = {
        "complete": True,
        "evidence": "configured server reports exact symlink-backed Part.file paths",
        "server_version": discovery["identity"]["version"],
        "server_id": args.expected_server_id,
        "library_id": library["id"],
        "library_uuid": library["uuid"],
        "commands": j.commands,
        "remote_library_retained": True,
    }
    (j.root / "report.json").write_text(json.dumps(report, indent=2))
    print(json.dumps(report))


if __name__ == "__main__":
    main()
