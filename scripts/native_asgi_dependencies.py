#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Offline, lock-bound wheel provenance for the paired ASGI fixture dependencies.

Export only already-cached wheels matching the installed pinned dependencies.
This tool never downloads packages or invokes an installer. The exported exact
wheel paths can be passed to pip --isolated --disable-pip-version-check install
--no-index --no-deps. No source distribution or ambient source import is accepted.
"""

import argparse
import email
import hashlib
import importlib.metadata
import json
import re
import shutil
import zipfile
from pathlib import Path


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def canonical(name):
    return re.sub(r"[-_.]+", "-", name).lower()


def locked_dependencies(root):
    result = {}
    for name in ("runtime.lock", "http-test.lock"):
        text = (root / "requirements" / name).read_text().replace("\\\n", " ")
        for raw in text.splitlines():
            line = raw.strip()
            if not line or line.startswith("#") or line == "-r runtime.lock":
                continue
            match = re.fullmatch(
                r"([\w-]+)==([\w.]+)\s+((?:--hash=sha256:[a-f0-9]{64}\s*)+)", line
            )
            if not match:
                raise ValueError(f"unsupported pinned dependency line: {name}: {line}")
            package, version, hashes = match.groups()
            package = canonical(package)
            if package in result:
                raise ValueError(f"duplicate pinned dependency: {package}")
            result[package] = {
                "version": version,
                "sha256": re.findall(r"sha256:([a-f0-9]{64})", hashes),
            }
    if len(result) != 18:
        raise ValueError("expected exactly 18 pinned ASGI dependency distributions")
    return result


def wheel_metadata(path):
    with zipfile.ZipFile(path) as archive:
        names = archive.namelist()
        if len(names) != len(set(names)):
            raise ValueError("duplicate dependency wheel members")
        metadata = [name for name in names if name.endswith(".dist-info/METADATA")]
        wheels = [name for name in names if name.endswith(".dist-info/WHEEL")]
        if len(metadata) != 1 or len(wheels) != 1:
            raise ValueError("missing or ambiguous dependency wheel metadata")
        info = email.message_from_bytes(archive.read(metadata[0]))
        tags = email.message_from_bytes(archive.read(wheels[0])).get_all("Tag")
        if not tags or any(len(tag.split("-")) != 3 for tag in tags):
            raise ValueError("invalid dependency wheel tags")
        parts = [
            ".".join(sorted({tag.split("-")[i] for tag in tags})) for i in range(3)
        ]
        filename = (
            metadata[0].split("/")[0].removesuffix(".dist-info")
            + "-"
            + "-".join(parts)
            + ".whl"
        )
        return canonical(info["Name"]), info["Version"], filename


def verify_dependency_wheel(path, name, version):
    """Compare every archive payload to its installed file, without importing it."""
    actual_name, actual_version, _ = wheel_metadata(path)
    if (actual_name, actual_version) != (name, version):
        raise ValueError("dependency wheel name/version differs from lock")
    distribution = importlib.metadata.distribution(name)
    if distribution.version != version:
        raise ValueError(f"installed dependency version differs: {name}")
    root = Path(distribution.locate_file("")).resolve()
    generated = {"RECORD", "INSTALLER", "REQUESTED", "direct_url.json"}
    recorded = set()
    for entry in distribution.files or ():
        relative = Path(entry)
        if "__pycache__" in relative.parts:
            continue
        if relative.parts[0].endswith(".dist-info") and relative.name in generated:
            continue
        # pip-generated entrypoint launchers are not library payloads.
        if (
            ".." in relative.parts
            and relative.parts[:2] == ("..", "..")
            and "bin" in relative.parts
        ):
            continue
        if relative.is_absolute() or ".." in relative.parts:
            raise ValueError(f"unexpected relocated dependency payload: {entry}")
        recorded.add(str(relative))
    hashes = {}
    with zipfile.ZipFile(path) as archive:
        members = archive.infolist()
        if (
            len(members) > 20000
            or sum(m.file_size for m in members) > 128 * 1024 * 1024
        ):
            raise ValueError("dependency wheel exceeds payload budget")
        for member in members:
            relative = Path(member.filename)
            if member.is_dir():
                continue
            if (
                relative.is_absolute()
                or ".." in relative.parts
                or any(part.endswith(".data") for part in relative.parts)
            ):
                raise ValueError("unsupported dependency wheel path")
            if "__pycache__" in relative.parts or relative.suffix in {".pyc", ".pyo"}:
                raise ValueError("dependency wheel contains bytecode")
            if member.filename.endswith(".dist-info/RECORD"):
                continue
            target = root / relative
            if target.is_symlink() or not target.resolve().is_relative_to(root):
                raise ValueError("linked or escaped dependency payload")
            expected = hashlib.sha256(archive.read(member)).hexdigest()
            if not target.is_file() or digest(target) != expected:
                raise ValueError(
                    f"installed dependency bytes differ: {name}: {relative}"
                )
            hashes[str(relative)] = expected
    if recorded != set(hashes):
        raise ValueError(
            f"dependency wheel/installed payload inventories differ: {name}"
        )
    # Reject unrecorded package additions too, including ignored source files.
    tops = {
        Path(name).parts[0]
        for name in hashes
        if not Path(name).parts[0].endswith(".dist-info")
    }
    for top in tops:
        package = root / top
        paths = package.rglob("*") if package.is_dir() else [package]
        for target in paths:
            if target.is_symlink():
                raise ValueError("dependency package contains a symlink")
            if (
                target.is_file()
                and "__pycache__" not in target.parts
                and str(target.relative_to(root)) not in hashes
            ):
                raise ValueError(f"unrecorded dependency package payload: {target}")
    return {
        "root": str(root),
        "version": version,
        "wheel": str(path.resolve()),
        "wheel_sha256": digest(path),
        "payloads_sha256": hashes,
    }


def dependency_snapshot(root, wheelhouse):
    locked = locked_dependencies(root)
    selected = {}
    for path in sorted(wheelhouse.glob("*.whl")):
        name, version, _ = wheel_metadata(path)
        if (
            name not in locked
            or version != locked[name]["version"]
            or digest(path) not in locked[name]["sha256"]
        ):
            raise ValueError(f"unlocked dependency wheel: {path.name}")
        if name in selected:
            raise ValueError(f"duplicate dependency wheel: {name}")
        selected[name] = verify_dependency_wheel(path, name, version)
    if set(selected) != set(locked):
        raise ValueError(
            f"missing pinned dependency wheels: {sorted(set(locked) - set(selected))}"
        )
    return selected


def export_cached(root, cache, destination):
    locked = locked_dependencies(root)
    wanted = {value: name for name, item in locked.items() for value in item["sha256"]}
    selected = {}
    # The pip HTTP cache keeps downloaded wheels as .body; the wheel cache uses .whl.
    for path in sorted(cache.rglob("*")):
        if not path.is_file() or path.suffix not in {".body", ".whl"}:
            continue
        hashed = digest(path)
        name = wanted.get(hashed)
        if name is None or name in selected:
            continue
        try:
            verify_dependency_wheel(path, name, locked[name]["version"])
        except (ValueError, FileNotFoundError):
            continue  # Another platform's locked wheel does not match this install.
        selected[name] = path
    if set(selected) != set(locked):
        raise ValueError(
            f"local cache lacks exact installed locked wheels: {sorted(set(locked) - set(selected))}"
        )
    destination.mkdir(parents=True, exist_ok=True)
    if list(destination.iterdir()):
        raise ValueError("dependency export destination must be empty")
    for _name, source in sorted(selected.items()):
        _, _, filename = wheel_metadata(source)
        shutil.copyfile(source, destination / filename)
    return dependency_snapshot(root, destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument("--source-root", type=Path, required=True)
    parser.add_argument("--cache", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    print(
        json.dumps(
            export_cached(
                args.source_root.resolve(), args.cache.resolve(), args.out.resolve()
            ),
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
