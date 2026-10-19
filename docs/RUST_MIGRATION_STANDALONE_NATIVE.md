<!-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors -->
<!-- SPDX-License-Identifier: MIT -->

# Standalone native candidate preparation

This is a **local, unqualified native candidate**, not a release, a default switch,
or completion of a migration gate. The existing Python distribution is untouched.
There is no upload, installer, Cargo publication, stable C ABI, or new Python
packager. The native library remains available through the existing Rust crates.

## Rust-only preparation

Use the pinned Rust 1.99.0 toolchain and an existing dependency cache:

```sh
cargo build --offline --locked -p catabolic-dist --jobs 1
# Use the executable in the selected Cargo target directory:
/path/to/target/debug/catabolic-dist prepare /absolute/checkout /absolute/new-candidate
```

`catabolic-dist` is a small, unpublished tooling crate. It invokes only locked,
offline Cargo metadata and the two release binary targets in `catabolic-cli`,
with one build job. It never builds the PyO3 crate, invokes Maturin, creates a
wheel, installs dependencies, or downloads packages. It rejects Python build
packages in the actual Cargo artifact records. An unavailable cached dependency
is a failed preparation, not permission to fetch it.

The new output directory contains:

- `bin/catabolic`: the preserved Rust CLI, built as `catabolic-legacy-native`
- `bin/catabolic-native`: the separate structured native companion
- `candidate.json`: source, toolchain, build-context, binary and payload identities
- `STATUS.txt`: explicit qualification and safety limitations
- `licenses/`: Catabolic's MIT license, frozen resource notices, actual built
  packages' Cargo license declarations and available package license/notice files

### Exact supplemental notice inputs

Five previously missing notice-text inputs are now bundled under
`crates/catabolic-dist/resources/supplemental-notices`: Inflector 0.11.4,
jsonschema-regex 0.58.6, jsonschema-value 0.58.6, uuid-simd 0.8.0 and vsimd 0.8.0.
The license texts are unmodified upstream bytes. The adjacent `manifest.json`
preserves their source URLs, exact Git revisions, upstream Git blob identities,
SHA-256 digests, byte counts, and package-to-revision evidence. Its original local
paths and candidate reference describe the retrieval evidence; they are not input
paths used by the packager.

For these five package names only, preparation requires the exact version,
crates.io package ID, repository and license declaration, plus the recorded hashes
of the cached `Cargo.toml`, `Cargo.toml.orig`, and `.cargo_vcs_info.json`. It checks
the VCS revision and package path, and the README hash where it supports the
retrieved notice. Another version, source, cache identity, notice text or digest
fails preparation rather than inheriting this evidence. The preparer's embedded
module and resource identities must match the checkout, so changing them requires
rebuilding `catabolic-dist`. No registry/cache files are changed and no upstream
fetch is performed.

Normal registry notice discovery is unchanged. For a built package on this exact
list, preparation additionally stages `UPSTREAM-LICENSE.txt` in its dependency
notice directory and records a `supplemental_notice` reference in
`licenses/dependencies.json`. The evidence manifest is copied byte-for-byte to
`licenses/supplemental-notices/manifest.json`. Verification checks these exact
package bindings, notice bytes, and provenance bytes without needing the cache or
a network connection, in addition to the external candidate-manifest anchor and
complete payload inventory. It rejects missing, changed or wrongly attributed
supplemental notices. Historical candidates without the inputs remain unchanged;
use their original preparation tool and anchor for historical verification.

This resolves these five missing notice-text inputs only. It does not complete the
dependency/license audit, establish every distribution obligation, or close M11
or any release gate. No full native candidate was rebuilt as part of this isolated
packaging change; the next candidate still needs a new source freeze and its own
preparation, integrity, safe smoke and runtime acceptance evidence.

No `src/catabolic` business implementation or `.py` runtime file is staged. The
earlier packaging baseline embedded the optional Apprise helper; that presence
was recorded rather than hidden by safe smoke results. The integrated source
now replaces it with [supervised native providers](RUST_MIGRATION_NATIVE_NOTIFICATIONS.md).
The final staged provider path needs its own replay, and complete runtime
acceptance still needs wider evidence. The manifest deliberately keeps
`python_free_runtime_accepted` false.

The manifest records the canonical checkout location, and the source inventory includes the six native/tooling crate directories, workspace
manifest/lockfile/toolchain/license, local Cargo configuration, and frozen HTTP
contract embedded from the source tree. It includes all files in those crate
directories, including tests and resource provenance. The tool rejects changed
source/recorded build context during preparation, an old preparation executable whose own
source changed, duplicate/wrong Cargo target identities, nonregular/symlink
inputs, oversized files/inventories, and existing output directories. No existing
output is reused or overwritten. Failed work has an `INCOMPLETE` marker and no
successful candidate manifest. Successful payload files/directories become
read-only against accidental editing.

Source-content identity is authoritative; `git_head` is supplemental and may
precede uncommitted local changes. Selected non-secret compiler and Cargo environment inputs (including target-qualified compiler and SQLite flags) are hashed rather
than exposed. Default or explicit Cargo-home and ancestor Cargo configuration files are also hashed before and after the build. These records are local
provenance, not a hermetic or independently reproducible build claim. Upstream
compiler/dependency binaries and the dynamic
system linker are not independently attested. License inventory is not a legal
compliance audit, and a Cargo license expression does not establish that all
required third-party notices have been found.

## Integrity and bounded smoke

Preparation prints a SHA-256 digest of `candidate.json`. Save that digest outside
the candidate as the trust anchor, then run:

```sh
/path/to/target/debug/catabolic-dist verify /absolute/candidate MANIFEST_SHA256 /absolute/checkout
/path/to/target/debug/catabolic-dist smoke /absolute/candidate MANIFEST_SHA256 /absolute/new-smoke.json
```

Omit the final checkout argument to check candidate bytes without current-source
comparison. Verification requires an external manifest anchor, exact complete
payload inventory (including directories), executable roles and permissions,
required notices/status, source-inventory digest, and explicitly unqualified
status. A self-consistent edited manifest is not trusted merely because its own
payload hashes agree. Preserve the externally recorded manifest digest.

Smoke starts in a new `/tmp` directory, with a cleared environment, PATH containing
only denial stubs for common Python interpreter names, and the companion pinned
by absolute `CATABOLIC_NATIVE_BINARY`. It exercises both binary identities,
preserved help, native initialization/reopening of a disposable catalog and the
two explicit HTTP refusals. It checks candidate integrity after every command
and writes a fresh receipt only after every assertion passes. Existing reports
are refused, so stale successes cannot be reused. Subprocess time/output budgets
are enforced and timed-out Unix process groups are killed.

This proves only the named safe smoke journeys without PATH-based Python. It
is not proof against an arbitrary absolute interpreter path, every runtime route,
external tools, hostile concurrent installation-directory changes, or complete
legacy behavior. The notification provider path is separately tracked. All
existing M6 output/mutation/recovery and HTTP fail-closed gates remain in place;
preparing or running these safe fixtures does not qualify dangerous functions.
No real catalog, media root, credential or remote service is used.

## Tests

```sh
cargo test --offline --locked -p catabolic-dist --jobs 1
cargo clippy --offline --locked -p catabolic-dist --all-targets --jobs 1 -- -D warnings
cargo fmt --check -p catabolic-dist
# Optional focused staging check using only the existing exact-version cache:
cargo test --offline --locked -p catabolic-dist --jobs 1 stage_five_notices_from_existing_offline_cache -- --ignored
```

Negative controls cover changed/added source, changed/swapped/missing/nonexecutable
binaries, wrong/missing/duplicate Cargo binary identities, changed manifest and
qualification claims, unexpected Python files/empty directories, incomplete
output, symlinks, output/report overwrite refusal, absent success metadata after
failed preparation, and command time/output limits. These are deliberately
bounded packaging checks; no broad filesystem/output safety review is implied.
Supplemental-notice tests additionally cover the five exact texts and provenance,
wrong package/version/source/license/repository, changed or missing cached evidence,
wrong revision/VCS path, changed preparer resources, missing/duplicate/unsupported
notice receipts and changed notice/provenance bytes. The ignored cache-backed test
uses the normal license staging path for all five inputs; it does not build either
engine executable or prepare a complete candidate.

## Standalone companion discovery

For preserved-command journeys that need the structured companion, configure it
explicitly before launching the Rust CLI:

```sh
export CATABOLIC_NATIVE_BINARY=/absolute/candidate/bin/catabolic-native
/absolute/candidate/bin/catabolic --help
```

The existing helper owner has no ambient PATH search. Its direct runtime users
include isolated observation scans, supervised watcher workers and fallback
probes. Shipping both executables does not automatically configure their
relationship, and bare `bin/catabolic` is not claimed to support every child-worker
journey without the setting above. Do not point the shared override at the
preserved CLI; its command grammar is different. The new notification provider
owner may pin its own preserved executable for its notification-only worker when
no companion is configured; this does not change other workers' discovery.

Configuration provenance requires a nonempty `CARGO_HOME`, or a nonempty `HOME`
when `CARGO_HOME` is unset. Relative home paths resolve against the build checkout,
matching the child Cargo working directory. With neither setting, preparation
refuses rather than silently omitting an operating-system home fallback.

The local development preparation produced both release executables and passed
all eight bounded safe smoke cases from outside the checkout with zero Python
PATH-denial hits. Eleven Rust negative-control/configuration test groups and strict
Clippy passed. This is preparation-tool development evidence; the integrated
source, including the provider replacement, still needs its own exact candidate,
manifest anchor and safe/provider smoke receipts before any current-artifact
claim. The candidate remains unqualified even when those bounded checks pass.

Environment provenance uses a narrow compiler/Cargo allowlist. Registry tokens,
credentials, proxy settings and arbitrary environment variables are not captured,
even as inputs to that digest. Unknown build-script environment inputs are outside
the recorded-context fence; this is another reason the candidate is not a
hermetic-build proof. Configurations are recorded by content digest, never body.

## Independent native CI source (not yet executed)

`.github/workflows/standalone-native.yml` defines a separate bounded Linux
(`ubuntu-24.04`) / macOS (`macos-15`) lane. It has no dependency on the existing
Python/wheel workflow, which remains unchanged. Only the existing pinned checkout
and artifact-upload actions are used, with read-only repository permission and
checkout credentials disabled. There is no release, publication or default-switch
step. A matrix definition is not evidence that either platform has passed.

CI installs the exact Rust 1.99.0 toolchain from Rustup and explicitly fetches the
workspace lockfile's Cargo dependencies once. That fetch may cache dependencies
of workspace members that are not built; it does not execute a Python build. All
subsequent Cargo builds/tests use `--offline --locked --jobs 1`. No Python setup,
interpreter, Maturin, wheel, pip/PyPI, oracle installation, package-manager install,
real service or user secret is required. Runner-provided Bash, Git, jq, tar and
shasum are host-tool prerequisites; missing tools fail rather than trigger installs.

`scripts/standalone-native-ci.sh CHECKOUT NEW_RUN_DIRECTORY` then requires a clean
source checkout, installed pinned compiler, an absolute `CARGO_TARGET_DIR`, and a
new outside-checkout output directory. It runs:

1. Formatting and warnings-denied all-targets Clippy for seven explicitly named
   native/tooling/verification crates, excluding `catabolic-python`
2. The `catabolic-dist` suite, followed by discovery and exactly one successful
   execution of `supplemental::tests::stage_five_notices_from_existing_offline_cache`
   with `--ignored --exact`. Missing or skipped execution is a failure
3. Rust-only preparation, a separately saved manifest SHA-256 anchor, independent
   manifest hashing and verification against the current source
4. Eight existing bounded outside-checkout smoke cases with Python PATH denial
5. All eight existing `native_notification_providers` Rust fixtures, each required
   to be discovered and execute once against explicit overrides pointing at the
   staged binaries. Candidate/source verification follows every fixture. These
   are disposable loopback/supervision tests, not live-service qualification;
   one replacement test intentionally uses a disposable copy of the staged CLI

Fresh per-command logs and a terminal `evidence/result.json` retain both failure
and success. Setup logs are always uploaded independently. A previously existing
run directory is refused without modifying its evidence. As soon as preparation
and verification succeed, `candidate.tar` preserves the read-only payload and
executable modes that a direct GitHub artifact upload would discard. Its digest
and the manifest anchor live outside the archive. The checked archive is retained
and uploaded even if later smoke or provider checks fail; a failed run does not
turn its archive into an accepted candidate. No-file uploads fail explicitly.
Hard cancellation before the exit trap can leave only partial logs/setup evidence;
missing terminal evidence must not be interpreted as success.

Downloaded artifacts still need archive-digest checking and verification using
the separately retained manifest anchor and a compatible `catabolic-dist` before
use. GitHub artifact retention and read-only filesystem modes are not cryptographic
attestation, signed provenance, reproducible builds or protection from hostile
concurrent changes. The normal source inventory fences native inputs; the workflow
also records Git HEAD and requires the full tracked checkout to stay clean.

Local validation of this CI change used Bash syntax checks, YAML parsing with an
already-installed parser, and the shell-only fake-command contract suite. Its
literal workflow digest rejects changed/disabled execution, ignored failures,
wrapper dependencies, unpinned actions and success-only evidence uploads. Its
runner fixtures cover successful sequencing, missing toolchains, wrong Rust,
dirty source, lint failure, missing/skipped notice and provider tests, missing or
failed preparation, missing manifests, wrong anchors, failed verification, missing
or invalid smoke receipts, disappearing final archives/anchors, failed source-list
commands, late nonzero exits, and later failures retaining the candidate archive.
These fixtures execute no Cargo build, dependency download or real candidate.
An independent narrow CI review found a missing-archive false success; the corrected
final check now rejects both the retained regression and an independent reproducer.

No workflow was pushed or executed, no current native candidate was prepared,
and no real Rust build/test was rerun for this isolated wiring change. Linux and
macOS execution remain pending. This lane does not run the six mandatory ignored
lifecycle/media/admin witnesses, complete engine suites, generated-reference/model
checks, optimized destructive-output refusal checks, mounted-filesystem tests,
full protocol/parity, performance or rollback acceptance. Those remain separate
requirements, not silently skipped qualifications. All `python_free_runtime_accepted`,
release and M4–M11 qualification flags remain false. Existing destructive-output
and experimental loopback HTTP gates, and both blocked broad review lanes, are
unchanged. Thin-wrapper work remains last.
