# Ordinary public Watchers adapter checkpoint

This selected M5/M10 checkpoint adds `catabolic_native.watchers.Watchers` and
`catabolic_native.source_events.invalidate` for the eleven frozen inventory rows.
It is not full Watchers parity, a release/default change, or closure of a broader
filesystem, source-access, observation or watcher review.

## Ownership

- The facade retains the caller's Application, persistent Store and profile.
  Metadata, admission, fencing, history and invalidation dispatch to their
  existing native owners through Session and the PyO3 boundary. No Python SQL,
  alternate Python decision engine, or path-based one-call Store is used.
- Query-plan report/event runs use the existing production watcher lifecycle and
  completion owner. `watcher::run` remains the Store-facing entry; `run_owned`
  contains its unchanged lifecycle body and accepts an owner dereferencing to
  that same Store. A wrapper local to `public_watchers` lends the Session's
  existing Evaluation, so evaluation sees caller-uncommitted definitions/data.
  A new evaluation is created for each run; it has no earlier selection cache
  when observation/admission changes occur.
- Python cancellation returns the exact captured exception after returning the
  Session. An active caller transaction stays on its owning thread. An idle
  Session is lent to the native worker while the caller polls signals.
  The borrowed query's progress-handler cleanup is followed by reinstating the
  outer cancellation bridge before completion mutations. On a completion error,
  existing lifecycle failure bookkeeping can finish without being interrupted
  again. A signal first observed after a successful commit cannot undo it.
- Event invalidation uses the existing observation `dirty` operation, including
  generated-source suppression and watcher invalidation in the same transaction.
  The Python `reason` return value is preserved without serializing arbitrary
  caller objects.

## Concrete missing functionality

The ordinary facade currently refuses these run families **before admission**:

1. Observation plans: require an end-to-end public observation owner contract
2. Fallback plans: existing completion detaches and reopens a writer/evaluator;
   it needs an equivalent borrowed completion boundary
3. Processing reactions: need the detached capability/admission owner's public
   transaction and cancellation contract
4. HTTP reactions: existing HTTP preparation reopens a reader; it needs a
   same-owner preparation boundary
5. Projection reactions: the existing publication restriction remains in
   effect; public calls never forward the private disposable-fixture option.
   Projection-plan report/event runs also need a borrowed planning boundary

The refusal witnesses are not oracle-equality or completed-parity claims. Full
Pydantic coercion/validation-error equivalence, all scheduler/timezone inputs,
all SQLite exception classes, bound-source observation effects, publication,
recovery, and platform behavior remain unqualified.

## Verification entry points

- `cargo test -p catabolic-runtime --test public_watchers_facade`
- `python -m unittest discover -s tests -p test_native_watchers_contract.py`
- Installed outside the checkout:
  `python -I scripts/native_watchers_embedding_tests.py --wheel WHEEL
  --source-root ROOT --build-manifest MANIFEST --inventory INVENTORY
  --reference-root FROZEN_REFERENCE --reference-python PYTHON_3_14_6 --report REPORT`

The driver binds the source, complete installed artifact, fixed scenario/group/
row counts, signatures and pinned oracle. `--dev-wheel` is an explicitly
helperless development checkpoint, never release qualification. The parent
integration must build and qualify its own final same-source artifact.

## Local executed development evidence (2026-10-12)

- Rust 1.99.0: six `public_watchers_facade` tests passed, including equivalence
  of the original Store-facing runtime wrapper and borrowed completion, plus
  deterministic completion-cancellation fixtures for both transaction modes
- Nine source-contract tests passed on both Python 3.14.6 and Python 3.11.15
- The same helperless development wheel was installed outside the checkout and
  passed all 18 required scenarios, 32 exact paired groups and 11 inventory-row
  checks on each interpreter, using the pinned Python 3.14.6 oracle
- Five unsupported-family witnesses are separately recorded as refusals, never
  counted as paired parity; cancellation fixtures only use disposable SQL
  triggers and signal the fixture process itself. The cancellation scenario
  covers admission and completion, exact exception identity, caller rollback,
  failed-run bookkeeping and same-owner reuse
- Ruff checks and `git diff --check` passed

Source-content digest:
`eae8fa405d6a540a9c500583309880c41b1705114b5910c8ae9a604d95b8d78e`

Development wheel digest:
`0fce168251872f64cf56cc7c312e352c1c3de187f6608abfd8bfbeee873687c8`

Native extension digest:
`67cf6ba45e3d071b412d6220c0f61765bfe5137221b025125c04e9282772fe9a`

Strict release-wheel mode is implemented but was not run in this lane. No full
migration, publication, platform or release acceptance is claimed.
