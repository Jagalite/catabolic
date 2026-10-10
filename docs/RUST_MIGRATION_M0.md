# Rust migration: M0 reference freeze

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

M0 is **in progress**. The [migration plan](RUST_MIGRATION_PLAN.md) remains the
acceptance contract. The implementation freezes the Python reference and
makes the migration scope enumerable; it does not authorize starting M1 or declare
feature parity.

## Reference and scope

The oracle is commit `453fca983222c6665a48775eef67c489c96b527e`, package 0.2.0,
database schema 32, HTTP contract 1.7.0. `git archive` exports that exact commit,
including when the working checkout contains newer changes. Archive, package,
resource, runner and inventory hashes accompany the local evidence. The archive
hash identifies that capture; rebuilding a wheel may produce different ZIP bytes,
so qualify and retain the exact wheel that was installed.

[The frozen inventory](../tests/parity/inventory.json) contains:

- 271 CLI parser nodes, including groups and aliases, with arguments, defaults,
  choices, types and mutually exclusive groups. Parent options retain their own
  scope. `CATABOLIC_*` is unset during discovery. Home-relative default paths
  encode only the home prefix as `${HOME}` and retain the complete suffix; this
  avoids freezing a particular user account. Environment override behavior needs
  separate scenarios.
- 62 operations from the frozen HTTP 1.7.0 OpenAPI document. Its referenced
  component schemas remain in the hashed source resource.
- All 32 SQL migrations and hashes of reference source, dependency locks, tests,
  fixture resources, scripts, docs and CI workflows.
- Public-looking symbols in 143 Python modules, marked for compatibility review;
  these are **candidates**, not a new public API declaration.
- 1,010 statically named test methods and acceptance script entry points.
  Runtime discovery and conditional skips can produce different execution counts.

[The supplemental contracts](../tests/parity/contracts.json) add 388 surfaces:
196 GraphQL fields, 38 GraphQL types, 120 HTTP component schemas, 31 released
HTTP/interchange artifacts, and the Python migration hooks for versions 15, 16
and 25. GraphQL signatures retain arguments, defaults and nullability. Source
hashes are verified against the original inventory before generating contracts
or executing mapped tests.

[The review ledger](../tests/parity/ledger.json) assigns 753 surfaces to the
plan's workstreams. Each row now has planning-level inputs/outputs, effects,
failure behavior and an executable reference proof plan. `specified` means that
this contract and plan are recorded; it does not mean every operation has an
independent behavioral test, or that a Rust implementation exists. Broad
subsystem mappings are labeled as such. `qualified` additionally requires Rust
and differential evidence references.

Additional scope indexes cover:

- [12 integration families](../tests/parity/integrations.json), including
  consumer protocols, Plex, mapping adapters, Apprise, webhooks, playback and
  media processors.
- [Saved-query dispatch variants](../tests/parity/dispatch-variants.json), which
  share one argparse node but include both read-only operations and `query save`.
- [Python imports and signatures](../tests/parity/python-api.json), including
  embedded installed-acceptance programs, constructors and context-manager
  protocols. Preserve embedding facades and repository consumers. Test-only or
  unobserved symbols are not automatically public promises, and their
  classification does not authorize removal or exclude unknown downstream users.

The supplemental checkpoint passed 24 tooling tests, 15 mapped reference tests
and four independent migration-hook checks. The latter cover Unicode/profile
encoding, digest generation, nonfinite-value refusal and UDF cleanup, final FK
validation, and rollback after a backfill write fails. Field-specific differential
scenarios remain future implementation proof obligations.

## Captured platform and fixture evidence

[The CI receipt](../tests/parity/ci-evidence.json) retains results and artifact
hashes from [the exact pinned commit's run](https://github.com/Jagalite/catabolic/actions/runs/37972971491).
All four Linux/macOS Python 3.11/3.14 unit jobs, wheel/sdist validation, both HTTP
jobs and real Jellyfin acceptance passed. Media, consumer-protocol and Apprise
steps passed on both operating systems. The overall run failed: the messy
collection journey failed on both systems, and subsequent storage steps were
skipped. No cancelled or skipped lane is promoted to passing.

The journey's retained checkpoints differ only in refresh-queue count between
`initial` and `repeat1` (0 to 1); subsequent repeats are stable. This narrows the
observed discrepancy without declaring its producer cause fixed. It remains in
[the defect register](../tests/parity/known-defects.json).

A new local macOS storage run passed real cross-filesystem rejection, unmount
preservation and explicit remount/rebind, with no cleanup errors. Linux storage
remains unexecuted. The standalone
[storage workflow](../.github/workflows/migration-reference-storage.yml) passes
`actionlint` and is retained locally: the user explicitly requested no remote
publication. It must run before the full platform gate can close.

[Historical fixture evidence](../tests/parity/legacy-fixture-evidence.json)
retains synthetic populated databases at all 32 schema boundaries. Every one
upgraded to schema 32 with preservation, integrity and FK checks. These are
v1-derived states, complemented by feature-populated v14/v24 scenarios; they are
not every possible historical feature combination. Captured database paths and
filesystem identities are local; rerun the generator in new roots for relocation.
No user database or source media was used.

The complete local-volume 100/1,000/5,000 synthetic run passed, including crash
recovery at each size. Deep, wide, many-small and simulated-slow scan profiles
also passed. The high-cardinality database profiles through one million
records produced 60 successful results and three explicit size-limit refusals; filesystem publication at both 10,000 and 100,000 files exceeded its 600-second
worker budget, so subsequent filesystem phases did not run. The scale CLI exited
zero despite those timeouts; the supplemental receipt reports `budget-limited`. Runtime samples now include fresh-process startup,
query/loopback HTTP distributions, writer-lock holds, job admission, queued and
subprocess cancellation, server RSS and bounded resource snapshots. Their exact
scope and unmeasured quantities are retained with the receipts.

The external-volume synthetic attempt exposed a fixture normalization mismatch:
the catalog contains an NFD filename, while the fixture lookup expects NFC. The
failure is retained separately from the local-volume benchmark. It does not
justify changing arbitrary source-path normalization in product code.

## Initial local results

On macOS arm64 with Python 3.14.6, all 12 new tooling tests and 33 focused legacy
migration tests passed. The installed reference wheel passed import isolation,
`pip check` and `spec check`; source lint, formatting, bundled documentation and
HTTP/interchange contract checks also passed.

The full reference suite ran 1,010 tests with **3 failures, 3 errors and 5 skips**.
Seven component-lifecycle tests passed on a focused rerun, while two importer
startup-marker failures recurred. Original failures remain in the defect register.

The 100- and 1,000-file synthetic scenarios passed. The 5,000-file run completed
initial apply/verify/repeat phases but terminated with `ENOSPC` while writing its
aggregate report. The preceding fixture exception was lost by that reporter; the
5,000-file scenario is incomplete and must be rerun with sufficient disk headroom.
These are single-run local measurements, not performance qualification.

Duplicate exports created during this work were removed after the space failure.
Logs, completed fixture reports, the tested wheel/environment, the original test
export and the canonical `m0-evidence-final` export remain. The cleanup receipt
maps removed duplicate source paths in older commands to the canonical export.

## Reproduce

Use a Python environment provisioned from the repository's hash-locked build and
dev requirements. Capture environment versions even when they differ from CI;
never report an unqualified local environment as the entire supported matrix.
From the repository root:

```sh
python scripts/migration_baseline.py \
  --output .local-tests/rust-migration/new-capture --checks --benchmarks --wheel
```

The output directory must not already exist. The capture retains the archive,
exported source, capture-tool copies, inventory, logs, environment metadata, structured test results,
optional synthetic benchmark output and exact wheel. Each check has a time budget
(default 1,800 seconds). Failures, skips, expected failures and budget stops remain
visible; an incomplete check is not a pass. Unit results are saved after each
completed test so an interrupted run retains partial evidence.

The wheel option builds with the invoking environment's setuptools, installs
hash-locked runtime dependencies into a new venv, installs that exact wheel, runs
`pip check`, and uses isolated Python mode to verify the installed module and
`spec check`. It does not replace the release lane's sdist, full resource,
media, HTTP, consumer or platform acceptance checks. No user catalog is selected.
The runner unsets `CATABOLIC_*` and isolates XDG directories for its child checks.

To verify an existing frozen inventory without rewriting it:

```sh
python scripts/migration_inventory.py \
  --source .local-tests/rust-migration/new-capture/source \
  --output tests/parity/inventory.json --check
python scripts/migration_scope.py \
  --inventory tests/parity/inventory.json --ledger tests/parity/ledger.json \
  --contracts tests/parity/contracts.json
python scripts/migration_contracts.py \
  --source .local-tests/rust-migration/new-capture/source --check
python scripts/migration_proofs.py \
  --source .local-tests/rust-migration/new-capture/source \
  --output .local-tests/rust-migration/new-contract-proofs
python scripts/migration_python_api.py \
  --source .local-tests/rust-migration/new-capture/source --check
python scripts/migration_fixtures.py \
  --source .local-tests/rust-migration/new-capture/source \
  --output .local-tests/rust-migration/new-legacy-fixtures
python scripts/migration_hook_proofs.py \
  --source .local-tests/rust-migration/new-capture/source
python -m unittest tests.test_migration_baseline tests.test_migration_contracts -v
python scripts/migration_m0_audit.py
```

`migration_scope.py --initialize` is only for creating a new ledger file. It
refuses to overwrite the reviewed ledger. Do not regenerate away review work or
change frozen expectations to make a candidate pass.

## M0 closeout

The machine-readable [gate register](../tests/parity/m0-gates.json) distinguishes
captured evidence, specified plans, running work and unexecuted requirements.
Run `python scripts/migration_m0_audit.py` to audit coverage; it exits nonzero while
any required gate remains open. A frozen reference may have recorded defects;
freezing those failures is not a release approval or a parity exception.

The [performance policy](../tests/parity/performance-policy.json) adopts the
plan's greater-than-10-percent repeatable-regression investigation threshold.
Reference and candidate comparisons require repeated interleaved measurements
with matching environments. The current discovery benchmarks overlapped, so
their timing samples are contended and cannot establish isolated release
thresholds. The additional resource/latency metrics remain explicitly listed.

Do not start M1 under a claim that M0 is complete while Linux storage or any other
required gate is unexecuted. Baseline defects remain visible; future differential
scenarios must not silently normalize them away.

See [reference.json](../tests/parity/reference.json) for the original capture and
[local-evidence.json](../tests/parity/local-evidence.json) for the supplemental
local receipts. Historical capture hashes remain unchanged.

### Local Linux storage follow-up — 2026-10-10

An isolated local Lima/Apple VZ Linux VM now supplies the missing storage capture.
The unchanged reference acceptance harness passed actual cross-filesystem
hardlink rejection, source-unmount output preservation and explicit rebind on
new disposable tmpfs mounts; cleanup succeeded. The receipt is retained at
`.local-tests/rust-migration/m2-m3/linux-receipts/m0-linux-storage.json` and the
storage gate is now captured. No remote workflow was dispatched. The VM was
stopped after qualification. M0 remains in progress for its budget-limited
high-cardinality benchmarks and partial resource/performance metrics.
