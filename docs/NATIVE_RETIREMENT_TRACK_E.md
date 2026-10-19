<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

# Native retirement, track E: packaging and distribution

Decisions of October 16 (binding): macOS arm64 only, the native engine becomes
the `catabolic` distribution itself (maturin, thin compatibility layer, version
1.0.0), the Python API is the CLI plus the 159 preserved names, and every other
public name is deprecated with a documented list (`PYTHON_API_1_0.md`).

## Single distribution

`crates/catabolic-python/pyproject.toml` now builds one distribution named
`catabolic` 1.0.0 with maturin. Its `python-source` ships two top-level packages:

- `catabolic_native`: the extension, the paired `catabolic-native` and
  `catabolic-legacy-native` helpers and the native facade;
- `catabolic`: the thin compatibility layer. It contains no engine, SQL,
  provider, process or filesystem business logic and never imports the frozen
  Python engine. It explicitly reexports existing `catabolic_native` objects, so
  constructors, methods, argument handling, result objects, exceptions and safety
  refusals stay owned by the native facade. `catabolic.cli.main` is the same
  returning `catabolic_native.cli.main` object, which keeps the paired-helper
  manifest checks. No traceback is synthesized.

Console scripts: `catabolic = catabolic.cli:main` and
`catabolic-native = catabolic_native:main`. The Cargo workspace version is 1.0.0.
The earlier separate `catabolic-native` 0.2.0 and `catabolic-native-compat`
candidates no longer exist as distributions.

### Exclusive environment

Never install this distribution into an environment that contains the legacy
`catabolic` 0.x distribution or another `catabolic` namespace; both own the
same files. Use a new environment. The compatibility qualification
(`scripts/qualify_native_compatibility.py`) refuses legacy, split or foreign
catabolic installations and preloaded namespaces, requires the installed
compatibility files to equal the checked-in source, and then compares forwarded
object identities on the exact installed native artifacts.

### Limits that remain

Unselected private/deep imports, legacy class `__module__` strings and pickle
paths, `python -m catabolic.cli` and arbitrary sqlite connection compatibility
are outside the supported API. Production HTTP admission, publication/recovery,
omitted output-root creation, native exception behavior and the unknown-profile
and mapping-v2 decisions keep their existing limitations. Name presence and
forwarding tests do not establish behavioral parity or authorize retiring the
Python engine.

## Build-only release lane

`.github/workflows/native-release-candidate.yml` is dispatch-only on a macOS
arm64 runner with a read-only token. It runs `scripts/check_failpoints_graph.py`
and `scripts/generate_api_disposition.py --check`, builds with
`scripts/build_native_wheel.py`, installs and verifies the wheel, and uploads the
artifact. It has no publishing step, and `release.yml` is unchanged.

## Local 1.0.0 wheel and installed acceptance

Built through Kyoto from commit 2e2532f on macOS arm64 (Python 3.14.6, maturin
1.13.1): `catabolic-1.0.0-cp311-abi3-macosx_11_0_arm64.whl`, SHA-256
`f25584f9ad3342c887d9e5f6e06a9b77e3f7f7643dd674c4b4be5c5f04ab23cc`. Installed
into a fresh venv outside the checkout. Exact commands and every lane result are
in `/Volumes/seed1/Projects/catabolic-builds/evidence/track-e/RESULTS.md`.

Passing: wheel qualification (before and after the suite), compatibility
qualification (159 rows), installed API disposition check, the prerequisite
tests, and the query, component-policy, catalog-refresh, fallback, watchers,
access-admin, private-secret, layout, reconciler, publisher and Plex lanes, plus
the embedding lane on a rerun after one load flake.

Findings fixed in this track: the wheel did not include the compatibility
package (explicit maturin include), maturin rejected a LICENSE outside the
project, `catabolic --version` printed the frozen 0.2.0, and
`catabolic manifest --output/--in-catalog` always failed with "database is
locked".

Open, not Track E and not relaxed: `scripts/acceptance.py` stops at
`rendition import-receipt` because native does not yet publish catalog links
automatically; `scripts/consumer_acceptance.py` hits the intentional
`projection execute` refusal; `test_sigint_worker_wait` fails on macOS; two
processing lanes hardcode `/usr/bin/ffmpeg`; the legacy-adapter inventory has
drifted for `tests/test_rust_legacy_external_worker_diagnostics.py`. Skipped:
FFmpeg libwebp, Tesseract, real services, ASGI lanes (need Python 3.11 or an
offline wheelhouse).

## Remaining step: switching the default

Switching the root distribution to this project, removing `src/catabolic` from
the shipped package, changing `release.yml` and publishing all await the user's
explicit approval. None of them is done here.
