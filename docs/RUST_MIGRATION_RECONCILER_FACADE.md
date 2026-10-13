# Ordinary Reconciler read facade checkpoint

This opt-in adapter preserves eight frozen Python inventory rows: `Reconciler`,
its constructor, `owner`, `catalogs`, `preview`, `verify`, `apply`, and `recover`.
The reference remains `453fca983222c6665a48775eef67c489c96b527e`.
Python remains the default. No milestone or release gate is closed.

## Boundary

`catabolic_native.reconcile.Reconciler` (also exported at package root) retains
its caller's Application, Store, profile, and notification object. The read
methods borrow that exact persistent native Store through the existing PyO3
read/signal bridge. They do not reopen a database, detach a snapshot, create a
second connection, commit a caller transaction, or invoke a companion process.

- `owner` uses the same pure marker-value constructor as the existing filesystem
  owner. Cached database identity remains readable after Store.close(), as in
  the frozen public method. This is the only change to the existing filesystem
  module: one marker-construction expression delegates to the shared pure helper
- `catalogs` selects ordinary catalog metadata from the same Store, including
  uncommitted caller changes and nested rollback visibility
- `preview` and `verify` validate public catalog selection, then forward only
  their public read fields to `filesystem_mutation::execute`. The existing
  fixed-owner unsupported-mode and destructive-plan qualification gates remain
  in force. This adapter does not enable the mutable live-probe owner
- The native adapter preserves budget-before-catalog error ordering. Python
  adapts nonfinite/unrepresentable value kinds to an invalid-value sentinel,
  leaving validation and planning to the native owner. It restores Python
  integer-versus-float scalar kinds for the echoed percent limit and the zero
  denominator percentage; paired checks compare scalar types as well as values
- Signals use the existing interruptible read owner. The installed regression
  checks exact Python KeyboardInterrupt identity and subsequent Store reuse

## Publication is intentionally unavailable

`apply` and `recover` retain their exact public signatures but always raise a
native CatabolicError before catalog resolution, publication locking, journal
execution, output effects, or callbacks. They do not serialize or invoke the
`after_filesystem` object. Readonly callers also receive this qualification
refusal. Closed callers retain the existing native Store closed-state error.

The refusal is unconditional in the new public dispatcher, including debug
builds and private requests containing `disposable_fixture`. Read forwarding
builds a fresh allowlisted request, so private fields cannot relax the existing
production preview gates either. The companion protocols and their existing
default/release gates are unchanged.

This is a gate-refusal witness, not apply/recover parity or evidence of safe
filesystem publication. The harness never calls frozen apply/recover.

## Qualification

Source contracts:

    python -m unittest tests.test_native_reconciler_contract -v

Native adapter-only regressions (metadata fixtures):

    cargo test --locked --offline -p catabolic-store public_reconcile::tests

Strict installed qualification, run outside the checkout:

    /isolated/venv/bin/python -I /repo/scripts/native_reconciler_embedding_tests.py \
      --wheel /artifacts/catabolic_native.whl \
      --build-manifest /artifacts/build-manifest.json \
      --source-root /repo --inventory /repo/tests/parity/python-api.json \
      --reference-root /frozen --reference-python /frozen-venv/bin/python \
      --report /artifacts/reconciler-proof.json

The strict gate requires a release wheel, packaged helper, matching same-build
external native and legacy executables, exact source/artifact identities, fixed
frozen oracle and interpreter/import identity, all six distinct installed
cases without skips, all 25 fixed paired read groups, and actual calls for all
eight inventory rows. Apply/recover coverage is explicitly classified as
refusal-only, with zero paired effect calls. Identity checks repeat after the
run. Optimized Python, stale existing receipt paths, source drift, and artifact
drift fail before a successful receipt can be written.

Fixtures create only disposable databases, metadata, and empty output roots.
An empty root may produce a `prepare` plan, but nothing executes it. The signal
fixture contains catalog-name metadata only. No source media, managed links,
markers, credentials, external services, or actual publication/recovery are
created or exercised.

## Remaining gaps

- Public apply/recover remain unavailable pending their independent safety and
  qualification gates; their callback and effect semantics are not parity
- The existing fixed-owner refusal for fallback reconciliation is retained;
  live fallback probing and mutable-owner snapshot semantics are not exposed
- Closed ordinary reads retain native CatabolicError rather than the frozen
  Python AttributeError from accessing a closed connection; the receipt records
  this lifecycle error boundary separately, not as paired exception parity
- Non-string/custom catalog objects, arbitrarily large integers beyond the
  existing native JSON/u64 boundary, all coercion objects, populated source and
  output fixtures, and all promised platforms/Python versions are unqualified
- Broad filesystem/hardlink/source-access/observation/remount/watcher safety
  reviews, race investigation, independent adapter review, and release gates
  remain untouched and open

Exact executed source and artifact receipts are recorded separately. A passing
strict artifact-bound adapter gate does not qualify the full release.

## Executed local receipt (2026-10-12)

The final strict release-artifact-bound adapter gate passed **6/6 installed
cases, 25 type-aware paired read groups, and eight called inventory rows**.
The native adapter tests passed 2/2; source contracts passed 3/3. Formatting,
Ruff, diff checks, and all-target clippy for `catabolic-python` and
`catabolic-store` with warnings denied passed. No broad subsystem tests ran.

Artifacts were built offline from the isolated `catabolic-reconciler-facade`
worktree with its separate `target`, using Rust/Cargo 1.99.0, Maturin 1.13.1,
and Python 3.14.6. The wheel was installed outside the checkout using isolated
pip with `--no-index --no-deps`. The frozen reference and main source/artifacts
were not modified.

- Source before/after: `c6dc9dc1dd014f1d2c2ea4f10074f6f14062d9427bba2279e134e37d0de32d11`
- Wheel: `10ccca38ce148cb7621aae8bd4064cc99cefcd6488782ce672a652db8e9779eb`
- Extension: `b5551d10c4ecf7fbc7bdabe06151653214d1794a8cb994c0a80ad9527585b155`
- Native helper: `8aabeea6e3a340921ee50d29cefba5c9ff8e01d4889f9a84f6f68ae421e07a9a`
- Legacy helper: `66d86d16bc5f5440464e92db9dfc67139dd4c151d393a63b0f7d57c93fa18c17`
- Build manifest: `715263fc0bfe5c9ec99feca7caa3ef95dea7c0c177522fef9b5194c8217c0a21`
- Receipt: `31fd5f344bbf8f4af1b0d9cbd05a9b30f1ad6a3ef9f780a4dea2aaa71c6ab59f`
- Receipt path: `.local-tests/reconciler-release-final/reconciler-proof.json`

The report includes every successful test identity, separate gate/lifecycle
witnesses, per-row installed/paired call counts, exact installed payloads,
source/build/script/inventory identities, and pinned oracle/runtime/import
identities. All fences passed again after execution. Altered source and wheel
hashes in separate negative manifests were rejected without successful receipts.

Initial evidence is retained under `.local-tests/reconciler-release`: the first
snapshot fixture used rollback-journal mode, which blocked the deliberate
second-writer commit; the fixture now explicitly uses WAL for that metadata-only
snapshot test. The next run passed all lifecycle cases, but its type-aware pair
rejected float-versus-integer output drift. The final Python boundary restores
those scalar kinds without changing native planning or safety decisions. The
initial artifacts are not evidence for the final source snapshot.

This receipt qualifies only the selected adapter contracts. Independent fresh
adapter review and all previously open subsystem/platform/release gates remain
required; there was no push, merge, publication, or default-engine change.
