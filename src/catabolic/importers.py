# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Explicit, bounded import adapters using official application CLIs."""

import os
import shutil
import tempfile
from collections import defaultdict
from pathlib import Path
from uuid import uuid4

from .curation import occurrence
from .domain import CatabolicError
from .exports import http_base
from .filesystem import open_directory
from .process_runner import CommandFailure, command_output
from .source_access import validated_source


def import_catalog(
    app, document, target, destination, *, apply=False, limit=1000, timeout=300
):
    if target not in ("calibre", "calibre-web", "immich"):
        raise CatabolicError(
            "import supports calibre, calibre-web and immich; use layout presets for other targets"
        )
    if type(limit) is not int or not 1 <= limit <= 10000 or not 1 <= timeout <= 3600:
        raise CatabolicError("import limit must be 1–10000 and timeout 1–3600 seconds")
    if target == "immich":
        destination = http_base(destination).rstrip("/")
    else:
        path = Path(destination).absolute()
        app._validate_binding_path("output", "import", path)
        fd = open_directory(path if path.exists() else path.parent)
        try:
            app._guard_default_parent(fd)
        finally:
            os.close(fd)
        destination = str(path)
    content = document["content"]
    items = {item["id"]: item for item in content["items"]}
    files = {file["id"]: file for file in content["files"]}
    kinds = ("photo", "video") if target == "immich" else ("book", "book_edition")
    decisions = {}
    for association in content["associations"]:
        item = items[association["item_id"]]
        if association["role"] != "primary" or item["kind"] not in kinds:
            continue
        file = files[association["file_id"]]
        if (
            file["id"] in decisions
            and decisions[file["id"]]["item"]["id"] != item["id"]
        ):
            raise CatabolicError(
                "one import file has multiple media identities; select a smaller catalog"
            )
        decisions[file["id"]] = {"file": file, "item": item}
    if len(decisions) > limit:
        raise CatabolicError(
            "import exceeds limit; select a smaller query catalog or raise --limit explicitly"
        )
    actions = []
    for decision in decisions.values():
        file, item = decision["file"], decision["item"]
        snapshot = import_source(app, file)
        with validated_source(snapshot) as fd:
            snapshot["ctime_ns"] = os.fstat(fd).st_ctime_ns
        decision["snapshot"] = snapshot
        actions.append(
            {
                "file_id": file["id"],
                "item_id": item["id"],
                "source": str(Path(snapshot["root"]) / snapshot["path"]),
                "size": file["size"],
            }
        )
    executable = "immich" if target == "immich" else "calibredb"
    result = {
        "target": target,
        "destination": destination,
        "executable": executable,
        "actions": actions,
        "applied": False,
        "completed": [],
        "complete": True,
        "copies_media": True,
        "outcome": "not_started",
        "attempts": [],
    }
    groups = defaultdict(list)
    for decision in decisions.values():
        group = decision["file"]["id"] if target == "immich" else decision["item"]["id"]
        groups[group].append(decision)
    for group in groups.values():
        suffixes = [Path(value["file"]["path"]).suffix.lower() for value in group]
        if len(suffixes) != len(set(suffixes)):
            raise CatabolicError(
                "multiple copies of one book format; select one file per format"
            )
    if not apply or not actions:
        return result
    program = shutil.which(executable)
    if program is None:
        raise CatabolicError(
            f"install the official {executable} CLI before using --apply"
        )
    env = {
        key: value for key, value in os.environ.items() if not key.startswith("IMMICH_")
    }
    if target == "immich":
        key = os.environ.get("IMMICH_API_KEY")
        if not key:
            raise CatabolicError("set IMMICH_API_KEY before applying an Immich import")
        env.update(IMMICH_API_KEY=key, IMMICH_INSTANCE_URL=destination)
    # Stage one logical book or photo at a time. External tools see private copies.
    for group in groups.values():
        item = group[0]["item"]
        attempt = {
            "id": str(uuid4()),
            "file_ids": [value["file"]["id"] for value in group],
            "outcome": "failed_before_launch",
        }
        result["attempts"].append(attempt)

        def launched(attempt=attempt):
            attempt["outcome"] = "external_outcome_unknown"

        try:
            with tempfile.TemporaryDirectory(prefix="catabolic-import-") as temporary:
                for decision in group:
                    file = decision["file"]
                    stage = Path(temporary) / (
                        "asset" + Path(file["path"]).suffix.lower()
                    )
                    stage_file(app, file, stage, snapshot=decision["snapshot"])
                if target == "immich":
                    argv = [program, "upload", "--no-progress", str(stage)]
                else:
                    argv = [
                        program,
                        "add",
                        "--with-library",
                        destination,
                        "--one-book-per-directory",
                        "--title=" + str(item["metadata"].get("title") or item["id"]),
                        "--identifier=catabolic:"
                        + content["database_id"]
                        + ":"
                        + item["id"],
                    ]
                    if item["metadata"].get("author"):
                        argv.append("--authors=" + str(item["metadata"]["author"]))
                    argv.append(temporary)
                command_output(
                    argv,
                    env=env,
                    timeout=timeout,
                    maximum=8 * 1024 * 1024,
                    capture=False,
                    on_start=launched,
                )
                attempt["outcome"] = "completed"
                result["completed"].extend(value["file"]["id"] for value in group)
        except (OSError, CatabolicError, CommandFailure, KeyboardInterrupt) as exc:
            result.update(
                complete=False,
                applied=True
                if result["completed"]
                else (
                    None if attempt["outcome"] == "external_outcome_unknown" else False
                ),
                outcome=attempt["outcome"],
                safe_to_retry=attempt["outcome"] == "failed_before_launch"
                and not result["completed"],
                interrupted=isinstance(exc, KeyboardInterrupt),
                failed_files=[value["file"]["id"] for value in group],
                error="import interrupted; inspect the destination before retrying"
                if isinstance(exc, KeyboardInterrupt)
                else str(exc),
            )
            return result
    result["outcome"] = "completed"
    result["applied"] = True
    return result


def import_source(app, file):
    """Portable manifest fields must agree with the local recorded occurrence."""
    snapshot = occurrence(app.store, app.profile, file["id"])
    if any(
        str(snapshot.get(key)) != str(file.get(key))
        for key in ("location", "path", "status", "size", "mtime_ns")
    ):
        raise CatabolicError(
            "import source changed or disagrees with inventory; rescan before importing"
        )
    return snapshot


def stage_file(app, file, stage, *, snapshot=None):
    snapshot = snapshot if snapshot is not None else import_source(app, file)
    with validated_source(snapshot) as fd:
        with os.fdopen(os.dup(fd), "rb") as source, stage.open("xb") as output:
            shutil.copyfileobj(source, output)
