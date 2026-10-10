# Rust migration: M0 reference freeze

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

M0 is **in progress**. The [migration plan](RUST_MIGRATION_PLAN.md) remains the
acceptance contract. This first implementation freezes the Python reference and
makes the initial scope enumerable; it does not authorize starting M1 or declare
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

[The review ledger](../tests/parity/ledger.json) assigns the 365 enumerated
CLI/HTTP/migration surfaces to the plan's workstreams. Rows start as
`inventoried`, with unreviewed semantics and proof mappings explicitly absent.
`specified` requires mutation/failure descriptions, reference tests and a proof
plan; `qualified` additionally requires Rust and differential evidence references.
The validator checks structure and coverage, not the truth of a test receipt.

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
  --inventory tests/parity/inventory.json --ledger tests/parity/ledger.json
python -m unittest tests.test_migration_baseline -v
```

`migration_scope.py --initialize` is only for creating a new ledger file. It
refuses to overwrite the reviewed ledger. Do not regenerate away review work or
change frozen expectations to make a candidate pass.

## Remaining M0 gate

1. Review every ledger row's inputs/outputs, mutations, refusals and executable
   proof mapping. Expand enumeration to GraphQL fields, exported schemas,
   integration adapters and Python migration hooks. The SQL files alone do not
   capture the Python hooks for versions 15, 16 and 25 in `migration.py`.
2. Classify documented and de facto Python consumers, including installation,
   `catabolic.cli:main`, `Application`, `Store`, and direct module imports. Preserve
   public contracts without treating every test-only helper as public.
3. Finish the fixture corpus. Existing synthetic version-1, version-14 and
   version-24 migration scenarios are registered in
   [fixtures.json](../tests/parity/fixtures.json); they do not yet establish all
   supported historical states or cross-implementation recovery.
4. Resolve baseline failures or record reviewed oracle exceptions in
   [known-defects.json](../tests/parity/known-defects.json). Missing external tools
   remain qualification gaps, not allowed parity losses.
5. Run the applicable existing CI/acceptance lanes and preserve their receipts:
   Linux/macOS on Python 3.11/3.14; exact wheel/sdist; media; storage mounts;
   consumer protocols; optional Apprise; real Jellyfin; HTTP; TypeScript clients;
   fallback and watchers. The existing matrix is the minimum qualification
   target, recorded in [platforms.json](../tests/parity/platforms.json).
   Native wheel architectures/deployment targets still require an
   explicit inventory; `py3-none-any` does not establish native portability.
6. Complete baseline measurements at 100/1,000/5,000 synthetic sizes plus the
   planned resource/latency metrics, and ratify performance thresholds. A single
   local synthetic run is not a real-library or NAS performance claim.

See [reference.json](../tests/parity/reference.json) for this initial capture's
identities, results and evidence locations. No M0 completion or Rust-equivalence
claim follows from the new tooling tests passing.
