# Native item, rule, and desired-layout workflow checkpoint

This is an opt-in M4 implementation checkpoint. Production Python remains the
default. It does not close M4, any independent review, filesystem publication
safety gate, full public-facade compatibility, or platform/release qualification.
The behavioral oracle is unchanged Catabolic commit
`453fca983222c6665a48775eef67c489c96b527e`, run with Python 3.14.6.

## Ownership

- `catabolic-store::item_workflow` owns recorded checks/status reads, bounded
  worklog/check pages, notes, status requests, explicit requirements and resolution.
  Every mutation rechecks the item and expected revision inside the same writer
  transaction that appends the worklog event. Requirements and their events roll
  back together. Completion uses the frozen workflow/rendition SQL, including
  profile-specific source, current-job/artifact and external receipt evidence.
  `needs_attention` is computed without rewriting requested status or history.
- `catabolic-store::rules` owns complete candidate snapshots, current analysis,
  render and fenced external job admission, space estimates/calibration, recorded
  evaluation statistics, rule/job links and full-selection requirement attachment.
  Candidate database reads share one read snapshot. Live source and publication
  checks use existing native owners; preview never encodes media, hashes entire
  files or writes a catalog. Estimates are planning heuristics, not guarantees.
- `catabolic-runtime::rules` owns preview/apply/run/maintenance sequencing. Query
  completeness is established before writes; details and queue limits never limit
  requirement attachment. Space and explicit byte-budget blockers precede all
  writes. Matching existing jobs are adopted, failed work requires explicit retry,
  run replans selection and executes only jobs registered to that rule. Maintenance
  shares queue/execution limits and deduplicates estimates by input/item/recipe/
  destination on each device. Processing, claims, attempts, artifact publication
  and external effects remain entirely with their existing owners.
- `catabolic-store::layout_mutation` applies complete desired mappings, never a
  truncated display plan. The existing `layout` planner was factored to retain
  complete desired mappings, owner history and admitted rendition associations
  privately while preserving its read-only report. Prepared plans pin database,
  profile, normalized inputs and catalog generation. Apply refuses stale or
  tampered plans before mutations and uses nested transactions, so a surrounding
  blocked-filesystem preview can roll back all mappings, ownership and statistics.
  Explicit mappings and disabled owned history are preserved. Desired-state
  changes do not themselves publish filesystem outputs.

The companion action/JSON families are `workflow`, `rules`, and `layout`.
`workflow` exposes `get`, `checks`, `log`, `note`, `status`, `require`, `resolve`.
`rules` exposes `preview`, `statistics`, `apply`, `run`, `maintain`.
`layout` exposes `preview`, `prepare`, `apply`; `prepare` includes a reviewed
`plan_digest` accepted by `apply` as `expected_plan`. These are local companion
surfaces, not a claim that the complete legacy CLI or HTTP contract is implemented.

Standalone local rule runs call the native catalog-refresh finish owner once
per batch. Refresh publication failures are reported separately from completed
media work. Maintenance suppresses per-rule drains. Existing filesystem gates
and unsupported fallback-refresh refusal remain effective; this owner does not
bypass either of them.

## Explicit reference correction

Frozen `rules.maintain` assumes every execution result has `completed` and
`errors` arrays. Analysis processing instead returns `processed` and `counts`.
Consequently Python performs the analysis work and then raises `KeyError` when
charging the execution budget. Native maintenance uses `processed` for analysis
and the existing arrays for render/external results. The regression demonstrates
that frozen failure, while an independent SQL ledger verifies exactly one attempt
for a one-job budget, preservation of the remaining queued job, and successful
continuation. This bounded correction does not change processing job states,
claim fences, attempt ownership or execution scope.

## Qualification

Checkpoint qualification on 2026-10-11: **45 pinned-oracle subprocess tests
passed** (30 new workflow/rule/layout tests plus 15 existing layout/definition/
plan regressions). Three focused Rust unit tests pass for nested layout rollback,
stale/tampered plans and exact unbounded-integer budget arithmetic. Ruff and
rustfmt pass for the owned files. The final subprocess run uses an immutable
copy of the rebuilt binary, so concurrent work cannot change its artifact midway.
Its SHA-256 and receipts are retained under `.local-tests/rust-migration/m4-plus/`
as `workflow-owner-binary.sha256`, `workflow-owner-source.sha256`,
`workflow-final-test.log`, and `workflow-final-clippy.log`.

Coverage includes 10,001-input requirement attachment with a one-row display,
scan-scope TEXT IDs, large expected revisions, Python control-whitespace handling,
profile-specific readiness with global revisions, requirement limits and rollback,
external accepted receipts and stale outputs, generated-source exclusion and
explicit derived admission, per-device deduplicated maintenance estimates, complete
layout application, stale reviewed plans, explicit mappings and retained disabled
IDs. Tests require a freshly built native binary; a skip is not qualification.

Strict integrated Clippy is currently blocked only by the active fallback
integration: the requested `layout::render_association` helper is not yet called.
The fallback owner is wiring that caller. No warning suppression was added to
conceal this temporary dependency. The recorded Clippy log, rather than passing
focused tests, establishes the aggregate lint outcome for this source snapshot.

```sh
. .local-tests/rust-migration/env.sh
cargo build -p catabolic-cli --offline
PYTHONPATH=.local-tests/rust-migration/reference/src \
  /workspace/scratch/9c9a5a8c3955/.local-tools/catabolic-reference-3.14.6/bin/python \
  -m unittest -q tests.test_rust_item_workflow tests.test_rust_rules \
  tests.test_rust_layout_mutation
cargo test -p catabolic-store layout_mutation::tests --offline
cargo clippy -p catabolic-runtime --offline -- -D warnings
```

Cloned-catalog comparisons cover returned values, requirement/event/job links,
recorded evaluations and logical database state. Nondeterministic UUIDs are
aligned at the unchanged Python UUID boundary; timestamps and observed free
space are not compared byte-for-byte. The real render test validates generated
bytes against the recorded checksum and verifies source bytes remain unchanged.
Separate Rust tests prove outer rollback and stale/tampered prepared admission.

## Remaining gates

- Complete legacy CLI, Python facade and authenticated HTTP exposure
- Broader rule scale and fault-injection, hostile concurrent filesystem replacement,
  all render/external receipt families and shared-device low-space acceptance
- Full projection/fallback execution and reviewed publication orchestration beyond
  the separately owned desired-layout primitive
- Aggregate multi-layout maintenance: each prepared layout pins one catalog epoch.
  A caller may prepare, apply and preview one layout in a rollback-capable outer
  transaction, as the catalog-refresh owner does. Preparing several layouts first
  and then applying all in one transaction will correctly refuse the second after
  the first changes that epoch. A future aggregate owner needs coordinated
  all-plan admission or transaction-local replanning; it must not bypass the fence
- Independent definition and filesystem reviews, filesystem safety gate closure,
  macOS acceptance, exact-artifact wheels, rollback/recovery and release evidence

No schema, frozen migration byte, worklog kind, journal protocol or production
engine default was changed by these owners.
