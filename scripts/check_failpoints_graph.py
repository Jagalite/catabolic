# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Fail if a shipped crate's normal feature graph enables catabolic-store failpoints.

catabolic-store's ``failpoints`` feature adds test-only observation hooks to
native restore, native backup and journal conversion, and accepts the
``failpoint``/``crash`` request fields. Test builds enable it through
catabolic-cli's dev-dependency, so the workspace test build has it on. The
shipped binaries and the wheel must not. This inspects the normal and
build-dependency feature graph offline with ``cargo tree``, which resolves
metadata without compiling anything, so it does not depend on how a test build
unifies features.

catabolic-python is resolved with exactly the feature options maturin adds
from ``[tool.maturin]`` in its pyproject.toml, since maturin merges them into
the extension's Cargo build. Maturin settings this check cannot reproduce fail.

Fails closed: a cargo error, empty or malformed output, a graph rooted at the
wrong package, or a missing catabolic-store where one is required all fail.
The native wheel build and catabolic-dist run this check, and catabolic-dist
and the wheel build also reject a release build artifact of catabolic-store
compiled with the feature.
"""

import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# Shipped crate -> whether its normal graph must contain catabolic-store.
SHIPPED = {"catabolic-cli": True, "catabolic-python": True, "catabolic-dist": False}
STORE = "catabolic-store"
FEATURE = "failpoints"
# Feature names never contain "@", so the feature field ends at the first one,
# whatever punctuation the package path contains.
SEPARATOR = "@@"


class GraphError(Exception):
    pass


# Maturin settings that change the Cargo feature selection, and those this
# check cannot reproduce faithfully (so their presence fails).
MATURIN_FEATURE_KEYS = {"features", "all-features", "no-default-features"}
MATURIN_UNSUPPORTED_KEYS = {"cargo-extra-args", "config", "unstable-flags"}


def maturin_feature_args(root=ROOT):
    """Cargo arguments matching maturin's effective feature selection."""
    root_project = tomllib.loads((root / "pyproject.toml").read_text("utf-8"))
    if "maturin" in root_project.get("tool", {}):
        raise GraphError(
            "pyproject.toml has [tool.maturin]; only crates/catabolic-python configures maturin"
        )
    project = tomllib.loads(
        (root / "crates/catabolic-python/pyproject.toml").read_text("utf-8")
    )
    settings = project.get("tool", {}).get("maturin", {})
    unsupported = sorted(MATURIN_UNSUPPORTED_KEYS & set(settings))
    if unsupported:
        raise GraphError(
            "catabolic-python maturin settings not reproducible by this check: "
            + ", ".join(unsupported)
        )
    features = settings.get("features", [])
    if not isinstance(features, list) or not all(
        isinstance(feature, str) and feature for feature in features
    ):
        raise GraphError("catabolic-python [tool.maturin].features must list names")
    arguments = []
    for key in ("all-features", "no-default-features"):
        value = settings.get(key, False)
        if not isinstance(value, bool):
            raise GraphError(f"catabolic-python [tool.maturin].{key} must be a boolean")
        if value:
            arguments.append("--" + key)
    if features:
        arguments += ["--features", ",".join(features)]
    return arguments


def parse(package, output):
    """Return (store present, store features) from ``{f}@@{p}`` lines."""
    lines = [line for line in output.splitlines() if line.strip()]
    if not lines:
        raise GraphError(f"{package}: cargo tree printed no dependency graph")
    present = False
    features = set()
    for index, line in enumerate(lines):
        feature_text, separator, identity = line.partition(SEPARATOR)
        name = identity.split(" ", 1)[0]
        if not separator or not name or " v" not in identity:
            raise GraphError(f"{package}: malformed cargo tree line: {line!r}")
        if index == 0 and name != package:
            raise GraphError(
                f"{package}: cargo tree graph is rooted at {name!r}, not {package!r}"
            )
        if name != STORE:
            continue
        present = True
        features.update(item for item in feature_text.split(",") if item)
    return present, features


def store_features(package):
    extra = maturin_feature_args() if package == "catabolic-python" else []
    try:
        result = subprocess.run(
            [
                "cargo",
                "tree",
                "--offline",
                "--locked",
                "-e",
                "normal,build",
                "-p",
                package,
                "--prefix",
                "none",
                "-f",
                "{f}" + SEPARATOR + "{p}",
                *extra,
            ],
            capture_output=True,
            text=True,
        )
    except OSError as error:
        raise GraphError(f"{package}: cannot run cargo tree: {error}") from error
    if result.returncode:
        raise GraphError(
            f"{package}: cargo tree failed ({result.returncode}): {result.stderr.strip()}"
        )
    return parse(package, result.stdout)


def check(resolve=store_features):
    """Return the list of failure messages; empty when every crate is clean."""
    failures = []
    for package, required in SHIPPED.items():
        try:
            present, features = resolve(package)
        except GraphError as error:
            failures.append(str(error))
            continue
        state = ",".join(sorted(features)) or "(none)"
        print(
            f"{package}: {STORE} {'present' if present else 'absent'}; features {state}"
        )
        if required and not present:
            failures.append(f"{package}: {STORE} missing from its normal graph")
        if FEATURE in features:
            failures.append(f"{package}: enables {STORE}/{FEATURE}")
    return failures


def main():
    failures = check()
    if failures:
        for failure in failures:
            print(failure, file=sys.stderr)
        return 1
    print(f"ok: no shipped crate enables {STORE}/{FEATURE}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
