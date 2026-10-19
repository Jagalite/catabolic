#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Qualify compatibility forwarding on exact installed native bytes.

The single ``catabolic`` distribution ships the native extension and the thin
compatibility namespace. This entrypoint never builds or installs. It refuses a
legacy, split or foreign catabolic installation and any preloaded namespace
before importing a provider or running a check. It reuses
native_facade_qualification.strict_artifact for the installed wheel, extension,
paired helpers, release build manifest and source fence, requires the installed
compatibility files to equal the checked-in source, and compares the installed
forwarding identities. Success is forwarding evidence, not behavior parity.
Run outside the source tree with Python -I -B, without -O.
"""

import argparse
import hashlib
import importlib
import importlib.metadata
import importlib.util
import inspect
import json
import re
import sys
from pathlib import Path

PROJECT = Path("crates/catabolic-python")
EXPECTED_ROWS = 159
# Frozen public names/signatures, not a replacement artifact or helper manifest.
EXPECTED_PRESERVED_SHA256 = (
    "068564e2e3050b157450c79c2dc574bafdf90c3b4b55d6119680edc3592b29f2"
)


def refuse_existing_namespace():
    """Only the single native distribution may own the catabolic namespace."""
    owners = []
    for distribution in importlib.metadata.distributions():
        name = re.sub(r"[-_.]+", "-", distribution.metadata.get("Name", "")).lower()
        if name in {"catabolic-native", "catabolic-native-compat"}:
            raise RuntimeError(
                "compatibility qualification requires a clean environment: "
                f"remove the obsolete {name} distribution from this qualification flow"
            )
        if name == "catabolic":
            owners.append(distribution)
    if len(owners) != 1:
        raise RuntimeError(
            "compatibility qualification requires a clean environment with exactly "
            "one catabolic distribution"
        )
    files = {str(entry) for entry in owners[0].files or ()}
    if not {"catabolic/__init__.py", "catabolic_native/__init__.py"} <= files:
        raise RuntimeError(
            "the installed catabolic distribution is not the native single "
            "distribution; refusing mixed installation"
        )
    if any(
        name == "catabolic" or name.startswith("catabolic.") for name in sys.modules
    ):
        raise RuntimeError(
            "catabolic was already imported; use a fresh clean interpreter"
        )
    return owners[0]


def load_shared(source_root):
    path = source_root / "scripts/native_facade_qualification.py"
    spec = importlib.util.spec_from_file_location("native_facade_qualification", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def select_rows(inventory, shared):
    identifiers = {
        row["id"]
        for row in inventory["rows"]
        if row["classification"].startswith("preserve-")
    }
    rows = shared.inventory_rows(inventory, identifiers, EXPECTED_ROWS)
    boundary = [
        {key: row[key] for key in ("id", "classification", "source", "signature")}
        for row in rows
    ]
    digest = hashlib.sha256(
        json.dumps(
            sorted(boundary, key=lambda row: row["id"]),
            sort_keys=True,
            separators=(",", ":"),
        ).encode()
    ).hexdigest()
    if digest != EXPECTED_PRESERVED_SHA256:
        raise RuntimeError("preserved public inventory contract changed")
    return rows


def module_name(row):
    parts = Path(row["source"]).relative_to("src").with_suffix("").parts
    return ".".join(parts).removesuffix(".__init__")


def static_attribute(owner, suffix):
    for name in suffix:
        owner = inspect.getattr_static(owner, name)
    return owner


def verify_forwarding(installed_root, rows):
    """Compare object/descriptor identities; never invoke an engine operation."""
    installed_root = installed_root.resolve()
    result = []
    for row in rows:
        module = module_name(row)
        proxy = importlib.import_module(module)
        if not Path(proxy.__file__).resolve().is_relative_to(installed_root):
            raise RuntimeError(f"wrong compatibility module origin: {module}")
        native = importlib.import_module(
            module.replace("catabolic", "catabolic_native", 1)
        )
        identifier = row["id"].removeprefix("python:")
        suffix = identifier.removeprefix(module).removeprefix(".")
        if not suffix:
            # Modules are intentionally distinct proxies. Public values and
            # callable/class identities are checked by their own inventory rows.
            status = "module_proxy_present"
            if module == "catabolic" and proxy.__version__ != native.__version__:
                raise RuntimeError("root version forwarding differs")
        else:
            if static_attribute(proxy, suffix.split(".")) is not static_attribute(
                native, suffix.split(".")
            ):
                raise RuntimeError(f"forwarded object identity differs: {identifier}")
            status = "native_object_identity"
        result.append({"id": row["id"], "status": status})
    return result


def verify_installed_source(distribution, source_package):
    """The installed compatibility files must be the checked-in source bytes."""
    root = Path(distribution.locate_file("")).resolve()
    installed = {
        Path(entry).relative_to("catabolic"): (root / entry)
        for entry in distribution.files or ()
        if Path(entry).parts[0] == "catabolic"
        and "__pycache__" not in Path(entry).parts
    }
    source = {
        path.relative_to(source_package): path
        for path in source_package.rglob("*")
        if path.is_file() and "__pycache__" not in path.parts
    }
    if set(installed) != set(source):
        raise RuntimeError("installed compatibility file set differs from source")
    for relative, path in source.items():
        if (
            hashlib.sha256(installed[relative].read_bytes()).digest()
            != hashlib.sha256(path.read_bytes()).digest()
        ):
            raise RuntimeError(f"installed compatibility file differs: {relative}")
    return root


def qualify(source_root, wheel, build_manifest):
    if not __debug__:
        raise RuntimeError("qualification requires enabled Python assertions")
    # This must remain before loading shared helpers, importing catabolic_native,
    # changing sys.path or doing any candidate scenario. No installer exists here.
    distribution = refuse_existing_namespace()
    source_root = Path(source_root).resolve()
    shared = load_shared(source_root)
    inventory_path = source_root / "tests/parity/python-api.json"
    inventory = json.loads(inventory_path.read_text())
    rows = select_rows(inventory, shared)
    fence = {
        "script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "inventory_sha256": hashlib.sha256(inventory_path.read_bytes()).hexdigest(),
    }
    before = shared.strict_artifact(wheel, build_manifest, source_root)
    identity = shared.NativeIdentity(source_root)
    installed_root = verify_installed_source(
        distribution, source_root / PROJECT / "python/catabolic"
    )
    results = verify_forwarding(installed_root, rows)
    identity.verify()
    after = shared.strict_artifact(wheel, build_manifest, source_root)
    if before != after or fence != {
        "script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "inventory_sha256": hashlib.sha256(inventory_path.read_bytes()).hexdigest(),
    }:
        raise RuntimeError("compatibility qualification inputs changed")
    return {
        "format": 1,
        "scope": "installed compatibility namespace forwarding on exact installed native artifacts",
        "forwarding_passed": True,
        "installed_compatibility_wheel_qualified": True,
        "behavioral_parity_qualified": False,
        "defaults_changed": False,
        "engine_operations_executed": 0,
        "native_artifacts": before,
        **fence,
        "rows": results,
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument("--source-root", type=Path, required=True)
    parser.add_argument("--wheel", type=Path, required=True)
    parser.add_argument("--build-manifest", type=Path, required=True)
    args = parser.parse_args(argv)
    if not __debug__ or not sys.flags.isolated or not sys.dont_write_bytecode:
        raise RuntimeError("qualification requires Python -I -B and enabled assertions")
    if (
        Path(__file__).resolve()
        != args.source_root.resolve() / "scripts/qualify_native_compatibility.py"
    ):
        raise RuntimeError(
            "qualification script must belong to the supplied source root"
        )
    print(
        json.dumps(qualify(args.source_root, args.wheel, args.build_manifest), indent=2)
    )


if __name__ == "__main__":
    main()
