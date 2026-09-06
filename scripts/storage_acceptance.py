# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Actual separate filesystems and source unmounts, using only new temporary mounts.

macOS: hdiutil disposable sparse images. Linux: run as root for private tmpfs mounts.
Never accepts existing mount paths, catalogs, or source media.
"""

import argparse
import json
import os
import platform
import plistlib
import subprocess
import tempfile
import time
from pathlib import Path

if __package__:
    from .acceptance import Workflow, require, run
else:
    from acceptance import Workflow, require, run


def detach(target, system):
    """Allow transient macOS disk users to finish without forcing an unmount."""
    command = (
        ["hdiutil", "detach", str(target)]
        if system == "Darwin"
        else ["umount", str(target)]
    )
    for attempt in range(5):
        result = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=120,
            env={**os.environ, "LC_ALL": "C"},
        )
        if result.returncode == 0:
            return
        if system == "Darwin":
            # A detach can unmount the volume before returning an error. Use
            # its original device, not the now-empty mountpoint, for retries.
            # Only treat an error as success when hdiutil confirms it is gone.
            info = plistlib.loads(run(["hdiutil", "info", "-plist"]).encode())
            devices = {
                entity["dev-entry"]
                for image in info["images"]
                for entity in image["system-entities"]
            }
            if str(target) not in devices:
                return
        if (
            system != "Darwin"
            or "resource busy" not in result.stderr.lower()
            or attempt == 4
        ):
            raise AssertionError(f"command failed: {command}\n{result.stderr[-4000:]}")
        time.sleep(2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", required=True)
    parser.add_argument("--report", required=True)
    args = parser.parse_args()
    system = platform.system()
    require(system in ("Darwin", "Linux"), "unsupported storage acceptance platform")
    if system == "Linux":
        require(
            os.geteuid() == 0,
            "Linux tmpfs fixture requires root (run only this disposable harness with sudo)",
        )
    with tempfile.TemporaryDirectory(prefix="catabolic-mount-qa-") as directory:
        root = Path(directory).resolve()
        mounted = []
        devices = {}

        def mount(name):
            target = root / name
            target.mkdir(exist_ok=True)
            if system == "Darwin":
                image = root / (name + ".sparseimage")
                if not image.exists():
                    run(
                        [
                            "hdiutil",
                            "create",
                            "-quiet",
                            "-size",
                            "64m",
                            "-type",
                            "SPARSE",
                            "-fs",
                            "HFS+",
                            "-volname",
                            "Catabolic QA",
                            str(image),
                        ]
                    )
                attachment = plistlib.loads(
                    run(
                        [
                            "hdiutil",
                            "attach",
                            "-plist",
                            "-nobrowse",
                            "-mountpoint",
                            str(target),
                            str(image),
                        ]
                    ).encode()
                )
                devices[target] = attachment["system-entities"][0]["dev-entry"]
            else:
                run(
                    [
                        "mount",
                        "-t",
                        "tmpfs",
                        "-o",
                        "size=32m",
                        "catabolic-qa",
                        str(target),
                    ]
                )
            mounted.append(target)
            return target

        def unmount(target):
            detach(devices[target] if system == "Darwin" else target, system)
            mounted.remove(target)

        report = {"passed": False, "platform": system, "checks": []}
        try:
            source = mount("source")
            output = mount("output")
            media = source / "fixture.mkv"
            payload = b"disposable filesystem fixture"
            media.write_bytes(payload)
            require(
                source.stat().st_dev != output.stat().st_dev,
                "fixture did not create distinct filesystems",
            )
            workflow = Workflow(root, str(Path(args.cli).resolve()))
            workflow.cli("init")
            workflow.cli("location", "bind", "media", "--root", str(source))
            workflow.cli(
                "catalog",
                "bind",
                "hard",
                "--root",
                str(output),
                "--link-mode",
                "hardlink",
            )
            workflow.cli("scan")
            fid = workflow.cli("files")["files"][0]["id"]
            proposal = workflow.json_file(
                "proposal.json",
                {
                    "item": {
                        "kind": "movie",
                        "identities": {"fixture": "mount"},
                        "metadata": {"title": "Mount"},
                    }
                },
            )
            p = workflow.cli("proposal", "put", "--file-id", fid, "--file", proposal)
            item = workflow.cli("proposal", "accept", p["id"], "--actor", "acceptance")[
                "result"
            ]["item_id"]
            workflow.cli(
                "mapping",
                "put",
                "--catalog",
                "hard",
                "--file",
                fid,
                "--item",
                item,
                "--path",
                "fixture.mkv",
            )
            blocked = workflow.cli("sync", "--catalog", "hard", expected=3)
            require(
                not blocked["safe"] and blocked["applied"] == [],
                "cross-device hardlink batch was not blocked",
            )
            require(
                not (output / "fixture.mkv").exists(),
                "cross-device fallback unexpectedly wrote output",
            )
            require(
                media.read_bytes() == payload, "cross-device rejection changed source"
            )
            report["checks"].append("actual_cross_filesystem_rejection")
            # A separate symlink catalog remains intact through actual unmount.
            links = root / "links"
            links.mkdir()
            workflow.cli("catalog", "bind", "links", "--root", str(links))
            workflow.cli(
                "mapping",
                "put",
                "--catalog",
                "links",
                "--file",
                fid,
                "--item",
                item,
                "--path",
                "fixture.mkv",
            )
            workflow.cli("sync", "--catalog", "links")
            link = links / "fixture.mkv"
            unmount(source)
            require(source.is_dir(), "unmount did not leave its empty mountpoint")
            workflow.cli("scan", expected=3)
            require(
                not workflow.cli("sync", "--catalog", "links", expected=3)["safe"],
                "unmounted source accepted",
            )
            require(link.is_symlink(), "unmount deleted the output link")
            report["checks"].append("actual_unmount_preserves_output")
            mount("source")
            if system == "Linux":
                media.write_bytes(payload)
            require(media.read_bytes() == payload, "remount changed image content")
            # Remount may assign new device identity; explicit rebinding and scan
            # are the supported recovery, never silent adoption of a new root.
            workflow.cli("location", "bind", "media", "--root", str(source))
            workflow.cli("scan")
            require(
                workflow.cli("sync", "--catalog", "links")["healthy"],
                "remount did not recover",
            )
            require(link.read_bytes() == payload, "restored link points to wrong data")
            report["checks"].append("explicit_rebind_after_remount")
            report["passed"] = True
        finally:
            failures = []
            for target in reversed(mounted[:]):
                try:
                    unmount(target)
                except (AssertionError, subprocess.SubprocessError) as exc:
                    failures.append(str(exc))
            report["cleanup_errors"] = failures
            Path(args.report).write_text(json.dumps(report, indent=2) + "\n")
            print(json.dumps(report, indent=2), flush=True)
            # Do not recursively clean a mount that could not be detached.
            if failures:
                os._exit(1)


if __name__ == "__main__":
    main()
