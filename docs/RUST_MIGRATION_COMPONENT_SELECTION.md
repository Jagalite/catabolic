# Native component selection checkpoint

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

This is a focused implementation receipt, not a completed M4–M11 or release
qualification. Python remains the default. The reference owner for this slice is
frozen `component_packages.py`, together with the package-admission and artifact
reuse portions of `component_packaging.py`.

## Ownership and contract

- `catabolic-core::component` implements pure compatibility and packaging
  decisions over normalized frozen evidence. Same-container timestamps and
  explicit cross-file edition, part, timeline, coverage, synchronization and
  offset claims keep their original meanings.
- `catabolic-store::component_selection` owns candidate queries, current
  occurrence reconstruction, requirement fallbacks, ambiguity, revision-pinned
  dependencies, package signatures, result-budget accounting, exact ready
  artifact reuse, sidecar expansion and admitted-package revalidation.
- `fallback_resolution::Resolver` supplies the single shared query/probe budget
  and detached source-check owner. The selection helper does not run its own
  subprocess or acquire a competing writer.
- `catabolic-runtime::component_packaging::package` previews through that same
  resolver or admits the reviewed plan through the existing artifact owner.
  `operation component package` takes `policy_id`, `item_id`, `location`, `apply`
  and `expected_plan`. Preview returns the full resolution plan. Applying requires
  the exact current plan identity and returns the normal artifact enqueue result.
  Preview uses a writable session for durable shared probe slots but does not
  enqueue a render job.
- Publication revalidation reconstructs pinned video and requirement choices
  within one read-only evaluation with the policy's query and result budgets.
  The existing journal remains responsible for the final live group check.

Selection preserves exact normalized component records and signatures. A
not-ready mux/conversion has no parent-container content URL. Accepted weak root
identity policy does not relax component file-version or source-policy revision
pinning. Exact output lineage remains the artifact owner's responsibility.

## Focused evidence, October 11, 2026

The frozen Python 3.14.6 oracle and a unique copied Rust 1.99.0 native CLI ran
against disposable schema-32 catalogs and source files. Resolution comparisons
use independent database copies retaining the same catalog identity; whole
results, including captures, signatures, plan IDs, counters and reason strings,
are compared without stripping contract fields.

- `tests/test_rust_component_selection.py`: **20 passed**. Includes every frozen
  package-selection test, required/optional requirements, offline dependencies,
  source mutation, refreshed inventory, component and live-check budgets,
  complete component selection, direct/bounded occurrence-decoding parity,
  exact video-occurrence tiers, same-file video
  ambiguity, codec conversion, weak root-device identity, and policy revisions.
- Its sidecar integration case compares exact frozen/native projection previews,
  performs native publication and group revalidation on a disposable output,
  verifies no-op inode stability, retargets to a backup subtitle when the preferred
  source disappears, and retains existing membership when all subtitles become
  unavailable. Expansion row limits preserve the frozen blocked reason and do
  not silently advance to another tier.
- The same suite generates actual five-stream media with FFmpeg 7.1.5, previews
  and admits a package natively, rejects stale preview approval, renders the
  three selected streams natively, checks packet evidence and component lineage,
  compares exact ready-artifact reuse, and rejects a changed output. A separate
  real-media case changes accepted component evidence after enqueue and verifies
  that native execution rejects the stale admitted evidence without publication
  or source modification.
- `tests/test_rust_fallback_probe.py`: **4 passed**. The actual CLI helper matches
  the frozen source checker for current/stale source evidence and private SQLite
  magic; it also rejects catalog identity replacement. Live process IDs retain
  every durable probe slot across native resolution.
- `crates/catabolic-runtime/tests/fallback_probe.rs`: **3 passed**. Production
  probe ownership retains occupied slots; a sleeping external helper fault
  terminates within the bound, is reaped, and leaves no occupied slot; an actual
  catalog mutation during a detached resolver callback acquires the released
  writer and causes the returned receipt to fail with `stale_resolution_plan`.
  The sleeping helper injects an external-process fault, not a replacement
  selection or timeout implementation.
- The pure component compatibility/exact-container unit test passes. Focused
  Python Ruff checks and formatting pass. All-target Clippy for `catabolic-core`,
  `catabolic-store` and `catabolic-runtime` passes with `--locked -- -D warnings`.

The copied CLI for the recorded 20+4 Python tests has SHA-256
`ffafaba0d5a2c45531ce3ce2e93930bf82601a822197bf28c6798e62a808b4a5`.
The runtime integration test was compiled from the current shared checkout.

## Reproduction

Source `.local-tests/rust-migration/env.sh`, build `catabolic-cli --locked`, and
copy the executable to a unique filename before testing. Use `set -euo pipefail`
when capturing logs. Run the Python tests with the frozen source and frozen test
root first on `sys.path`; load each new test file explicitly so that its fixture
imports cannot silently resolve to a changed Python implementation:

```python
import importlib.util, pathlib, sys, unittest

root = pathlib.Path.cwd()
reference = root / ".local-tests/rust-migration/reference"
sys.path[:0] = [str(reference / "src"), str(reference)]
import catabolic

assert str(reference) in catabolic.__file__
suite = unittest.TestSuite()
for filename, classname in [
    ("test_rust_component_selection.py", "NativeComponentSelectionTest"),
    ("test_rust_fallback_probe.py", "NativeFallbackProbeTest"),
]:
    spec = importlib.util.spec_from_file_location(
        filename[:-3], root / "tests" / filename
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    suite.addTests(
        unittest.defaultTestLoader.loadTestsFromTestCase(getattr(module, classname))
    )
result = unittest.TextTestRunner(verbosity=2).run(suite)
raise SystemExit(not result.wasSuccessful())
```

Set `CATABOLIC_NATIVE_BINARY` to the copied executable and execute the snippet
using the isolated frozen Python 3.14.6 interpreter. Run the runtime integration
with `cargo test -p catabolic-runtime --test fallback_probe --locked`.

## Remaining acceptance boundaries

This receipt does not establish full projection/journal integration, all HTTP
principal/grant combinations, a complete interleaving search, exhaustive process
or power-loss behavior, macOS/platform qualification, installed-wheel parity, or
release performance. The sidecar case qualifies its specific group-publication
path; broader projection and journal acceptance remains a separate gate. Global
workspace checks and the migration ledger remain separate gates. No frozen migration or
contract artifact was changed, and no live service or real user media was used.
