# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Installed CLI consumer acceptance against a disposable loopback protocol fixture."""

import argparse
import json
import os
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit
from xml.etree import ElementTree as ET


class Journey:
    def __init__(self, cli, root, environment=None):
        self.cli, self.root = str(Path(cli).resolve()), root.resolve()
        self.env = {k: v for k, v in os.environ.items() if k != "PYTHONPATH"}
        self.env.update(environment or {})
        self.db = self.root / "catalog.sqlite3"
        self.commands = 0

    def call(self, *args, pending=False):
        self.commands += 1
        result = subprocess.run(
            [self.cli, "--db", str(self.db), "--machine", *args],
            cwd=self.root,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=180,
        )
        if result.returncode not in ((0, 3) if pending else (0,)):
            # Never echo environment values or server response bodies in failures.
            raise AssertionError(
                f"CLI {args[:2]} failed with exit {result.returncode}: {result.stdout[:2000]}"
            )
        body = json.loads(result.stdout)
        assert body["interface_version"] == 1
        return body["data"]

    def setup(self):
        self.root.mkdir(parents=True, exist_ok=False)
        (self.root / "source").mkdir()
        (self.root / "output").mkdir()
        self.call("init")
        self.call("location", "bind", "source", "--root", str(self.root / "source"))
        self.call("catalog", "bind", "movies", "--root", str(self.root / "output"))
        self.call("layout", "put", "plex", "--preset", "plex")
        selection = self.root / "selection.json"
        selection.write_text(
            json.dumps(
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT item_id FROM catalog_items WHERE kind='movie'",
                    }
                }
            )
        )
        query = self.call("query", "save", "movies", "--definition", str(selection))
        self.call(
            "projection", "put", "movies", "--query", query["id"], "--layout", "plex"
        )

    def add(self, name, *, real=False):
        target = self.root / "source" / (name + ".mkv")
        if real and not target.exists():
            subprocess.run(
                [
                    "ffmpeg",
                    "-nostdin",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=c=blue:s=160x120:r=5",
                    "-t",
                    "1",
                    "-threads",
                    "1",
                    "-c:v",
                    "mpeg4",
                    str(target),
                ],
                check=True,
                capture_output=True,
                timeout=60,
            )
        elif not real:
            target.write_bytes(b"protocol fixture; not real encoded media")
        self.call("scan")
        media = next(
            row for row in self.call("files")["files"] if row["path"] == target.name
        )
        item = self.call(
            "item",
            "put",
            "--kind",
            "movie",
            "--identity",
            "fixture=" + name,
            "--metadata",
            json.dumps({"title": name, "year": 2026}),
        )
        self.call("association", "put", "--file", media["id"], "--item", item["id"])

    def connect(self, endpoint, credential):
        return self.call(
            "consumer",
            "connection-put",
            "fixture",
            "--application",
            "plex",
            "--endpoint",
            endpoint,
            "--credential-env",
            credential,
            "--apply",
        )

    def bind(self, remote_root, library_id):
        return self.call(
            "consumer",
            "bind",
            "fixture",
            "--connection",
            "fixture",
            "--catalog",
            "movies",
            "--subtree",
            "Movies",
            "--remote-root",
            remote_root,
            "--library-id",
            library_id,
            "--type",
            "movie",
            "--automatic",
            "--apply",
        )

    def publish(self):
        report = self.call("projection", "execute", "movies", pending=True)
        assert report["complete"], report
        return report


class Fixture:
    def __init__(self):
        self.scans, self.requests = 0, []
        self.expected_paths = lambda: []
        self.libraries = [("7", "uuid-7", "/fixture/Movies", "Movies")]
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                self.reply()

            def do_POST(self):
                self.reply()

            def reply(self):
                route = urlsplit(self.path)
                fixture.requests.append((self.command, route.path))
                root = ET.Element("MediaContainer")
                if route.path == "/":
                    root.set("machineIdentifier", "disposable-protocol")
                    root.set("version", "protocol-fixture")
                elif route.path == "/library/sections" and self.command == "GET":
                    for key, uuid, path, title in fixture.libraries:
                        row = ET.SubElement(
                            root,
                            "Directory",
                            key=key,
                            uuid=uuid,
                            title=title,
                            type="movie",
                            scanner="Fixture Scanner",
                            agent="fixture.agent",
                            refreshing="0",
                            createdAt="1",
                        )
                        ET.SubElement(row, "Location", path=path)
                elif route.path == "/library/sections" and self.command == "POST":
                    q = parse_qs(route.query)
                    # This movie fixture enforces Library.add's string type contract.
                    if q.get("type") != ["movie"]:
                        self.send_error(400)
                        return
                    fixture.libraries.append(
                        ("8", "uuid-8", q["location"][0], q["name"][0])
                    )
                elif route.path == "/system/scanners/1":
                    ET.SubElement(root, "Scanner", name="Fixture Scanner")
                elif route.path == "/system/agents":
                    ET.SubElement(root, "Agent", identifier="fixture.agent")
                elif route.path in {
                    f"/library/sections/{row[0]}/refresh" for row in fixture.libraries
                }:
                    if self.command != "GET":
                        self.send_error(405)
                        return
                    if route.query:
                        self.send_error(400)
                        return
                    fixture.scans += 1
                elif route.path.endswith("/all"):
                    for n, path in enumerate(fixture.expected_paths()):
                        item = ET.SubElement(
                            root, "Video", ratingKey=str(n), guid="fixture:" + str(n)
                        )
                        ET.SubElement(ET.SubElement(item, "Media"), "Part", file=path)
                else:
                    self.send_error(404)
                    return
                body = ET.tostring(root)
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.endpoint = f"http://127.0.0.1:{self.server.server_port}"

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", required=True)
    parser.add_argument(
        "--root", required=True, type=Path, help="NEW disposable directory"
    )
    args = parser.parse_args()
    journey = Journey(
        args.cli, args.root, {"CATABOLIC_FIXTURE_TOKEN": "disposable-only"}
    )
    journey.setup()
    fixture = Fixture()
    try:
        journey.connect(fixture.endpoint, "CATABOLIC_FIXTURE_TOKEN")
        journey.call("consumer", "discover", "fixture", "--type", "movie")
        journey.bind("/fixture/Movies", "7")
        assert fixture.scans == 0
        journey.add("Fixture One")
        journey.publish()
        assert fixture.scans == 1
        links = {
            p: p.lstat().st_ino
            for p in (journey.root / "output").rglob("*")
            if p.is_symlink()
        }
        assert len(links) == 1 and all(p.resolve().is_file() for p in links)
        journey.publish()
        journey.bind("/fixture/Movies", "7")
        assert fixture.scans == 1
        assert all(p.lstat().st_ino == ino for p, ino in links.items())
        journey.add("Fixture Two")
        journey.publish()
        assert fixture.scans == 2
        assert journey.call("consumer", "bindings")["complete"]
        assert journey.call("consumer", "run")["processed"] == 0
        # Creation preview is read-only; repeated apply reuses the returned identity.
        spec = journey.root / "create.json"
        spec.write_text(
            json.dumps(
                {
                    "name": "Disposable",
                    "type": "movie",
                    "root": "/other",
                    "scanner": "Fixture Scanner",
                    "agent": "fixture.agent",
                    "language": "en-US",
                }
            )
        )
        command = (
            "consumer",
            "create",
            "created",
            "--connection",
            "fixture",
            "--catalog",
            "movies",
            "--subtree",
            "Movies",
            "--remote-root",
            "/other",
            "--type",
            "movie",
            "--spec",
            str(spec),
        )
        journey.call(*command)
        assert len(fixture.libraries) == 1
        journey.call(*command, "--apply")
        journey.call(*command, "--apply")
        assert len(fixture.libraries) == 2
        adopted_again = journey.call(
            "consumer",
            "bind",
            "created",
            "--connection",
            "fixture",
            "--catalog",
            "movies",
            "--subtree",
            "Movies",
            "--remote-root",
            "/other",
            "--library-id",
            "8",
            "--type",
            "movie",
            "--apply",
        )
        assert adopted_again["binding"]["origin"] in ("created", "created_reconciled")
        journey.call("notify", "status")
        report = {
            "complete": True,
            "evidence": "loopback protocol; real Plex unverified",
            "commands": journey.commands,
            "scans": fixture.scans,
            "libraries_created": 1,
        }
        (journey.root / "report.json").write_text(json.dumps(report, indent=2))
        print(json.dumps(report))
    finally:
        fixture.close()


if __name__ == "__main__":
    main()
