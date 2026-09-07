# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Exercise an installed Catabolic CLI with generated, independently specified media.

Requires ffmpeg/ffprobe. Uses a NEW disposable root, never an existing library.
Optionally starts its own isolated Jellyfin container; never accepts a user server.
"""

import argparse
import hashlib
import json
import os
import platform
import shutil
import subprocess
import tempfile
import time
from pathlib import Path


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def run(argv, *, timeout=120, cwd=None, env=None):
    result = subprocess.run(
        argv, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout
    )
    require(
        result.returncode == 0, f"command failed: {argv[0:3]}\n{result.stderr[-4000:]}"
    )
    return result.stdout


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def fixtures(root):
    a, b = root / "source-a", root / "source-b"
    a.mkdir()
    b.mkdir()
    subtitle = a / "Feature.en.forced.srt"
    subtitle.write_text(
        "1\n00:00:00,100 --> 00:00:00,800\nBlue planet café 日本語\n\n",
        encoding="utf-8",
    )
    chapters = root / "chapters.txt"
    chapters.write_text(
        ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=500\ntitle=Opening\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=500\nEND=1000\ntitle=Closing\n"
    )
    ffmpeg = shutil.which("ffmpeg")
    require(
        ffmpeg and shutil.which("ffprobe"),
        "ffmpeg and ffprobe are mandatory for acceptance",
    )

    def encode(target, *args):
        run(
            [
                ffmpeg,
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                *args,
                "-threads",
                "1",
                str(target),
            ]
        )

    encode(
        a / "Feature.mkv",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x180:rate=24",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=880:sample_rate=48000",
        "-i",
        str(subtitle),
        "-f",
        "ffmetadata",
        "-i",
        str(chapters),
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-map",
        "2:a",
        "-map",
        "3:s",
        "-map_metadata",
        "4",
        "-map_chapters",
        "4",
        "-metadata:s:a:0",
        "language=eng",
        "-metadata:s:a:1",
        "language=jpn",
        "-metadata:s:s:0",
        "language=eng",
        "-disposition:s:0",
        "forced",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-c:a",
        "aac",
        "-c:s",
        "srt",
        "-t",
        "1",
    )
    encode(
        b / "Feature.mp4",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=160x90:rate=24",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-c:a",
        "aac",
        "-movflags",
        "+faststart",
        "-t",
        "1",
    )
    encode(
        a / "Tone.flac",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000",
        "-metadata",
        "title=Fixture tone",
        "-c:a",
        "flac",
        "-t",
        "1",
    )
    encode(
        b / "Tone.wav",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=880:sample_rate=8000",
        "-c:a",
        "pcm_s16le",
        "-t",
        "1",
    )
    encode(
        b / "Colour.png", "-f", "lavfi", "-i", "color=red:size=32x16", "-frames:v", "1"
    )
    # Transfer metadata evidence, not a claim of mastering-display HDR validation.
    encode(
        b / "Transfer.mkv",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=64x48:rate=24",
        "-vf",
        "setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-color_trc",
        "smpte2084",
        "-color_primaries",
        "bt2020",
        "-colorspace",
        "bt2020nc",
        "-t",
        "1",
    )
    (b / "Broken.mp4").write_bytes((b / "Feature.mp4").read_bytes()[:16])
    (b / "Empty.mkv").write_bytes(b"")
    (b / "Notes.md").write_text("Blue planet field notes\n", encoding="utf-8")
    return {
        "Feature.mkv": {
            "height": 180,
            "width": 320,
            "video": "h264",
            "audio": 2,
            "subtitles": 1,
            "chapters": 2,
        },
        "Feature.mp4": {
            "height": 90,
            "width": 160,
            "video": "h264",
            "audio": 1,
            "subtitles": 0,
            "chapters": 0,
        },
        "Tone.flac": {"audio_codec": "flac", "audio": 1},
        "Tone.wav": {"audio_codec": "pcm_s16le", "audio": 1},
        "Colour.png": {"height": 16, "width": 32, "video": "png", "audio": 0},
        "Transfer.mkv": {
            "height": 48,
            "width": 64,
            "video": "h264",
            "audio": 0,
            "hdr": True,
        },
    }


class Workflow:
    def __init__(self, root, executable):
        self.root, self.executable = root, executable
        self.db = root / "catalog.sqlite3"
        self.commands = 0
        self.env = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith("CATABOLIC_") and k != "PYTHONPATH"
        }

    def cli(self, *args, expected=0):
        self.commands += 1
        result = subprocess.run(
            [self.executable, "--db", str(self.db), "--json", *args],
            cwd=self.root,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=120,
        )
        require(
            result.returncode == expected,
            f"CLI {args}: exit {result.returncode}\n{result.stdout[-4000:]}\n{result.stderr[-2000:]}",
        )
        return json.loads(result.stdout)

    def json_file(self, name, data):
        path = self.root / name
        path.write_text(json.dumps(data, ensure_ascii=False), encoding="utf-8")
        return str(path)

    def exercise(self):
        recipes = fixtures(self.root)
        media = [
            p for name in ("source-a", "source-b") for p in (self.root / name).iterdir()
        ]
        hashes = {str(p.relative_to(self.root)): digest(p) for p in media}
        self.cli("init")
        for name in ("source-a", "source-b"):
            self.cli("location", "bind", name, "--root", str(self.root / name))
        self.cli("scan")
        files = {r["path"]: r["id"] for r in self.cli("files")["files"]}
        require(len(files) == len(media), "inventory omitted fixture files")
        for name in recipes:
            self.cli("process", "enqueue", "probe", "--file-id", files[name])
        require(
            self.cli("process", "run", "--limit", "100")["counts"]
            == {"complete": len(recipes)},
            "fixture probing failed",
        )
        for name, recipe in recipes.items():
            data = self.cli("process", "facts", files[name])["facts"][0]["data"]
            streams = data["streams"]
            summary = data["summary"]
            for key in ("height", "width", "hdr"):
                if key in recipe:
                    require(
                        summary[key] == recipe[key], f"{name}: wrong {key}: {summary}"
                    )
            if "video" in recipe:
                require(
                    recipe["video"] in summary["video_codecs"],
                    f"{name}: wrong video codec",
                )
            if "audio_codec" in recipe:
                require(
                    recipe["audio_codec"] in summary["audio_codecs"],
                    f"{name}: wrong audio codec",
                )
            require(
                sum(s["codec_type"] == "audio" for s in streams) == recipe["audio"],
                f"{name}: audio count",
            )
            if "subtitles" in recipe:
                require(
                    sum(s["codec_type"] == "subtitle" for s in streams)
                    == recipe["subtitles"],
                    f"{name}: subtitle count",
                )
                require(
                    len(data.get("chapters", [])) == recipe["chapters"],
                    f"{name}: chapter count",
                )
            if name == "Tone.flac":
                tags = {
                    k.lower(): v
                    for k, v in data.get("format", {}).get("tags", {}).items()
                }
                require(
                    tags.get("title") == "Fixture tone", "embedded audio title missing"
                )
            if name == "Feature.mkv":
                require(
                    {"eng", "jpn"} <= set(summary["languages"]),
                    "missing stream languages",
                )
                require(
                    any(
                        s["disposition"]["forced"]
                        for s in streams
                        if s["codec_type"] == "subtitle"
                    ),
                    "missing forced subtitle flag",
                )
                require(
                    [c["tags"]["title"] for c in data["chapters"]]
                    == ["Opening", "Closing"],
                    "wrong chapter titles",
                )
        for name in recipes:
            require(
                len(
                    self.cli("process", "enqueue", "probe", "--file-id", files[name])[
                        "cached"
                    ]
                )
                == 1,
                "probe cache miss",
            )
        require(
            self.cli("process", "run")["processed"] == 0, "unchanged probes ran again"
        )
        for name in ("Broken.mp4", "Empty.mkv"):
            self.cli("process", "enqueue", "probe", "--file-id", files[name])
        require(
            self.cli("process", "run", expected=3)["counts"] == {"failed": 2},
            "corrupt files reported healthy",
        )
        for name in ("Feature.mkv", "Tone.flac"):
            self.cli("process", "enqueue", "decode", "--file-id", files[name])
        require(
            self.cli("process", "run")["counts"] == {"complete": 2},
            "real AV decode failed",
        )
        for name in ("Feature.en.forced.srt", "Notes.md"):
            self.cli("process", "enqueue", "text", "--file-id", files[name])
        self.cli("process", "run")
        hits = self.cli("content", "search", "blue planet")["hits"]
        require(len(hits) == 2 and all(h["current"] for h in hits), "wrong text hits")
        require(
            any(h["locator"].get("seconds") == 0.1 for h in hits),
            "subtitle locator missing",
        )
        for name in ("Feature.mkv", "Feature.mp4"):
            payload = self.json_file(
                "proposal.json",
                {
                    "item": {
                        "kind": "movie",
                        "identities": {"fixture": "feature"},
                        "metadata": {"title": "Fixture Feature", "year": 2020},
                    }
                },
            )
            proposal = self.cli(
                "proposal", "put", "--file-id", files[name], "--file", payload
            )
            decision = self.cli(
                "proposal", "accept", proposal["id"], "--actor", "acceptance"
            )
        item = decision["result"]["item_id"]
        self.cli("tag", "put", "qa:accepted")
        self.cli("tag", "add", "qa:accepted", "--item", item)
        require(
            self.cli("query", "SELECT count(*) FROM catalog_taggings")["rows"] == [[1]],
            "tag assignment was not persisted",
        )
        sidecars = self.cli("sidecar", "--apply")["candidates"]
        require(len(sidecars) == 1, "unexpected sidecar candidates")
        self.cli("proposal", "accept", sidecars[0]["proposal"], "--actor", "acceptance")
        for catalog, mode in (("jellyfin", "symlink"), ("hard", "hardlink")):
            output = self.root / catalog
            output.mkdir()
            self.cli(
                "catalog", "bind", catalog, "--root", str(output), "--link-mode", mode
            )
            policy = self.json_file(
                "copies.json", {"prefer": [{"field": "height", "order": "desc"}]}
            )
            self.cli("copies", "put", "--catalog", catalog, "--file", policy)
        self.cli("layout", "put", "movies", "--preset", "plex")
        for catalog in ("jellyfin", "hard"):
            self.cli("layout", "apply", "movies", "--catalog", catalog)
            self.cli("sync", "--catalog", catalog, "--dry-run", "--max-removals", "0")
            self.cli("sync", "--catalog", catalog, "--max-removals", "0")
            require(
                self.cli("sync", "--catalog", catalog)["applied"] == [],
                "repeat sync mutated output",
            )
            require(
                self.cli("verify", "--catalog", catalog)["healthy"],
                "output verification failed",
            )
        self.projected = (
            self.root
            / "jellyfin/Movies/Fixture Feature (2020)/Fixture Feature (2020).mkv"
        )
        require(
            self.projected.resolve() == self.root / "source-a/Feature.mkv",
            "wrong selected copy",
        )
        require(
            os.path.samefile(
                self.root / "hard" / self.projected.relative_to(self.root / "jellyfin"),
                self.projected,
            ),
            "hardlink identity wrong",
        )
        self.cli("process", "enqueue", "hash", "--file-id", files["Tone.wav"])
        self.cli("process", "run")
        self.cli("process", "enqueue", "verify", "--file-id", files["Tone.wav"])
        self.cli("process", "run")
        require(
            self.cli(
                "query",
                "SELECT count(*) FROM catalog_facts WHERE operation='probe' AND current=1",
            )["rows"]
            == [[len(recipes)]],
            "SQL facts mismatch",
        )
        require(
            "errors"
            not in self.cli(
                "graphql", "{files(first:100){nodes{id facts}} jobs(first:100){nodes}}"
            ),
            "GraphQL failed",
        )
        # A readable replacement directory is not the original registered source.
        a = self.root / "source-a"
        detached = self.root / "detached"
        a.rename(detached)
        a.mkdir()
        try:
            self.cli("scan", "source-a", expected=3)
            require(
                not self.cli("sync", "--catalog", "jellyfin", expected=3)["safe"],
                "lost root did not block sync",
            )
            require(self.projected.is_symlink(), "lost root removed output")
        finally:
            a.rmdir()
            detached.rename(a)
        self.cli("scan", "source-a")
        require(
            self.cli("sync", "--catalog", "jellyfin")["applied"] == [],
            "source restoration changed output",
        )
        generated = self.root / "generated"
        generated.mkdir()
        self.cli("artifact", "bind", "generated", "--root", str(generated))
        mobile = self.cli(
            "rendition",
            "define",
            "mobile",
            "--definition",
            json.dumps({"role": "custom:mobile", "file_metadata": {"device": "phone"}}),
        )
        for preset in ("thumbnail", "remux-mkv", "h264-720p", "subtitle-srt"):
            extra = (
                [
                    "--output-definition",
                    mobile["id"],
                    "--options",
                    '{"audio_stream":null,"crf":30}',
                ]
                if preset == "h264-720p"
                else []
            )
            recipe = self.cli("artifact", "recipe", preset, "--preset", preset, *extra)
            queued = self.cli(
                "artifact",
                "enqueue",
                "--file-id",
                files["Feature.mkv"],
                "--item-id",
                item,
                "--recipe",
                recipe["id"],
                "--location",
                "generated",
            )
            created = self.cli("artifact", "run", "--limit", "1")["completed"][0]
            artifact = self.cli("artifact", "show", created["artifact_id"])
            require(artifact["state"] == "ready", "generated output was not registered")
            require(
                digest(generated / artifact["path"]) == artifact["sha256"],
                "generated output hash mismatch",
            )
            rendition = self.cli("rendition", "show", created["output_id"])
            require(
                rendition["origin"] == "generated"
                and rendition["source_file_id"] == files["Feature.mkv"],
                "missing generated rendition provenance",
            )
            if preset == "h264-720p":
                require(
                    rendition["definition_id"] == mobile["id"]
                    and rendition["metadata"]["device"] == "phone",
                    "custom output definition was not applied",
                )
            require(
                self.cli("process", "attempts", queued["job_id"])["attempts"][0][
                    "state"
                ]
                == "complete",
                "missing output attempt history",
            )
        require(
            self.cli("artifact", "recover")["recovered"] == [],
            "completed outputs needed recovery",
        )
        # External output registration records the user's claim without inventing execution history.
        external = a / "External-remux.mkv"
        shutil.copyfile(a / "Feature.mkv", external)
        self.cli("scan", "source-a")
        external_id = self.cli(
            "query",
            "SELECT file_id FROM catalog_files WHERE source_relative_path='External-remux.mkv' AND profile='default'",
        )["rows"][0][0]
        external_result = self.cli(
            "rendition",
            "register",
            "--file-id",
            external_id,
            "--source-file-id",
            files["Feature.mkv"],
            "--item-id",
            item,
            "--output-definition",
            mobile["id"],
        )
        require(
            external_result["origin"] == "registered"
            and external_result["artifact_id"] is None,
            "external rendition was mislabeled as generated",
        )
        self.cli("scan", "generated")
        require(
            self.cli("sync", "--catalog", "jellyfin")["applied"] == [],
            "generated outputs changed existing selection",
        )
        for path, before in hashes.items():
            require(
                digest(self.root / path) == before, f"source content changed: {path}"
            )
        self.hashes = hashes
        return {
            "media_files": len(media),
            "probe_recipes": recipes,
            "source_hashes": hashes,
            "commands": self.commands,
            "checks": [
                "generated_artifacts",
                "custom_rendition_definitions",
                "external_rendition_registration",
                "probe_values",
                "languages",
                "chapters",
                "forced_subtitles",
                "corruption",
                "decode",
                "cache",
                "text_locations",
                "proposals",
                "tags",
                "copy_selection",
                "sidecars",
                "symlinks",
                "hardlinks",
                "repeat_sync",
                "source_replacement",
                "source_preservation",
                "SQL",
                "GraphQL",
            ],
        }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", default=shutil.which("catabolic"))
    parser.add_argument("--root", help="new directory; existing paths are refused")
    parser.add_argument(
        "--jellyfin-image",
        help="explicit version/digest of Jellyfin image; requires Docker",
    )
    args = parser.parse_args()
    require(args.cli, "specify an installed --cli executable")
    root = (
        Path(args.root).absolute()
        if args.root
        else Path(tempfile.mkdtemp(prefix="catabolic-acceptance-")) / "run"
    )
    root.mkdir()
    root = root.resolve()
    started = time.monotonic()
    report = {
        "passed": False,
        "root": str(root),
        "platform": platform.platform(),
        "consumer": {"status": "not_run"},
    }
    try:
        workflow = Workflow(root, str(Path(args.cli).resolve()))
        report.update(workflow.exercise())
        report["cli_version"] = run(
            [workflow.executable, "--version"], cwd=root, env=workflow.env
        ).strip()
        report["python"] = platform.python_version()
        report["tools"] = {
            tool: run([shutil.which(tool), "-version"]).splitlines()[0]
            for tool in ("ffmpeg", "ffprobe")
        }
        if args.jellyfin_image:
            report["consumer"] = {"status": "running", "image": args.jellyfin_image}
            from acceptance_jellyfin import verify_consumer

            report["consumer"] = verify_consumer(workflow, args.jellyfin_image)
            for path, before in workflow.hashes.items():
                require(
                    digest(root / path) == before, "consumer altered source content"
                )
        report["passed"] = True
    except Exception as exc:
        report["error"] = str(exc)[:4000]
        if report["consumer"]["status"] == "running":
            report["consumer"]["status"] = "failed"
        raise
    finally:
        report["seconds"] = time.monotonic() - started
        (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report, indent=2), flush=True)


if __name__ == "__main__":
    main()
