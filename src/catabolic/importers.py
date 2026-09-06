"""Explicit, bounded import adapters using official application CLIs."""

import os
import shutil
import stat
import subprocess
import tempfile
from collections import defaultdict
from pathlib import Path

from .domain import CatabolicError
from .exports import http_base
from .filesystem import open_directory, parent_handle, root_handle, source_stat


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
        with root_handle(app.binding("source", file["location"])) as root:
            status = source_stat(root, file["path"])
        if (
            file["status"] != "present"
            or str(status.st_size) != file["size"]
            or str(status.st_mtime_ns) != file["mtime_ns"]
        ):
            raise CatabolicError(
                "import source changed or is unobserved; rescan before importing"
            )
        actions.append(
            {
                "file_id": file["id"],
                "item_id": item["id"],
                "source": file["source_path"],
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
        try:
            with tempfile.TemporaryDirectory(prefix="catabolic-import-") as temporary:
                for decision in group:
                    file = decision["file"]
                    stage = Path(temporary) / (
                        "asset" + Path(file["path"]).suffix.lower()
                    )
                    stage_file(app, file, stage)
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
                process = subprocess.run(
                    argv,
                    env=env,
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    timeout=timeout,
                    check=False,
                )
                if process.returncode:
                    raise CatabolicError(
                        f"{executable} exited with status {process.returncode}"
                    )
                result["completed"].extend(value["file"]["id"] for value in group)
        except (OSError, CatabolicError, subprocess.TimeoutExpired) as exc:
            result.update(
                complete=False,
                applied=bool(result["completed"]),
                failed_files=[value["file"]["id"] for value in group],
                error=str(exc),
            )
            return result
    result["applied"] = True
    return result


def stage_file(app, file, stage):
    with root_handle(app.binding("source", file["location"])) as root:
        with parent_handle(root, file["path"]) as (parent, leaf):
            fd = os.open(
                leaf, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent
            )
            with os.fdopen(fd, "rb") as source:
                before = os.fstat(source.fileno())
                if (
                    not stat.S_ISREG(before.st_mode)
                    or str(before.st_size) != file["size"]
                    or str(before.st_mtime_ns) != file["mtime_ns"]
                ):
                    raise CatabolicError("import source changed after preview")
                with stage.open("xb") as output:
                    shutil.copyfileobj(source, output)
                after = os.fstat(source.fileno())
                if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (
                    after.st_size,
                    after.st_mtime_ns,
                    after.st_ctime_ns,
                ):
                    raise CatabolicError("import source changed while staging")
