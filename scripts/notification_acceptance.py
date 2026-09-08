# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Optional installed Apprise acceptance against local ntfy protocol endpoints."""

import argparse
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from consumer_acceptance import Fixture, Journey


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--cli", required=True, help="installed CLI with notifications extra"
    )
    parser.add_argument(
        "--root", type=Path, required=True, help="NEW disposable directory"
    )
    args = parser.parse_args()
    received, fail = [], [True]

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            data = self.rfile.read(int(self.headers["Content-Length"]))
            received.append((self.path, data.decode()))
            self.send_response(
                503 if fail[0] and ("bad" in self.path or b'"bad"' in data) else 200
            )
            self.send_header("Content-Length", "2")
            self.end_headers()
            self.wfile.write(b"{}")

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    fixture = Fixture()
    try:
        j = Journey(
            args.cli,
            args.root,
            {
                "CATABOLIC_FIXTURE_TOKEN": "disposable-only",
                "NOTIFY_GOOD": f"ntfy://127.0.0.1:{server.server_port}/good",
                "NOTIFY_BAD": f"ntfy://127.0.0.1:{server.server_port}/bad",
            },
        )
        j.setup()
        j.connect(fixture.endpoint, "CATABOLIC_FIXTURE_TOKEN")
        j.bind("/fixture/Movies", "7")
        for name in ("good", "bad"):
            j.call(
                "notify",
                "put",
                name,
                "--credential-env",
                "NOTIFY_" + name.upper(),
                "--event",
                "projection_updated",
                "--tag",
                "fixture",
                "--apply",
            )
        j.add("Private Fixture Title")
        j.publish()
        states = {
            r["destination_id"]: r["state"]
            for r in j.call("notify", "status", pending=True)["deliveries"]
        }
        assert states == {"good": "complete", "bad": "retry"}, states
        assert len(received) == 2, received
        assert all(
            "Private Fixture Title" not in body and str(j.root) not in body
            for _, body in received
        )
        fail[0] = False
        j.call("notify", "retry", "bad")
        assert j.call("notify", "run", "--tag", "fixture")["complete"]
        assert len(received) == 3
        assert fixture.scans == 1
        report = {
            "complete": True,
            "evidence": "real pinned Apprise with loopback ntfy protocol; no external notification accounts",
            "commands": j.commands,
            "requests": len(received),
            "successful_destination_not_repeated": True,
        }
        (j.root / "report.json").write_text(json.dumps(report, indent=2))
        print(json.dumps(report))
    finally:
        fixture.close()
        server.shutdown()
        server.server_close()
        thread.join()


if __name__ == "__main__":
    main()
