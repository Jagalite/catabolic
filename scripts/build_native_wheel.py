#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Build a companion wheel and both CLIs from the same source-content snapshot.

This stages and verifies local artifacts only. It never uploads or publishes.
Use the pinned Maturin interpreter and run qualify_native_wheel.py afterwards
from a clean installed environment outside this checkout.
Source fencing plus the recorded toolchain is local provenance, not a claim of
complete hermetic or independently reproducible builds.
"""

import argparse
import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import tomllib
import zipfile
from pathlib import Path


def source_digest(root):
    """Fence crate source content and local Cargo config across paired builds."""
    paths = [
        root / name
        for name in (
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            # catabolic-http embeds this frozen contract via include_str!.
            "src/catabolic/http/releases/1.7.0/openapi.json",
        )
    ]
    generated = {
        "crates/catabolic-python/python/catabolic_native/bin/catabolic-native",
        "crates/catabolic-python/python/catabolic_native/bin/catabolic-native.tmp",
        "crates/catabolic-python/python/catabolic_native/native-helper.json",
    }
    paths.extend(
        path
        for path in (root / "crates").rglob("*")
        if path.is_file()
        and "__pycache__" not in path.parts
        and "target" not in path.parts
        and str(path.relative_to(root)) not in generated
    )
    paths.extend(path for path in (root / ".cargo").rglob("*") if path.is_file())
    digest = hashlib.sha256()
    for path in sorted(set(paths)):
        if path.is_file():
            digest.update(str(path.relative_to(root)).encode() + b"\0")
            digest.update(hashlib.sha256(path.read_bytes()).digest())
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python", default=sys.executable)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    sidecar = args.out / "build-manifest.json"
    sidecar.unlink(missing_ok=True)
    root = Path(__file__).resolve().parents[1]
    # Resolving a venv's interpreter symlink would silently select its base
    # environment and lose the pinned build dependencies.
    python = str(Path(args.python).absolute())
    version = subprocess.check_output(
        [python, "-m", "maturin", "--version"], text=True
    ).strip()
    if version != "maturin 1.13.1":
        raise SystemExit("build requires exactly maturin 1.13.1")
    package = root / "crates/catabolic-python/python/catabolic_native"
    helper = package / "bin/catabolic-native"
    manifest = package / "native-helper.json"
    project = tomllib.loads((root / "Cargo.toml").read_text())
    release = project["workspace"]["package"]["version"]
    sources = source_digest(root)
    build_script_sha256 = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    provenance = {
        "scope": "local-source-content-and-toolchain; not a hermetic-build proof",
        "rustc": subprocess.check_output(["rustc", "-vV"], cwd=root, text=True).strip(),
        "cargo": subprocess.check_output(
            ["cargo", "--version"], cwd=root, text=True
        ).strip(),
        "maturin": version,
        "python": subprocess.check_output([python, "--version"], text=True).strip(),
        "python_executable": python,
        "build_environment_sha256": hashlib.sha256(
            json.dumps(
                {
                    key: value
                    for key, value in os.environ.items()
                    if key.startswith(("CARGO_", "RUST", "PYO3_", "MATURIN_"))
                    or key
                    in (
                        "CC",
                        "CXX",
                        "AR",
                        "CFLAGS",
                        "CXXFLAGS",
                        "LDFLAGS",
                        "MACOSX_DEPLOYMENT_TARGET",
                    )
                },
                sort_keys=True,
            ).encode()
        ).hexdigest(),
    }
    command = [
        "cargo",
        "build",
        "--locked",
        "--release",
        "-p",
        "catabolic-cli",
        "--bin",
        "catabolic-native",
        "--bin",
        "catabolic-legacy-native",
        "--message-format=json-render-diagnostics",
    ]
    build = subprocess.run(command, cwd=root, text=True, stdout=subprocess.PIPE)
    if build.returncode:
        raise SystemExit(build.returncode)
    executables = {"catabolic-native": [], "catabolic-legacy-native": []}
    for line in build.stdout.splitlines():
        message = json.loads(line)
        if (
            message.get("reason") == "compiler-artifact"
            and message["target"]["name"] in executables
            and message.get("executable")
        ):
            executables[message["target"]["name"]].append(Path(message["executable"]))
    if any(len(paths) != 1 for paths in executables.values()):
        raise SystemExit("Cargo did not identify exactly one native and one legacy CLI")
    binary = executables["catabolic-native"][0]
    legacy_binary = executables["catabolic-legacy-native"][0]
    legacy_digest = hashlib.sha256(legacy_binary.read_bytes()).hexdigest()
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    if binary.stat().st_size > 128 * 1024 * 1024:
        raise SystemExit("native helper exceeds its installed validation budget")
    helper.parent.mkdir(exist_ok=True)
    # Staged files are ignored by Git but explicitly included in the wheel.
    # An interrupted build cannot create a successful artifact: archive bytes
    # and execute bits are checked before anything reaches the requested output.
    temporary = helper.with_suffix(".tmp")
    shutil.copyfile(binary, temporary)
    temporary.chmod(0o755)
    os.replace(temporary, helper)
    metadata = {
        "format": 1,
        "version": release,
        "path": "bin/catabolic-native",
        "sha256": digest,
        "source_sha256": sources,
        "build": provenance,
    }
    manifest.write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
    with tempfile.TemporaryDirectory(prefix="catabolic-wheel-") as directory:
        subprocess.run(
            [
                python,
                "-m",
                "maturin",
                "build",
                "--locked",
                "--release",
                "--manifest-path",
                "crates/catabolic-python/Cargo.toml",
                "--interpreter",
                python,
                "--out",
                directory,
            ],
            cwd=root,
            check=True,
        )
        wheels = list(Path(directory).glob("*.whl"))
        if (
            source_digest(root) != sources
            or hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
            != build_script_sha256
        ):
            raise SystemExit(
                "crate sources changed during the paired helper/extension build; rebuild a stable snapshot"
            )
        if len(wheels) != 1:
            raise SystemExit("build did not produce exactly one wheel")
        wheel = wheels[0]
        with zipfile.ZipFile(wheel) as archive:
            if "catabolic_native/bin/catabolic-legacy-native" in archive.namelist():
                raise SystemExit("release wheel must not package the legacy CLI")
            member = archive.getinfo("catabolic_native/bin/catabolic-native")
            if hashlib.sha256(archive.read(member)).hexdigest() != digest:
                raise SystemExit("wheel native helper differs from the Cargo artifact")
            if not stat.S_IMODE(member.external_attr >> 16) & 0o111:
                raise SystemExit("wheel native helper lost executable permissions")
            if (
                json.loads(archive.read("catabolic_native/native-helper.json"))
                != metadata
            ):
                raise SystemExit(
                    "wheel helper manifest differs from the staged artifact"
                )
            if (
                "catabolic_native:main"
                not in archive.read(
                    f"catabolic_native-{release}.dist-info/entry_points.txt"
                ).decode()
            ):
                raise SystemExit("wheel native launcher entry point is missing")
            extensions = [
                name
                for name in archive.namelist()
                if name.startswith("catabolic_native/_native.")
                and name.endswith((".so", ".pyd"))
            ]
            if len(extensions) != 1:
                raise SystemExit("wheel must contain exactly one native extension")
            extension_digest = hashlib.sha256(archive.read(extensions[0])).hexdigest()
        destination = args.out / wheel.name
        shutil.copyfile(wheel, destination)
        for name, artifact, expected in (
            ("catabolic-native", binary, digest),
            ("catabolic-legacy-native", legacy_binary, legacy_digest),
        ):
            target = args.out / name
            shutil.copyfile(artifact, target)
            target.chmod(0o755)
            if hashlib.sha256(target.read_bytes()).hexdigest() != expected:
                raise SystemExit("CLI artifact changed during the paired build")
        if (
            source_digest(root) != sources
            or hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
            != build_script_sha256
        ):
            raise SystemExit("source or build script changed before artifact handoff")
        wheel_digest = hashlib.sha256(destination.read_bytes()).hexdigest()
        build_manifest = {
            "format": 1,
            "profile": "release",
            "source_sha256": sources,
            "source_sha256_after": sources,
            "wheel": destination.name,
            "wheel_sha256": wheel_digest,
            "extension_sha256": extension_digest,
            "native_binary": "catabolic-native",
            "native_binary_sha256": digest,
            "legacy_binary": "catabolic-legacy-native",
            "legacy_binary_sha256": legacy_digest,
            "build_script_sha256": build_script_sha256,
            "build": provenance,
            "publication": "local-only",
            "scope": "Local source-content/toolchain provenance; not hostile-directory attestation or hermetic reproduction",
        }
        # The success sidecar is emitted last; an interrupted build has no new receipt.
        sidecar.write_text(
            json.dumps(build_manifest, indent=2) + "\n", encoding="utf-8"
        )
        print(
            json.dumps(
                {
                    "wheel": str(destination),
                    "wheel_sha256": hashlib.sha256(
                        destination.read_bytes()
                    ).hexdigest(),
                    "native_helper_sha256": digest,
                    "native_helper_bytes": binary.stat().st_size,
                    "publication": "local-only",
                    "legacy_binary_sha256": legacy_digest,
                    "extension_sha256": extension_digest,
                    "build_manifest": str(sidecar),
                    "build_manifest_sha256": hashlib.sha256(
                        sidecar.read_bytes()
                    ).hexdigest(),
                },
                sort_keys=True,
            )
        )


if __name__ == "__main__":
    main()
