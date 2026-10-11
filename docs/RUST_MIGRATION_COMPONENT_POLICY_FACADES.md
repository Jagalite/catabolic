# Persistent Components and fallback Policies facades

2026-10-11 implementation checkpoint. This is a **separate opt-in adapter lane**,
not a migration hard-gate closure, default switch, release, or publication claim.
The larger snapshot remains unqualified while its independent work and reviews
continue. No existing filesystem, fallback-publication, HTTP, or blocked review
gate is relaxed by exposing these methods.

## Selected preserved rows: exactly 10

From the frozen `tests/parity/python-api.json` inventory:

- `python:catabolic.components`
- `python:catabolic.components.revision`
- `python:catabolic.components.index_file`
- `python:catabolic.components.backfill`
- `python:catabolic.components.get`
- `python:catabolic.fallback_policies.Policies`
- `python:catabolic.fallback_policies.Policies.__init__`
- `python:catabolic.fallback_policies.Policies.get`
- `python:catabolic.fallback_policies.Policies.put`
- `python:catabolic.fallback_policies.Policies.listing`

These rows are exposed under the development wheel's `catabolic_native`
namespace, with the exact frozen signatures. The installed default `catabolic`
package is unchanged. Import availability and signatures alone are not exhaustive
behavioral parity.

## Ownership and behavior

The Python modules serialize boundary values and delegate to the existing native
owners. They do not open a second database, invoke the path-reopening companion
`execute_operation`, import the Python engine, execute SQL, calculate revision
hashes, or normalize policy definitions in Python.

`public_components` forwards indexing, historical adoption, revision identity and
row decoding to `components` and `component_mutation`. The persistent PyO3 call
borrows its existing `Session`, keeping the caller's connection, transaction and
savepoints. `backfill(db)` accepts only a native command handle and checks that
exact handle token during the same native call; a transaction handle expires
when its lexical transaction exits. The root `store.db` handle follows the
existing native owner lifecycle. Backfill retains the existing fallback epoch.

`get(session=...)` accepts only a same-owner, same-profile native
`EvaluationSession`. It validates the registered token, epoch, cancellation and
deadline, lends the same Store to the existing evaluator, bounds the unique
occurrence lookup, and charges the raw row array against the same cumulative
byte budget before decoding. Foreign Python evaluation/access objects are
explicitly rejected, not ignored. Currentness still includes the existing live
recorded occurrence/revision evidence check. No source file is probed.

`Policies` uses `fallback_mutation` for immutable revisions, normalization,
digests, dependency records and pagination. The existing owner now preserves
JSON scalar cursor bindings (including NULL) instead of silently turning them
into an empty cursor, accepts Python boolean page limits as the frozen method
does, and checks an unknown package operation before query dependencies. These
are focused public-adapter corrections, not a replacement policy engine.

## Qualification and receipts

The dedicated gate is `scripts/native_component_policy_embedding_tests.py`.
After independent review it declares and enforces **13 distinct, non-skipping
installed scenarios** and **37 named paired differential groups**, tracing every
selected row. Both `-O`
and `-OO` are refused before package import. It checks installed package bytes
against the exact supplied development wheel, records hashes of the wheel,
extension and Python modules, fences its selected source files before/after,
verifies frozen reference provenance and imported source modules, and retains a
separate attempt receipt. Existing reports/attempt receipts are never overwritten.
An explicit development build manifest now binds the entire crate source digest
before and after the build to the exact wheel and installed extension. Missing,
stale-source, stale-wheel and wrong-extension manifests are refused.

The comparison uses the pinned Python 3.14.6 interpreter without resolving its
venv symlink and the frozen `453fca983222c6665a48775eef67c489c96b527e`
source. Each side operates on disposable copies of one initialized seed. Random
policy UUIDs are replaced by consistent labels and creation timestamps are
omitted. Component/revision identity and policy digests remain exact. Structural
policy errors compare the `invalid fallback policy` category only; semantic
errors compare exact messages.

Focused source tests cover all 10 rows/signatures, delegation constraints,
optimization refusal and build identity refusal. Seven native Store tests cover
revision identity, adoption
epoch/rollback, stale-handle refusal, evaluation ownership/budgets, policy
savepoints and scalar cursors. Installed tests also cover historical/current
probe evidence, invalid stream indexes, unknown/profile-isolated occurrences,
closed/detached/read-only owners, snapshots, nested rollback, same-session
cancellation/expiry, policy defaults, revision idempotence and pagination. Live
cancellation is checked after the initial Python boundary read and during native
lookup, including exact callback exception propagation and owner recovery.

Local receipts are retained under `.local-tests/components-policies/`:

- `store-tests-initial.log`: initial 5 Store tests passed
- `store-tests-final.log`: 6 Store tests passed after the cursor regression
- `source-tests-freeze.log`: 3 source tests passed; scoped Ruff checks passed
- `session-regressions.log`: 6 existing Session ownership/cleanup regressions passed
- `clippy.log`: Store adapter target passed Clippy with `-D warnings`
- `qualification-initial.log`: all 12 installed tests passed, then the reference
  import-provenance helper refused because its required `catabolic.app` module
  had not been imported. This harness failure is retained, not counted as a pass
- `qualification-second.json` and `.log`: 12 installed tests and 36 paired groups
  passed after explicitly importing that public module for the shared verifier

That second receipt belongs to a **provisional helperless development wheel**:
SHA-256 `1b01bb16d709374315681aa136fddc0f1c36eb9b14e1c2a2a4fd93588f0644e3`,
with extension SHA-256
`1c71afcbc20ec9047d64ae6e04c9d92ce3c16d136e0119add82046e1c9a01072`.
It predates the final scalar-cursor/error-precedence assertions and concurrent
lanes' final edits. It is not final-source evidence.

A later source-fenced shared development build passed the updated gate:
**12/12 installed scenarios, 36/36 differential groups, and all 10 selected rows**.
The scalar-cursor, boolean-limit and mixed-invalid package-operation precedence
checks are included in that replay. Exact installed bytes, frozen oracle imports,
and before/after selected-source fences held.

- Shared crate source SHA-256:
  `0e0bad440cd231bbabb8fe61ff2a84ab986dde299fed8ca3f2fc41cb6c16c0c8`
- Development wheel SHA-256:
  `efac9db4a6cffe01a7eccdad325f7522aed4a7d0751a8a1b6e08b6a3356052fb`
- Installed extension SHA-256:
  `aca9cd54e7ee3f89b4df305f31cbfdb4ef9102192cdca0d0189603d18b6e9a0c`
- `qualification-phase4-shared.json` receipt SHA-256:
  `661e3c0112e8e2f61325c64ea4fd7eb2f1b4c1648aadd6784d4dc798275ed7a9`
- The `.attempt.json` and `.log` records and the shared build manifest are retained

Those results qualify only that earlier helperless development snapshot. The
production adapter sources were then frozen for independent review. Subsequent
Components and CatalogRefresh corrections required the fresh build and replay
recorded below. No aggregate or release qualification follows.

Example final replay (outside the checkout, in the wheel's isolated venv):

```sh
python /path/to/repo/scripts/native_component_policy_embedding_tests.py \
  --dev-wheel /path/to/exact-development.whl \
  --build-manifest /path/to/matching-build-manifest.json \
  --inventory /path/to/repo/tests/parity/python-api.json \
  --reference-root /tmp/catabolic-embedding-frozen-453fca9 \
  --reference-python /path/to/catabolic-reference-3.14.6/bin/python \
  --report /path/to/new-receipt.json
```

## Independent adapter review

The independent reviewer reproduced the historical **12/36/10** gate against the
exact shared wheel and retained a separate receipt under
`.local-tests/components-policies-independent-review/qualification-shared-replay.json`.
That replay is historical artifact evidence, not a claim about later sources.

The review found two focused defects and corrected them:

- A component evaluation copied `session.cancelled` into JSON only once. A real
  thread changing it during a lookup was ignored. On a valid retained 500 KB
  title fixture, all three 2/4/8 ms cancellation attempts returned successfully
  after about 0.75–0.94 seconds; frozen Python cancelled after 10–12 ms. The new
  bridge reads the live flag through a bounded native call, preserves latched
  Python exceptions, and rechecks epoch/cancellation before charging the raw
  rows. A deterministic regression fails against the old wheel. The existing
  Store is returned on both cancellation and callback exceptions
- The gate previously fenced source stability and installed wheel bytes
  independently without proving that the wheel was built from those sources.
  The mandatory build-manifest check closes that evidence gap; negative source
  tests cover missing manifest and source, wheel and extension mismatches

Additional independent probes matched frozen Python for 23 JSON revision values,
60 scalar-cursor/page combinations, and eight budget/precedence cases: exact byte
boundary, cumulative charging,
uncharged rejected expansions, empty-array charging on unknown lookups, shared
`account()` budget, and cancellation/staleness/deadline precedence. An attempted
two-writer epoch-timing fixture was refused by the existing writer lock on both
implementations; it is not counted as executed concurrent-writer coverage.

The correction passed seven Store tests, four focused source-contract tests,
PyO3 `cargo check`, Clippy with `-D warnings`, Cargo formatting and scoped Ruff
checks. The **13/37/10** installed replay then passed against shared development
build 3, including all ten installed signatures/bindings and exact build identity.
The three real-thread cancellation cases now all return the expected cancellation
error (about 97–100 ms on the debug development build; no performance equivalence
is claimed). All seven installed Session lifecycle/signal regressions and all
twenty native source-contract tests also passed. The earlier source-contract
failure in the separate CatalogRefresh exception import was corrected and
retested; its initial failed log is retained.

- Final receipt:
  `.local-tests/components-policies-independent-review/qualification-shared-3.json`
- Receipt SHA-256:
  `fb07a58fd5f8f12a4f2956fdacfd6cccb389abf157646419b8a421d05e5f8aa4`
- Shared crate source SHA-256:
  `6f9230216379d0a8a74007b8ea3d807b855681cbfc46802901fd6887d1a8371e`
- Wheel SHA-256:
  `922f7ec922275752abd42e7f4103eca6097f5b051b40a460d9bbb5018f4daf0b`
- Extension SHA-256:
  `b915a026371968526a79c307f57a5b3c84961b7579c75aa862c33294bf38e0f6`
- Full independent evidence index: `review-summary.json` in the same directory

The final source and build checks were repeated after the additional probes. No
release, aggregate M4/M10 or full underlying-owner review claim follows from this work.

## Explicit remaining limits

- A helperless development wheel is not release-wheel acceptance; no native
  helper, service, live catalog, remote publication, or real credential was used
- Foreign Python Store/sqlite3 objects, foreign sessions and Access contexts are
  unsupported. Native command handles are not a sqlite3 connection impersonation
- Invalid non-mapping revision inputs use a native `CatabolicError` rather than
  all frozen Python attribute/type failures. Non-JSON Python values, blob or
  memoryview pagination cursors and all invalid non-integer limit exception
  classes are not qualified
- Structural policy validation retains native diagnostic text, not Pydantic's
  full field/error rendering. Unknown-profile validation may occur earlier in
  the native policy owner. Exhaustive malformed definitions, combinations and
  byte boundaries are not claimed
- All corrupt stored evidence/JSON behavior, historical-schema permutations,
  competing-writer faults, signal timing windows and platform behavior remain
  outside this selected qualification
- No fallback Resolver/Projection/Worker, layouts, reconciliation, ASGI app,
  broader review or aggregate parity claim follows from these 10 rows
