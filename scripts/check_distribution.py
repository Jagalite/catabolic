# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Validate release versions and package contents without importing the application."""

import argparse
import ast
import hashlib
import json
import tarfile
import tomllib
import zipfile
from email.parser import BytesParser
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def check_version(root, ref="", require_tag=False):
    project = tomllib.loads((root / "pyproject.toml").read_text())["project"]
    version = project["version"]
    tree = ast.parse((root / "src/catabolic/__init__.py").read_text())
    versions = [
        ast.literal_eval(node.value)
        for node in tree.body
        if isinstance(node, ast.Assign)
        and any(isinstance(t, ast.Name) and t.id == "__version__" for t in node.targets)
    ]
    if versions != [version]:
        raise ValueError("pyproject.toml and catabolic.__version__ must agree")
    if require_tag or ref.startswith("refs/tags/"):
        if ref != f"refs/tags/v{version}":
            raise ValueError(
                f"release must use the exact tag v{version}; received {ref!r}"
            )
    return project


def check_distribution(root, dist, ref="", require_tag=False):
    project = check_version(root, ref, require_tag)
    version = project["version"]
    stem = f"catabolic-{version}"
    expected = {f"{stem}-py3-none-any.whl", f"{stem}.tar.gz"}
    actual = {p.name for p in dist.iterdir()}
    if actual != expected:
        raise ValueError(f"expected only {sorted(expected)}; found {sorted(actual)}")
    package = root / "src/catabolic"
    package_files = [
        p
        for p in sorted(package.rglob("*"))
        if p.is_file() and p.suffix in {".py", ".sql", ".json", ".md", ".license"}
    ]

    def metadata(raw):
        fields = BytesParser().parsebytes(raw)
        expected_fields = {
            "Name": project["name"],
            "Version": version,
            "Requires-Python": project["requires-python"],
            "License-Expression": project["license"],
            "License-File": "LICENSE",
        }
        for key, value in expected_fields.items():
            if fields[key] != value:
                raise ValueError(f"incorrect package metadata {key}: {fields[key]!r}")

    def same(raw, source):
        if raw != source.read_bytes():
            raise ValueError(f"packaged content differs from source: {source}")

    with zipfile.ZipFile(dist / f"{stem}-py3-none-any.whl") as wheel:
        metadata(wheel.read(f"{stem}.dist-info/METADATA"))
        same(wheel.read(f"{stem}.dist-info/licenses/LICENSE"), root / "LICENSE")
        for source in package_files:
            same(wheel.read(source.relative_to(root / "src").as_posix()), source)
    with tarfile.open(dist / f"{stem}.tar.gz") as source_archive:
        metadata(source_archive.extractfile(f"{stem}/PKG-INFO").read())
        sources = package_files + sorted((root / "docs").glob("*.md"))
        sources += [root / name for name in ("README.md", "LICENSE", "pyproject.toml")]
        for source in sources:
            name = f"{stem}/{source.relative_to(root).as_posix()}"
            member = source_archive.getmember(name)
            if not member.isfile():
                raise ValueError(f"source archive entry is not a regular file: {name}")
            same(source_archive.extractfile(member).read(), source)
    return {
        "version": version,
        "ref": ref,
        "package_files_checked": len(package_files),
        "files": [
            {
                "name": name,
                "sha256": hashlib.sha256((dist / name).read_bytes()).hexdigest(),
            }
            for name in sorted(expected)
        ],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dist", type=Path, default=Path("dist"))
    parser.add_argument("--ref", default="")
    parser.add_argument("--require-tag", action="store_true")
    parser.add_argument("--version-only", action="store_true")
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    if args.version_only:
        result = {"version": check_version(ROOT, args.ref, args.require_tag)["version"]}
    else:
        result = check_distribution(ROOT, args.dist, args.ref, args.require_tag)
    output = json.dumps(result, indent=2) + "\n"
    if args.report:
        args.report.write_text(output)
    print(output, end="")


if __name__ == "__main__":
    main()
