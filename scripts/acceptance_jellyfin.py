# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Disposable real-consumer lane; never connects to an existing user server."""

import json
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
from uuid import uuid4

from acceptance import require, run


def verify_consumer(workflow, image):
    require(
        image.startswith("jellyfin/jellyfin:")
        or image.startswith("jellyfin/jellyfin@sha256:"),
        "use an explicit official Jellyfin version/digest",
    )
    require(not image.endswith(":latest"), "pin the consumer image version")
    name = "catabolic-qa-" + uuid4().hex
    token = None
    base = None

    def api(path, data=None, *, method=None, binary=False):
        headers = {
            "Authorization": 'MediaBrowser Client="Catabolic QA", Device="Fixture", DeviceId="catabolic-acceptance", Version="1"'
        }
        if token:
            headers["X-Emby-Token"] = token
        if data is not None:
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(
            base + path,
            headers=headers,
            data=json.dumps(data).encode() if data is not None else None,
            method=method,
        )
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(request, timeout=10) as response:
            body = response.read(2 * 1024 * 1024 + 1)
        require(
            len(body) <= 2 * 1024 * 1024, "consumer response exceeded fixture bound"
        )
        return body if binary else json.loads(body) if body else None

    def eventually(callback, seconds=180):
        deadline = time.monotonic() + seconds
        error = None
        while time.monotonic() < deadline:
            running = run(
                ["docker", "inspect", "--format", "{{.State.Running}}", name]
            ).strip()
            require(
                running == "true", "Jellyfin exited; inspect the retained jellyfin.log"
            )
            try:
                value = callback()
                if value:
                    return value
            except (OSError, urllib.error.URLError, AssertionError) as exc:
                error = exc
            time.sleep(1)
        raise AssertionError(f"consumer condition timed out: {error}")

    # Only this invocation's uniquely named container is removed, even on failure.
    try:
        run(
            [
                "docker",
                "run",
                "-d",
                "--name",
                name,
                "--label",
                "catabolic.acceptance=true",
                "-p",
                "127.0.0.1::8096",
                "--mount",
                f"type=bind,src={workflow.root},dst=/media,readonly",
                "--tmpfs",
                "/config:rw,size=3g",
                "--tmpfs",
                "/cache:rw,size=3g",
                image,
            ],
            timeout=300,
        )
        port = run(["docker", "port", name, "8096/tcp"]).strip().split(":")[-1]
        base = "http://127.0.0.1:" + port
        eventually(lambda: api("/System/Info/Public"))
        eventually(lambda: api("/Startup/User"))
        password = uuid4().hex
        api("/Startup/User", {"Name": "catabolic-qa", "Password": password})
        api("/Startup/Complete", {}, method="POST")
        auth = api(
            "/Users/AuthenticateByName", {"Username": "catabolic-qa", "Pw": password}
        )
        token = auth["AccessToken"]
        system = api("/System/Info")
        require(bool(system.get("Version")), "server version missing after startup")
        api(
            "/Library/VirtualFolders?"
            + urllib.parse.urlencode(
                {
                    "name": "Fixture",
                    "collectionType": "movies",
                    "paths": "/media/jellyfin/Movies",
                    "refreshLibrary": "false",
                }
            ),
            {
                "LibraryOptions": {
                    "EnableRealtimeMonitor": False,
                    "EnableInternetProviders": False,
                    "EnableChapterImageExtraction": False,
                    "EnableTrickplayImageExtraction": False,
                    "TypeOptions": [
                        {"Type": kind, "MetadataFetchers": [], "ImageFetchers": []}
                        for kind in ("Movie", "Video")
                    ],
                    "SubtitleFetcherOrder": [],
                    "DisabledSubtitleFetchers": ["Open Subtitles"],
                }
            },
        )
        # Drive the real Catabolic refresh outbox through an actual output change.
        workflow.env["CATABOLIC_QA_REFRESH_TOKEN"] = token
        workflow.cli(
            "refresh",
            "configure",
            "--catalog",
            "jellyfin",
            "--endpoint",
            base,
            "--credential-env",
            "CATABOLIC_QA_REFRESH_TOKEN",
        )
        rows = workflow.cli(
            "query",
            "SELECT file_id,item_id FROM catalog_item_files WHERE role='subtitle' AND active=1",
        )["rows"]
        require(len(rows) == 1, "expected one accepted subtitle association")
        file_id, item_id = rows[0]
        workflow.cli(
            "mapping",
            "put",
            "--catalog",
            "jellyfin",
            "--file",
            file_id,
            "--item",
            item_id,
            "--path",
            "Movies/Fixture Feature (2020)/Fixture Feature (2020).jpn.srt",
        )
        synced = workflow.cli("sync", "--catalog", "jellyfin", "--max-removals", "0")
        require(
            bool(synced.get("refresh_events")), "output change did not queue refresh"
        )
        require(workflow.cli("refresh", "run")["complete"], "refresh delivery failed")

        def find_movie():
            items = api(
                "/Items?Recursive=true&IncludeItemTypes=Movie&Fields=Path,MediaStreams,MediaSources"
            )["Items"]
            matches = [
                i
                for i in items
                if i.get("Path")
                == "/media/jellyfin/Movies/Fixture Feature (2020)/Fixture Feature (2020).mkv"
            ]
            if len(matches) != 1:
                return None
            item = api("/Users/" + auth["User"]["Id"] + "/Items/" + matches[0]["Id"])
            streams = item.get("MediaStreams", [])
            if not any(
                s.get("IsExternal") and s.get("Type") == "Subtitle" for s in streams
            ):
                return None
            return item

        movie = eventually(find_movie)
        streams = movie["MediaStreams"]
        require(
            any(
                s.get("Type") == "Video"
                and s.get("Height") == 180
                and s.get("Codec") == "h264"
                for s in streams
            ),
            "consumer selected wrong video",
        )
        require(
            sum(s.get("Type") == "Audio" for s in streams) == 2,
            "consumer lost audio streams",
        )
        require(
            any(
                s.get("Type") == "Subtitle"
                and s.get("IsExternal")
                and s.get("Language") == "eng"
                for s in streams
            ),
            "consumer missed English sidecar",
        )
        payload = api("/Videos/" + movie["Id"] + "/stream?Static=true", binary=True)
        require(
            payload == workflow.projected.read_bytes(),
            "consumer could not serve the projected source bytes",
        )
        require(
            workflow.cli("sync", "--catalog", "jellyfin")["applied"] == [],
            "consumer scan disturbed owned output",
        )
        return {
            "status": "passed",
            "image": image,
            "version": system["Version"],
            "image_id": run(
                ["docker", "inspect", "--format", "{{.Image}}", name]
            ).strip(),
            "checks": [
                "scan",
                "selected_copy",
                "audio_streams",
                "external_subtitle",
                "static_playback_bytes",
                "refresh_delivery",
            ],
        }
    finally:
        info = subprocess.run(
            ["docker", "inspect", name], capture_output=True, text=True, timeout=15
        )
        if info.returncode == 0:
            (workflow.root / "jellyfin-container.json").write_text(info.stdout)
            logs = subprocess.run(
                ["docker", "logs", name], capture_output=True, text=True, timeout=15
            )
            (workflow.root / "jellyfin.log").write_text(logs.stdout + logs.stderr)
            run(["docker", "rm", "-f", name], timeout=30)
