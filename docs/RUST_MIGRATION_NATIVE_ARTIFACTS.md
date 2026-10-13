<!-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors -->
<!-- SPDX-License-Identifier: MIT -->

# Installed native companion artifact checkpoint

The `catabolic-native` distribution is an opt-in companion. The Python
`catabolic` implementation and its default command are not replaced. Complete
public-facade and promised-platform qualification are still M10 requirements;
M11 does not authorize publication before the dependency gates pass.

## Build and identity

Run `scripts/build_native_wheel.py --python PATH_TO_PINNED_PYTHON --out DIRECTORY`
with Rust 1.99 and Maturin 1.13.1. This only builds local artifacts. It compiles
both release CLIs, packages them under `catabolic_native/bin/catabolic-native`
and `catabolic_native/bin/catabolic-legacy-native`, and includes separate versioned
digest manifests beside the extension. The generated binaries and manifests are
ignored by Git and explicitly included in the wheel. The script
inspects actual archive bytes, execute permissions and the launcher entry point.

The helper and extension builds are fenced by a source-content digest covering
all crate inputs plus workspace manifests, lockfile, toolchain file, local
Cargo configuration and the frozen HTTP contract embedded from the Python
source tree. Source changes during the pair fail the build. The manifest
also records Rust/Cargo/Maturin/Python identities and a digest of relevant build
environment settings. This is local source-content/toolchain provenance, not a
hermetic or independently reproducible build claim.

On package import, the native extension verifies the packaged helper digest and
pins its path, device/inode, size, mtime and ctime. Each use rechecks that identity.
Missing, changed and non-executable packaged helpers fail explicitly. Development
extension-only builds can still perform in-process operations; child-process
work requires the qualified package or explicit `CATABOLIC_NATIVE_BINARY` override.
There is no ambient PATH lookup or reinterpretation of the Python executable as
a worker. The opt-in installed `catabolic-native` entry point launches this helper.

The separate `catabolic_native.cli.main(argv=None)` callable verifies
`legacy-helper.json`, matching companion source/toolchain provenance and executable
bytes on every call. It launches the packaged legacy helper through a bounded
returning text-stream protocol. It has no environment/PATH helper override.
See [the callable boundary checkpoint](RUST_MIGRATION_PYTHON_CLI_CALLABLE.md).

This identity check assumes a trusted installation directory. Returning a checked
pathname does not close a hostile concurrent check-to-exec replacement race.
No claim of arbitrary-host executable attestation is made.

## Installed checks

`scripts/qualify_native_wheel.py` must run outside the checkout in a clean
environment containing the exact supplied wheel. It verifies every archive
payload against the installed distribution, verifies the imported extension,
checks native read/mutation behavior and GIL release, then launches a real
isolated Rust observation worker from the embedded Python host without a fixture
binary override. It checks the console launcher and refuses missing, modified,
non-executable and changed-after-import helpers. Optimized Python (`-O` or
`PYTHONOPTIMIZE`) is rejected explicitly; each normal run checks that rejection.
The supplied CLI must be byte-identical to the packaged helper. Archive and
installed payload inventories must match completely; an empty or truncated
archive cannot qualify an unrelated installed extension. The installed import
interrupt fixture exercises an actual disposable subprocess, writer progress,
structured uncertain outcome, child reaping and private-copy cleanup.

`native-qualification.yml` prepares a separate opt-in Linux fixture lane and
clean Python 3.11/3.14 installed-artifact checks. The Python-only CI lane uses
`run_reference_tests.py`, which excludes `test_rust_*` modules before importing
them and still runs every original Python test. Configuration is
explicit about the four pinned-oracle/installed-native prerequisites in
`scripts/run_native_prerequisite_tests.py`: two printable-table generator checks
and the paired content-revision and owner-boundary checks. The reference report
enumerates those deferred scenario IDs; the installed job requires all four to
execute without skips against the exact wheel, with payload identity checked
before and after. Frozen oracle bytes, its pinned interpreter and the selected
driver/test sources must also remain unchanged throughout the run.
Source-only native harness tests remain in the Python reference lane. This is
not evidence of a successful GitHub run. The workflow does not publish packages
or qualify macOS, external consumer versions, or unresolved output safety.

The source-stable `55d5448` checkpoint's paired Linux x86_64
`cp311-abi3-manylinux_2_39` wheel passed exact installed checks on Python 3.11.15
and 3.14.6; its wheel/helper hashes and selected embedding coverage are recorded
in [native-m4-plus-checkpoint2.json](../tests/parity/native-m4-plus-checkpoint2.json).
Subsequent source changes require a fresh paired build and exact artifact
requalification before any readiness claim. This evidence does not
qualify lower glibc versions, macOS, other architectures, source distributions,
complete public Python compatibility, or release/rollback/performance gates.
