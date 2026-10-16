# Native definitions and interchange checkpoint

This is a bounded M4 implementation checkpoint, **not completion of M4 or of the
Python-to-Rust migration**. The native companion owns the operations listed here
without invoking Python. Production Python entry points remain the reference
until the wider cutover gates are satisfied.

## Implemented owners

- `catabolic-store::definitions`: saved query selection/report/composition
  definitions, immutable rule revisions and enablement, layout definitions,
  copy/rendition policies, and projection bindings. Canonical digests,
  idempotence, dependency references, older-revision disabling, and nested
  rollback are exercised against Python.
- `catabolic-store::curation`: item identities and metadata, file associations,
  directed relationships and cycle guards, expected collections/completeness,
  proposal snapshots and decisions, existing-association evidence acceptance,
  recorded sidecar discovery, and proposal/history pages. Component assertions
  delegate invariants and SQL ownership to `component_mutation`; source admission
  delegates guarded file access to `source`.
- `catabolic-store::interchange`: released v1/v2/v3 schemas and validation,
  decode/encode, count/reference/checksum validation, layout and ownership
  metadata, tag aliases/ancestor closure, and unknown-field preservation.
- `catabolic-store::manifest`: read-snapshot construction with layout, selection,
  statistics, tags and ownership metadata; guarded manifest publication under a
  held writer lock and transaction. Publication checks lexical paths and ancestor
  inode identities across all profiles, bound root/volume identity, output marker
  ownership, existing checksum/owner/schema, regular-file and single-link status.
  Publication uses no-follow descriptor-relative I/O, mode 0600, file/directory
  fsync, exclusive initial linking and checked atomic refresh. Unchanged content
  preserves the old inode, bytes and generated timestamp. Fault hooks before file
  sync and before publication verify old-export preservation and temp cleanup.
- `catabolic-store::programs`: versioned bounded dependency-closed bundles,
  query/output/operation/rule/layout/projection reference remapping, v2 fallback
  policies and v3 watchers, transactional imports, collision guards and rollback
  previews. Analysis/render operations dispatch through their runtime owners.
  Native external-operation import and unsupported watcher reaction families
  remain subject to the corresponding owner's admission and are not emulated.
- `catabolic-store::estimates`: every version-1 size heuristic, exact unbounded
  nonnegative-integer totals, retained-artifact calibration lookup and version-2
  calibrated estimates. These are recorded-evidence planning estimates, not live
  verification or encoder-size promises.
- `catabolic-store::plans`: strict typed references, dependency descriptions,
  observation requirements, fallback-projection binding digests and stale-digest
  refusal, query selection/rows/document evaluation, component evidence expansion,
  semantic baseline filtering and incomplete-baseline refusal. Evaluation shares
  its SQLite snapshot and cumulative 4 MiB/deadline budget. Fallback/projection
  plan **execution** still requires the future native resolution/publication owner.

Bundle and binding objects are traversed in source insertion order, as frozen
Python's dictionaries are. `serde_json` maps are sorted, so entry points that
hold the request text (CLI request arguments, `program import` files and the
Python extension) record object order with `catabolic_core::json::source_order`.
That order controls first-error selection and same-name revision numbering
only; decoded values, canonical encoding and digests are unchanged. Direct API
callers without source text keep the canonical sorted order.

JSON parsing uses `catabolic_core::decode_json` for ordinary last-key-wins
boundaries and the same structural parser with duplicate rejection for interchange.
Arbitrary integers remain exact, nonfinite floats are rejected, and legitimate
`$serde_json::private::Number` object keys cannot be reinterpreted as numbers.
Frozen schema resources are shipped locally with their original MIT licensing.

## Companion entry points

The CLI and generic runtime API expose `definitions`, `curation`, `programs`, and
`plans` action/JSON requests. `interchange` is a pure action/JSON boundary.
`manifest-build` uses a read-only evaluation; `manifest-write` accepts the validated
document plus `output` or `in_catalog`, and `replace`, under a writer transaction.
These are companion surfaces, not a claim of full legacy CLI or Python facade
compatibility. Runtime/CLI wiring is maintained separately from these owners.

## Differential qualification

Checkpoint on 2026-10-11: **48 native differential tests passed** in 24.707 seconds
with the integrated debug binary. Ruff passed for all seven test files. A separate
CLI smoke check confirmed that plans requested with HTTP context are refused
before opening a nonexistent database. Local evidence and the exact tested binary
SHA-256 are retained under `.local-tests/rust-migration/m4-plus/` as
`definition-owner-*` logs. These local logs are not release artifacts.

The following native subprocess suites compare returned values, immutable hashes,
logical database tables, mutation refusal, and/or actual filesystem effects with
the unchanged Python reference:

```sh
CATABOLIC_NATIVE_BINARY=target/debug/catabolic-native .venv/bin/python -m unittest -q \
  tests.test_rust_definitions tests.test_rust_interchange \
  tests.test_rust_manifest tests.test_rust_curation \
  tests.test_rust_programs tests.test_rust_estimates tests.test_rust_plans
```

The estimate suite contains hundreds of preset/fact/assumption combinations within
its test methods. Tests require a built native binary; without one their explicit
skip guard is not qualification evidence. Store strict Clippy and Ruff should be
run as part of the aggregate checkpoint. Shared-tree compile/lint failures from
other owners must be resolved before claiming an integrated clean gate.

## Explicitly open gates

- Full rule candidate planning, preview, completeness requirement attachment,
  apply/run/maintenance, generated publication eligibility, and reviewed
  application orchestration are not implemented by the definition-only owner.
- Item worklog/status/requirement mutation APIs and optimistic-revision completion
  gates remain open. Existing read-only workflow SQL is not mutation completion.
- Fallback and projection plan execution, full reviewed execution plans and
  stale-plan admission across runtime owners remain open. Unsupported plan kinds
  fail explicitly before pretending to evaluate or publish them.
- Watcher imports can admit only families supported by the native watcher owner;
  a roundtrip of a query/report watcher does not qualify processing, HTTP or
  projection reactions. External-operation import depends on the external owner.
- Metadata export adapters (XSPF, OPDS, NFO), non-overwriting export bundles,
  official-application import adapters and complete target descriptors remain open.
- Exact Pydantic structural diagnostic text for plan/component models is not yet
  reproduced; typed/defaulted accepted values and owner semantic errors are the
  present contract. Recursive JSON depth-limit equivalence also needs explicit
  qualification; the structural parser intentionally has a finite nesting bound.
- General OS error strings are platform-native. Linux publication safety tests do
  not qualify macOS filesystem behavior, all crash points, or hostile concurrent
  replacement interleavings. No native/Python public-facade cutover is implied.
