# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Serve an already-built local demo without caching its changing assets."""

import argparse
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


class PreviewHandler(SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--port", type=int, default=8765)
    args = parser.parse_args()
    if not (args.directory / "demo.json").is_file():
        parser.error("directory must contain a built demo.json")
    with ThreadingHTTPServer(
        ("127.0.0.1", args.port),
        partial(PreviewHandler, directory=str(args.directory.resolve())),
    ) as server:
        print(f"Demo: http://127.0.0.1:{args.port}/", flush=True)
        server.serve_forever()
