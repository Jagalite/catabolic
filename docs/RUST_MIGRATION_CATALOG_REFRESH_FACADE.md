# Persistent CatalogRefresh facade checkpoint

This opt-in checkpoint preserves eight frozen Python inventory rows and adapts
the five existing `catalog-refresh` CLI leaves. It does **not** switch the default
Python package, qualify publication, or close an M4–M11/release gate. The frozen
reference is `453fca983222c6665a48775eef67c489c96b527e`.

## Public boundary

`catabolic_native.catalog_refresh` preserves the module binding, `enqueue`,
`finish`, `CatalogRefresh`, its constructor, `configure`, `pending`, and `run`.
`catabolic_native.CatalogRefresh` exports the same class. Frozen signatures are
checked directly against `tests/parity/python-api.json`.

- The PyO3 adapter borrows the caller's existing `NativeStore` session. It does
  not invoke a path-based companion action or instantiate a Python SQLite
  connection. The existing runtime owners retain responsibility for their
  publication/probe detach/reconnect lifecycle
- `enqueue` passes the native command handle's lexical token into Rust. An
  expired token, cross-thread scope, closed/detached owner, or foreign handle is
  rejected. Queue/watcher changes participate in the producer's transaction and
  its nested savepoints
- The shared Store enqueue owner reuses observation-aware watcher dirtying. The
  existing observation, receipt, and media enqueue functions delegate to this
  owner. This is a narrow queue call-site consolidation, not a new review of
  those subsystems
- `configure` validates recovery, removals and catalog preconditions before
  Python `bool(enabled)`, and validates decoded owner metadata before `int(enabled)`
  conversion, then writes through the same Store configuration owner. The
  control-flow truthiness and SQLite integer parameter are distinct: `'0'`
  stores zero but still invokes the immediate drain; `2` fails the schema CHECK.
  The original Python `enabled` object is retained in the result. The companion
  JSON API remains boolean-only
- Falsey or malformed saved-owner metadata is rejected before configuration or
  queue writes. This configure-time rejection is distinct from per-row retry
  handling during an already-enabled drain
- Watcher-owned targets are skipped before decoding layout metadata. Target
  decoding is inside each row's retry boundary, so a corrupt layout records
  durable backoff and does not prevent later rows from running
- `run` preserves limit/writer/active-transaction checks before `int(force)`
  conversion, including `'0'` and `'1'`. Public and legacy limit diagnostics use
  the frozen wording; the companion diagnostic is unchanged
- `finish` updates the original result object, without serializing or copying
  it. Unrelated arbitrary objects and a pre-existing `catalog_refresh` value
  remain untouched when no work is processed/pending. Supported publication
  errors are bounded to 4,000 characters; cancellation exceptions escape
- Native cancellation captures and returns the original Python exception
  object after the Store is returned and the worker has joined. SQLite progress
  hooks are removed on completion/error/unwind

There is no public `disposable_fixture` argument. The public runtime adapter
ignores companion-only fixture options, and the existing fallback-publication
gate remains fail-closed. A dedicated installed fixture checks both the public
method and attempted private request-field injection without creating any
output roots, media, services, or credentials.

## Exact legacy CLI leaves

The frozen commands are `catalog-refresh enable`, `disable`, `pending`, `run`,
and `watch`. There are no new `configure`, `list`, or `retry` CLI leaves. The
separate legacy `refresh` family is unrelated and unchanged.

The existing frozen parser still owns flags/defaults. Each operation now uses
the narrow native catalog-refresh adapter. Watch:

1. Validates its limit and 1–60 second interval
2. Opens one writer, runs one bounded due-work batch, then closes the writer
3. Catches tick-level operation/opening errors and prints the frozen report
4. Prints nothing for an idle empty batch; flushes JSON lines for processed work
   or errors, with the frozen `json.dumps` field order/ASCII behavior
5. Waits without holding the writer or a transaction, then repeats until
   interrupted; SIGINT exits 130 through the existing legacy renderer

No invented cycle-count flag, finite success loop, scan, consumer refresh/TMDB
adapter, or fixture bypass was added. Bounded tests observe two genuine ticks,
open a second writer during idle, and externally interrupt the process. Both
quiet-idle and repeated opening-error behavior are exercised.

## Verification and artifact identity

Source contract gate:

    python -m unittest tests.test_native_catalog_refresh_contract -v

Store-owner regressions:

    cargo test --locked -p catabolic-store catalog_refresh::tests

Installed development checkpoint, run outside the checkout:

    /isolated/venv/bin/python -I /repo/scripts/native_catalog_refresh_embedding_tests.py \
      --dev --wheel /artifacts/catabolic_native.whl \
      --native-binary /artifacts/catabolic-native \
      --legacy-binary /artifacts/catabolic-legacy-native \
      --build-manifest /artifacts/development-build.json \
      --source-root /repo --inventory /repo/tests/parity/python-api.json \
      --reference-root /frozen \
      --reference-python /frozen-venv/bin/python \
      --report /artifacts/catalog-refresh-proof.json

The manifest must contain `profile: "development"`, `source_sha256` from
`scripts/build_native_wheel.py::source_digest`, and the exact wheel, native
binary and legacy binary SHA-256 values under `wheel_sha256`,
`native_binary_sha256`, and `legacy_binary_sha256`. Build source must be fenced
before and after the shared build; these receipts are local identity evidence,
not a hermetic-build claim.

The harness rejects optimized Python and existing report paths, requires the
exact 16 distinct tests to succeed without skips, records method-specific calls
for all eight inventory rows, and pairs 43 result groups with the pinned oracle.
It verifies every installed development-wheel payload, imported extension,
package extras, frozen source/import identity, source content, helper binaries,
inventory, build manifest, and qualification scripts. Input identities are
rechecked after execution. The executable companion is explicitly identified;
facade scenarios do not use it to reopen the catalog. The helperless wheel is
explicit development evidence and never passes as a final helper-paired release
artifact.

### Executed provisional evidence

- Three source tests passed, including optimized-Python rejection
- Three native Store tests passed: producer rollback/coalescing, unchanged strict
  companion/schema constraint, and late-generation acknowledgment/retry fencing
- Ten installed lifecycle tests passed against the first provisional wheel;
  the additional installed fail-closed-publication test passed separately
- Three paired CLI tests passed, including nine operation/error comparisons,
  exact watch lines, idle writer release, no idle output, repeated opening
  failures, and SIGINT 130
- The provisional 39-group pair exposed two limit-error wording differences;
  these were corrected before the final shared build

Final source/artifact hashes and a fresh combined 15-case/41-group result belong
in the generated receipt and parent integration report. Provisional logs are
retained, including failed fixture assumptions and the diagnostic drift; they
must not be promoted to final-build evidence.

### Source-stable shared build, before malformed-layout correction

The combined **14/14 cases, 39 paired groups, eight called rows** passed against
source `0e0bad440cd231bbabb8fe61ff2a84ab986dde299fed8ca3f2fc41cb6c16c0c8`,
wheel `efac9db4a6cffe01a7eccdad325f7522aed4a7d0751a8a1b6e08b6a3356052fb`,
and explicitly identified copied helpers. Receipt SHA-256:
`a427641671755586fec8e669f023a9eafeef0979d3218ba48bd235b09b8e8d87`.
Shared clippy also passed for `catabolic-python` and `catabolic-cli`, all targets,
with warnings denied.

A subsequent narrow frozen comparison reproduced malformed-layout handling:
the frozen worker skips a corrupt watcher-owned row and retries an unowned
corrupt row; the earlier native owner aborted the batch in both cases. The
correction moves target decoding inside the per-row attempt and skips owned
metadata before decoding. The new fifteenth test and two additional paired
groups verify a three-row batch: owned corrupt row unchanged, corrupt unowned
row retrying, and a later missing-layout row also retrying. This makes the
previous shared receipt historical, not final-source evidence.

### CatalogRefresh-tested snapshot after the correction

The source-stable installed gate passed **15/15 distinct cases, 41 paired result
groups, and all eight called inventory rows** in 16.774 seconds. It ran outside
the checkout under isolated Python 3.14.6, using the unchanged frozen reference
interpreter and explicit development artifacts:

- Source: `cd451bd06348245d7617a60d3bb7d55396e45f12aa3f549f27cb9997e7936673`
- Wheel: `f36dfe4b9dd1e426ad59456df1e7954471ed18c5e7afae96e70f2a4c68f42e33`
- Companion: `4853efd835db3a35575d672680b2c4eae60693068d702eb4cec66144d952f300`
- Legacy executable: `4a9a8175a175c455e3571173e4f123bdaaa9b3f0e6b02a6ade604ed9d5479d0a`
- Extension: `b918b98590eca6b4f98cf7762fe4239bba16451cf2980f2bb6c5a56c152fb961`
- Receipt: `bb0be4b022a270ea8572bc2c71cb8eaee2c05f59c768af017431fbfaf92e68ff`

The receipt records exact successful test identities, 12 installed facade cases,
three paired CLI cases, per-row call counts, all wheel payload/script/source
identities, and unpaired parser/opening diagnostic text. The three source tests,
three Store-owner cases, formatting, Ruff, diff checks, and all-target clippy
with warnings denied also passed after the correction. Later integration edits
require new shared-artifact qualification; these results qualify only the
recorded snapshot and selected contracts.

### Independent configuration-adapter corrections

Independent review repeated all 15 lifecycle/CLI cases against the recorded
wheel, passed the three then-current Store cases, and independently confirmed
the watcher-owned malformed-row skip, retry/continuation, and exact Python
exception restoration during cancellation of a retry write. The changed-source
gate correctly rejected the historical manifest without issuing a new receipt.

Additional frozen comparisons found that configure only checked existence of
saved-owner metadata: falsey or malformed values incorrectly enabled refresh and
mutated its queue. Configuration now decodes and checks that metadata before
writes. Nine installed fixtures preserve the complete preexisting settings and
queue rows after rejection; Store regressions also check initially empty rows
and rejection of non-text metadata without panicking.
Two throwing-object comparisons check removals/catalog errors before enabled
truthiness. These add one installed case and two paired groups.

The independent, source-stable build-3 gate passed **16/16 distinct cases, 43
paired result groups, and all eight called inventory rows** in 16.458 seconds.
The current-source Store tests passed 5/5, and the two source-contract suites
passed 12/12. Independent final-wheel signal testing also confirmed exact
cancellation identity, rollback of a retry write, and subsequent owner reuse.

- Source: `6f9230216379d0a8a74007b8ea3d807b855681cbfc46802901fd6887d1a8371e`
- Wheel: `922f7ec922275752abd42e7f4103eca6097f5b051b40a460d9bbb5018f4daf0b`
- Extension: `b915a026371968526a79c307f57a5b3c84961b7579c75aa862c33294bf38e0f6`
- Receipt: `16475a04a8ea481a24f3ddeae952d448daa00b0a1ac8c3c47809e9086dddf00f`
- Receipt path: `.local-tests/catalog-refresh-independent-review/final-proof-build3.json`

All post-run source, installed-payload, helper, frozen-source, inventory,
manifest, and script identity checks passed. This remains explicit helperless
development evidence for selected contracts, not release qualification.

The only Python SQLite dependency is an exception-class import for frozen
`finish` behavior. A structural AST guard permits exactly that alias in the
exception handler and still rejects module/engine imports and SQL calls.

## Explicit remaining gaps

- Real output publication, layout/fallback recovery, notification delivery,
  producer integration suites, filesystem races, all boundary/input shapes, and
  promised platform/Python combinations are not qualified by these fixtures
- `enabled=2` preserves the SQLite CHECK failure and message, but the native
  Store's general error boundary maps `sqlite3.IntegrityError` to
  `CatabolicError`. That exception-class difference is explicitly unqualified;
  the paired scenario compares the constraint message and records this gap
- Non-string observation-job/profile values, arbitrary catalog parameter
  objects, huge SQLite integers, and every custom coercion object are not
  independently qualified
- Malformed-layout JSON parser diagnostics differ (`invalid JSON` versus the
  frozen decoder's detailed location text). The retry/continuation comparison
  preserves row identities, attempts and complete report structure; both raw
  diagnostic sets are recorded separately instead of counted as exact text parity
- Configure-time malformed JSON rejects before mutation in both implementations,
  but its exception class is native `CatabolicError` versus frozen
  `JSONDecodeError`; this diagnostic is recorded separately too
- Missing-catalog watch errors are asserted to recur with the correct report
  envelope and cancellation behavior. Their platform/Store-specific diagnostic
  text is recorded separately rather than normalized into exact parity
- Earlier blocked broad definition/filesystem reviews and all other publication,
  rollback, performance, platform, independent-review, and release gates remain
  open independently of this facade checkpoint
