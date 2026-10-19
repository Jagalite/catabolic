# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Shared local provenance fences for the selected public facade wheel gates.

These checks bind source content and exact artifacts in a trusted local build
workflow. They do not attest a hostile directory or hermetic reproduction.
"""

import argparse
import hashlib
import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path

FROZEN_COMMIT = "453fca983222c6665a48775eef67c489c96b527e"
REFERENCE_VERSION = [3, 14, 6]


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def sibling(name):
    spec = importlib.util.spec_from_file_location(
        name, Path(__file__).with_name(name + ".py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def clear_stale_report(argv=None, parser=None):
    """Invalidate every canonical --report destination before fallible preflight.

    Only --report PATH and --report=PATH opt in; abbreviations are rejected by
    the full parser. Stop at -- and use that parser's option lexer to distinguish
    filename values (including version-specific negative numbers) from options.
    No argv parsing or project imports happen here. Bootstrap copies are checked
    against this implementation by the receipt-boundary tests.
    """
    if parser is None:
        parser = argparse.ArgumentParser(add_help=False, allow_abbrev=False)
        parser.add_argument("--report")
    arguments = sys.argv[1:] if argv is None else argv
    first_error = None
    for index, argument in enumerate(arguments):
        if argument == "--":
            break
        if argument == "--report":
            value = arguments[index + 1] if index + 1 < len(arguments) else ""
            if parser._parse_optional(value) is not None:
                continue
        elif argument.startswith("--report="):
            value = argument.partition("=")[2]
        else:
            continue
        if value:
            report = Path(value)
            for suffix in (None, ".attempt.json"):
                try:
                    target = report if suffix is None else report.with_suffix(suffix)
                    target.unlink(missing_ok=True)
                except (OSError, ValueError) as error:
                    if first_error is None:
                        first_error = error
    if first_error is not None:
        raise first_error


def inventory_rows(inventory, contracts, expected_count):
    assert inventory["reference_commit"] == FROZEN_COMMIT, "wrong frozen oracle commit"
    rows = [row for row in inventory["rows"] if row["id"] in contracts]
    assert len(rows) == len(contracts) == expected_count, (
        "wrong selected inventory count"
    )
    assert len({row["id"] for row in rows}) == expected_count, (
        "duplicate inventory rows"
    )
    return rows


def paired_groups(reference, native, expected, expected_count):
    assert len(expected) == len(set(expected)) == expected_count
    assert set(reference) == set(native) == set(expected), "wrong fixed paired groups"
    assert reference == native, json.dumps(
        {
            key: [reference[key], native[key]]
            for key in expected
            if reference[key] != native[key]
        },
        indent=2,
    )


def reference_runtime(python):
    # Preserve a venv's interpreter path rather than resolving its symlink.
    executable = str(Path(python).absolute())
    completed = subprocess.run(
        [
            executable,
            "-I",
            "-B",
            "-c",
            "import json,sys; print(json.dumps(dict(version=list(sys.version_info[:3]),"
            "executable=sys.executable,prefix=sys.prefix,base_prefix=sys.base_prefix)))",
        ],
        capture_output=True,
        text=True,
        check=True,
        timeout=15,
    )
    identity = json.loads(completed.stdout)
    assert identity["version"] == REFERENCE_VERSION, "oracle requires Python 3.14.6"
    assert Path(identity["executable"]).absolute() == Path(executable), (
        "oracle interpreter identity changed"
    )
    return {**identity, "executable_sha256": digest(executable)}


class NativeIdentity:
    def __init__(self, source_root):
        import catabolic_native as native

        self.native = native
        self.extension = native._native
        self.package = Path(native.__file__).resolve()
        self.path = Path(self.extension.__file__).resolve()
        assert not self.package.is_relative_to(Path(source_root).resolve()), (
            "installed package must be outside the checkout"
        )
        assert not Path.cwd().resolve().is_relative_to(Path(source_root).resolve()), (
            "run installed qualification outside the checkout"
        )

    def verify(self):
        assert sys.modules["catabolic_native"] is self.native, (
            "package identity changed"
        )
        assert Path(self.native.__file__).resolve() == self.package, (
            "package path changed"
        )
        assert self.native._native is self.extension, "extension identity changed"
        assert Path(self.extension.__file__).resolve() == self.path, (
            "extension path changed"
        )


def strict_artifact(
    wheel, manifest_path, source_root, *, native_binary=None, legacy_binary=None
):
    """Require the whole installed release wheel and same-build external CLIs."""
    import catabolic_native as native
    from catabolic_native.cli import legacy_binary as packaged_legacy_binary

    installed = sibling("qualify_native_wheel").verify_installed_wheel(wheel)
    manifest = json.loads(Path(manifest_path).read_text())
    source = sibling("build_native_wheel").source_digest(source_root)
    package = Path(native.__file__).resolve().parent
    helpers = {
        "native_binary": (Path(native.native_binary()).resolve(), "native-helper.json"),
        "legacy_binary": (
            Path(packaged_legacy_binary()).resolve(),
            "legacy-helper.json",
        ),
    }
    for name, (helper, _) in helpers.items():
        binary_name = (
            "catabolic-native" if name == "native_binary" else "catabolic-legacy-native"
        )
        assert helper == package / "bin" / binary_name, (
            "not the packaged helper",
            name,
        )
        assert helper.is_file() and os.access(helper, os.X_OK), (
            "packaged helper is not executable",
            name,
        )
    expected = {
        "format": 1,
        "profile": "release",
        "source_sha256": source,
        "source_sha256_after": source,
        "wheel": Path(wheel).name,
        "wheel_sha256": digest(wheel),
        "extension_sha256": installed["imported_extension_sha256"],
        "native_binary": "catabolic-native",
        "native_binary_sha256": digest(helpers["native_binary"][0]),
        "legacy_binary": "catabolic-legacy-native",
        "legacy_binary_sha256": digest(helpers["legacy_binary"][0]),
        "build_script_sha256": digest(
            Path(__file__).with_name("build_native_wheel.py")
        ),
        "publication": "local-only",
    }
    for key, value in expected.items():
        assert manifest.get(key) == value, ("release build identity mismatch", key)
    for name, (_helper, manifest_name) in helpers.items():
        metadata = json.loads((package / manifest_name).read_text())
        assert metadata == {
            "format": 1,
            "version": native.__version__,
            "path": "bin/" + expected[name],
            "sha256": expected[name + "_sha256"],
            "source_sha256": source,
            "build": manifest.get("build"),
        }, ("packaged helper provenance differs from the release build", name)
    provenance = manifest["build"]
    assert isinstance(provenance, dict)
    for key in ("scope", "rustc", "cargo", "maturin", "python", "python_executable"):
        assert isinstance(provenance.get(key), str) and provenance[key], (
            "missing build provenance",
            key,
        )
    assert provenance["maturin"] == "maturin 1.13.1"
    environment = provenance.get("build_environment_sha256", "")
    assert len(environment) == 64 and all(
        c in "0123456789abcdef" for c in environment
    ), "missing build environment identity"
    binaries = {}
    for name, supplied in (
        ("native_binary", native_binary),
        ("legacy_binary", legacy_binary),
    ):
        path = (
            Path(supplied) if supplied else Path(manifest_path).parent / manifest[name]
        )
        assert path.is_file() and os.access(path, os.X_OK), ("missing executable", name)
        assert digest(path) == manifest.get(name + "_sha256"), (
            "external CLI differs from the same-source release build",
            name,
        )
        binaries[name] = str(path.absolute())
    return {
        "wheel_sha256": expected["wheel_sha256"],
        "installed_wheel": installed,
        "build": manifest,
        "build_manifest_sha256": digest(manifest_path),
        "binaries": binaries,
    }


def file_fence(script, inventory, build_manifest):
    names = (
        Path(script).name,
        "native_facade_qualification.py",
        "native_qualification_results.py",
        "qualify_native_embedding.py",
        "qualify_native_wheel.py",
        "build_native_wheel.py",
    )
    return {
        "qualification_scripts_sha256": {
            name: digest(Path(script).with_name(name)) for name in names
        },
        "inventory_sha256": digest(inventory),
        "build_manifest_sha256": digest(build_manifest),
    }


def reference_cli():
    """Run each legacy CLI oracle with isolated, verified frozen imports."""
    if not __debug__ or os.environ.get("PYTHONOPTIMIZE"):
        raise SystemExit("reference CLI qualification requires assertions")
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--oracle-manifest", type=Path, required=True)
    parser.add_argument("--oracle-sha256", required=True)
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    assert list(sys.version_info[:3]) == REFERENCE_VERSION
    provenance = sibling("qualify_native_embedding")
    assert digest(args.oracle_manifest) == args.oracle_sha256, (
        "CLI oracle snapshot changed"
    )
    oracle = json.loads(args.oracle_manifest.read_text())["oracle"]
    assert oracle["head"] == FROZEN_COMMIT
    assert Path(oracle["root"]).resolve() == args.reference_root.resolve()
    assert args.arguments and args.arguments[0] == "--"
    sys.path.insert(0, str(args.reference_root / "src"))
    from catabolic import app, cli, store  # noqa: F401

    provenance.reference_imports(args.reference_root, oracle["tracked_source_sha256"])
    sys.argv = ["catabolic", *args.arguments[1:]]
    try:
        raise SystemExit(cli.main())
    finally:
        provenance.reference_imports(
            args.reference_root, oracle["tracked_source_sha256"]
        )
        assert digest(args.oracle_manifest) == args.oracle_sha256, (
            "CLI oracle snapshot changed"
        )


if __name__ == "__main__":
    reference_cli()
