# Ordinary Layouts and projection-digest adapters

Local implementation checkpoint, 2026-10-12. Base: `7c0cb4fc5a2168930ad0623fdfba38afef7ee186`.
This is a nine-row, opt-in Python adapter checkpoint. It does not close a
migration milestone, switch the Python default, qualify a release, or establish
an aggregate passing source. The narrow product correction has passed independent adapter review; the
separate shared receipt-bootstrap follow-up remains open.

## Independent-review corrections

The initial `3e20a80` implementation passed its selected development cases but
failed independent adapter review. The review found a SQLite progress handler
retained after poisoned-scope cleanup, eager Python flag truth conversion/type
erasure, and stale-receipt cleanup gaps. The correction retains those findings
rather than treating the earlier passing cases as approval:

- Cleanup calls the same cleanup-only `Session::clear_interrupt_handler` helper
  introduced by the access adapter. Poisoned operations still refuse; cleanup
  does not revive a scope. A regression verifies zero callback invocations in
  a later raw recursive query after the cancelled scope unwinds.
- A dedicated PyO3 boundary retains the Python option objects. Native exact-PyInt
  admission rejects bools, integer subclasses, IntEnum, and invalid arbitrarily
  large integers before invoking option truth callbacks. Apply is consumed after
  limit validation; replace-layout and allow-empty are consumed only at their
  planner branches. Exception identity and rollback are tested for all three.
  Existing Rust bool-taking entry points wrap the same planner without changing
  their behavior.
- The bootstrap adopts the shared receipt worker's standalone canonical-option
  scanner, invalidates every explicit report/attempt before fallible preflight,
  and rejects abbreviated options. Imports do not remove a caller's receipts.
  Repeated malformed-report and import-under-report controls are included.

The current artifacts below incorporate these corrections. Independent product
re-review passed on both interpreters, including zero retained callbacks after
poisoned cleanup, deferred option protocol edges, exception identity, rollback
and reuse. Integration commits are `44bb236` and `d06583d`. The initial receipt
scanner still needs the separately coordinated shared follow-up for unlink
failures and argparse filename classification; product approval does not qualify
that harness issue or a final integrated release artifact.

## Scope and ownership

The selected rows are `layouts.Layouts`, its constructor and five public
methods (`get`, `list`, `put`, `effective_definition`, `run`),
`plans.projection_digest`, and `layouts.PRESETS`.

- The Python methods adapt values and call the existing persistent NativeStore
  session. They do not open another catalog, invoke the CLI helper, share a
  Python sqlite3 connection, or reproduce SQL/layout/hash logic in Python.
- `get`, `list`, and `put` forward to the existing native definitions owner.
  `effective_definition` reuses its validated definition and the projection
  overlay factored from native layout preparation. Existing preparation keeps
  its original validation order and schema assumptions.
- `run` lends the current Store to a bounded native evaluator. The adapter-only
  preparation method seals the same complete plan consumed by
  `layout_mutation::apply`. It therefore sees uncommitted caller changes.
  The ordinary snapshot-based `layout_mutation::prepare` entry point is unchanged
  and still refuses an active writable caller transaction.
- Writable application uses a private nested rollback scope. An exception after
  mapping writes rolls back mapping ownership and statistics together, while an
  outer caller rollback still rolls back the entire operation. This private
  savepoint is an explicit native adaptation, rather than an assertion of
  identical internal sqlite3 savepoint behavior.
- The adapter restores the SQLite interrupt bridge after evaluation, including
  ordinary evaluation errors, before follow-on writes. Cleanup removes that
  handler through cleanup-only access even when the operational session is
  poisoned, so a latched cancellation cannot interrupt cleanup or leak into a
  later operation. Native scope poisoning still prevents reuse after SQLite has aborted
  an enclosing lexical transaction until the caller unwinds it.
- Limit validation precedes definition/catalog lookup and write admission.
  Readonly application preserves lookup/planning before write refusal and
  returns blocked plans without attempting mutation. Existing native read-only
  exception text/category for a safe write remains a documented boundary gap.
- `projection_digest` calls the existing native plan hash owner on the same
  Store. `PRESETS` is decoded from the same embedded `layouts.json` resource used
  by the native owner, generated from the pinned Python reference.

Only desired database mappings are applied here. No filesystem publication,
reconciliation, recovery or output admission gate is altered. This work is not
an independent review of definitions, plans, layout algorithms, or filesystem
safety. The previously blocked broad reviews remain blocked and unattempted.

## Selected evidence

The helperless development wheel was installed outside the checkout in clean
Python 3.11.15 and 3.14.6 environments. Each ran:

- Thirteen fixed installed scenarios, all passing without skips
- Twenty-seven fixed exact paired groups against isolated copies of the frozen
  Python `453fca983222c6665a48775eef67c489c96b527e` oracle using Python 3.14.6
- All nine fixed inventory rows, including exact signatures, import value, and
  positive installed and paired invocation coverage

The installed cases cover same-owner preview/application, uncommitted visibility,
outer/nested rollback, definition and statistics rollback, bounded details,
readonly/closed/detached/replaced owners, original Python signal and reconnect
callback exception identity, owner reuse, and no Python-engine/helper fallback.
Paired groups cover the imported presets, normal values/errors, validation order,
preview/application state, projection overlays/profile isolation, and digest
changes and rollback, exact-int transport, and deferred option callback ordering. Readonly blocked-application results are paired too.

Six Rust adapter tests passed, including deterministic interruption *after*
evaluation during a mapping insert trigger, poisoned lexical scope refusal,
complete rollback, and subsequent same-owner reuse. Late cancellation after
writes and callback unwind are separately tested. Two unchanged native layout
mutation regression tests passed, including stale/tampered prepared-plan refusal.
Eight source/gate contract tests passed on both Python versions. Ruff and formatting
checks passed. The pinned resource generator verified all 38 generated resources.

The gate is `scripts/native_layout_embedding_tests.py`. It supports strict
paired-release-artifact mode and an explicit `--dev-wheel` mode. Both retain
fixed scenario/group/inventory sets, exact installed package/wheel comparison,
source-before/after and extension fences, frozen tracked-reference verification,
interpreter verification, signatures, positive call coverage, and end-of-run
rechecks. Failed/optimized invocations invalidate old success/attempt receipts.
A passing development receipt cannot satisfy strict release mode.

## Artifact provenance

All paths below are relative to the isolated `catabolic-layout-facades` worktree.

- Wheel: `.local-tests/layout-facades/wheels/catabolic_native-0.2.0-cp311-abi3-manylinux_2_39_x86_64.whl`
- Build manifest: `.local-tests/layout-facades/build-manifest.json`
- Python 3.11 receipt: `.local-tests/layout-facades/evidence311.json`
- Python 3.14 receipt: `.local-tests/layout-facades/evidence314.json`
- Source-content digest: `b8491c5e0b95d745429e584ca86d080435f259c67c849cea4755241cf59257b8`
- Wheel SHA-256: `de4848cb5499c7d9d655052cb6de479fef80af3dd392bdca6692090b10d8336e`
- Extension SHA-256: `e888aff46b170f2b88ca034133f52e4e5f641670776f251fbdbd85e12997d707`

Build: Rust/Cargo 1.99.0, pinned Maturin 1.13.1, offline local dependencies,
worktree-owned `CARGO_TARGET_DIR`. This is local source/artifact provenance, not a
hermetic or reproducible-build claim. No upload, push, merge, or publication ran.

## Remaining gates and limits

Strict release-wheel qualification was not run for this slice. A newly integrated
source must be rebuilt and its exact artifacts qualified again; these receipts
cannot qualify another aggregate. Broad definition/layout/plans/filesystem reviews,
all native/public parity, historical-schema breadth, macOS and real filesystem
acceptance, performance, rollback qualification, and the existing migration
prerequisites remain open. Python remains the default.

The adapters inherit existing native layout/selection/definition behavior; the
selected cases do not qualify every layout, query, publication, corruption,
crash, or cancellation schedule. Foreign Python Stores and arbitrary sqlite3
connections remain unsupported. Non-JSON Python values can fail serialization
before semantic validation. Native SQLite/validation exception categories are
not universally identical to Python. Mutating the imported PRESETS dictionary
changes that Python value, not the embedded native validation resource.
