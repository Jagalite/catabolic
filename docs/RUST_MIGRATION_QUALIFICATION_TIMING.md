# Qualification fixture timing

## Scope

This is a test scheduling correction based on `7c0cb4f`. It changes no production
source, timeout default, authentication clock, plan admission rule, frozen oracle,
or native artifact. The frozen oracle remains
`453fca983222c6665a48775eef67c489c96b527e`.

The shared component-package fixture now explicitly requests a 10,000-ms probe
budget. Its lifecycle and media callers intend to verify successful selection and
publication, including exact preview/apply plan admission and no-op publication;
they do not intend to impose the production default's 1,000-ms interpreter-startup
deadline. Existing separately configured timeout and stale-plan tests are
unchanged. The shared maximum-checks and maximum-candidates controls remain in
force.

The controlled HTTP authority fixture now starts with five seconds of token
validity and a 10,000-ms probe budget. The frozen oracle records/asserts entry into
its controlled probe callback before expiry; the native fixture records/asserts
entry into its detached helper before expiry. Both then wait for real wall-clock
expiry before allowing resolution to continue. A missing probe entry fails the
test; an early native HTTP response now appears in the failure diagnostic.
Revocation still expects the exact 409 epoch-fence response, and natural expiry
still expects the exact 401 reauthentication response. Job and probe-slot cleanup
assertions are retained.

There are no retries-as-pass, skipped scenarios, probe-result replacements, clock
patches, regenerated expected plans, or result normalization beyond the existing
request-ID handling. Execution uses disposable catalogs/media and numeric
loopback only. No recovery-race or broad owner-semantics review is included.

## What the original receipts establish

- The completed 1,130-test reference receipt in
  `.local-tests/rust-migration/resumed-validation/reference-final-results.json`
  failed in `test_component_fallback_publication_and_noop` with
  `stale_or_missing_resolution_plan`. It also records one filesystem-dependent
  skip. The source fence passed, but the run did not capture the failing probes
  or both plans.
- The completed 877-test native receipt in
  `.local-tests/rust-migration/m4-plus/native-results.json` records the same stale
  plan signature during the reference publication portion of
  `test_selected_streams_packet_evidence_and_reference_publication`. It also
  records a missing-helper-marker assertion in
  `test_natural_token_expiry_during_component_probe_reauthenticates`, plus other
  failures outside this correction's scope. Source and artifact fences passed.
- The prior reference fixture diagnostic observed 26/27 ordinary repetitions
  passing; the remaining repetition reported a genuine `probe_timeout` later in
  no-op publication. Its separately labeled 1.25-second subprocess-start delay
  reproduced the original stale-plan signature in 3/3 controlled trials.

These records establish timing sensitivity and a reproducible mechanism. They do
not establish the precise cause of either historical aggregate failure. The
historical failed receipts and their immutable artifacts are preserved and are
not relabeled as passing by this change.

## Controlled regression evidence

1. Before changing the auth fixture, delay its native request by 1.25 seconds
   after arming the original 0.8-second lifetime. The unchanged test fails at the
   exact missing-helper-marker assertion, while the actual response is HTTP 401
   `unauthorized` and the helper marker does not exist. This is correct early
   admission refusal, not evidence of a production authentication defect. The
   local record is `early-expiry-before-fix.json` in the isolated worktree's
   `.local-tests/qualification-timing/` directory.
2. The checked-in early-expiry control applies that same 0.8-second lifetime and
   1.25-second pre-request delay independently to oracle and native requests.
   Neither may enter the probe, both must return the exact unauthorized response,
   and neither may admit work or retain a probe slot.
3. A separate checked-in delayed-request control uses five seconds of validity
   with the same 1.25-second pre-request delay. Both probe-entry assertions must
   succeed before expiry, followed by the same natural-expiry reauthentication
   checks as the original test.
4. `tests/probe_timing.py` delays one matching real child interpreter by 1.25
   seconds and then executes the original frozen probe module. It does not alter
   the deadline, result, or production source. Both the lifecycle fallback/no-op
   scenario and the native media/reference-publication scenario succeed with
   the explicit healthy-path budget and their existing assertions.
5. A negative lifecycle control explicitly restores a 1,000-ms policy budget.
   The same delayed missing-subtitle child must actually return `probe_timeout`;
   the next un-delayed evaluation must still reject the preview's exact plan ID
   with `stale_or_missing_resolution_plan`, without publishing any symlinks.

## Verification

The scoped test results and immutable-artifact checks are recorded in
`.local-tests/qualification-timing/` in the isolated worktree. They qualify this
fixture correction only, not the complete migration or either aggregate.

The initial development run rejected a proposed 20,000-ms auth-fixture budget at
schema validation. It was corrected to the existing supported 10,000-ms maximum;
no policy bound was changed. That initial failed development log is preserved as
`focused-first.log`.

Final scoped results:

- `http-privacy.log`: all 23 HTTP component-privacy tests passed, including the
  two new request-delay controls; no skips (58.606 seconds).
- `component-fixtures.log`: all 25 selected component tests passed; no skips
  (123.303 seconds). This includes all eight package tests and nine lifecycle
  tests, the original frozen media publication scenario, five native media
  publication/extraction scenarios, and the existing native maximum-checks and
  maximum-candidates controls. Recovery-race tests were not selected.
- Ruff format/check and `git diff --check` passed for the final fixture changes.
- The production paths have no diff from `7c0cb4f`; the frozen oracle checkout
  remains clean at the pinned commit. Native, HTTP, legacy, and extension artifact
  hashes match both the pre-check hashes and the preserved native receipt. No
  rebuild was needed. `verification.json` records these checks and final test
  source hashes.

The native artifact used here has SHA-256
`6e318f24ab964e4c3a85d35d3882f5602eb9042aa77003bb5437b5ec7155a7ad`;
the HTTP artifact has SHA-256
`28b1567948778d71f39506ffaaf23828b91bbeaef86abf3f7d31c82a11227081`.
These are the preserved qualification artifacts, not a new production build.

For the same isolated-worktree setup, the commands are:

```sh
source ../catabolic/.local-tests/rust-migration/env.sh
export CARGO_TARGET_DIR="$PWD/target"
export PYTHONPATH="$CATABOLIC_PYTHON_REFERENCE:$PWD"
export CATABOLIC_NATIVE_BINARY="$CATABOLIC_REPO/.local-tests/rust-migration/m4-plus/artifacts/qualification-native"
export CATABOLIC_HTTP_BINARY="$CATABOLIC_REPO/.local-tests/rust-migration/m4-plus/artifacts/qualification-http"

python -m unittest -v tests.test_rust_http_component_privacy

python -m unittest -v \
  tests.test_component_packages \
  tests.test_component_lifecycle \
  tests.test_component_media.MediaTest.test_mux_selected_streams_then_resolve_verified_artifact \
  tests.test_rust_component_media.NativeComponentMediaTest.test_selected_streams_packet_evidence_and_reference_publication \
  tests.test_rust_component_media.NativeComponentMediaTest.test_selected_streams_publication_tolerates_delayed_reference_probe \
  tests.test_rust_component_media.NativeComponentMediaTest.test_external_audio_subtitle_and_exact_accepted_offset \
  tests.test_rust_component_media.NativeComponentMediaTest.test_font_attachment_exact_bytes_and_lineage \
  tests.test_rust_component_media.NativeComponentMediaTest.test_exact_reference_extraction_plan_and_native_publication \
  tests.test_rust_component_selection.NativeComponentSelectionTest.test_component_live_checks_share_resolution_budget \
  tests.test_rust_component_selection.NativeComponentSelectionTest.test_component_candidate_budget_is_shared_across_requirements
```

## Native aggregate fixture provenance

The aggregate at `721b80f` stopped during import preflight, before any Python
scenario ran: the original guard required every borrowed `tests.*` module to
match the frozen inventory, and therefore rejected the reviewed new
`tests.probe_timing` helper. The existing live receipt was replaced with its
`started` marker. This failure did **not** leave an earlier receipt in place
and did **not** establish any Python scenario result. The original failure log
and the six archived `7c0cb4f` receipt/artifact files remain unchanged.

The harness now reads `tests/parity/native-fixture-adaptations.json`. This is an
explicit, source-fenced admission list for precisely the three files reviewed in
`340cd70e76ba0e4b4ffbe7b697a9ce4520cd708c`:

- `tests/test_component_packages.py`: a fixture adaptation with both its original
  frozen hash and reviewed current hash
- `tests/test_component_lifecycle.py`: a fixture adaptation with both hashes,
  including when first imported lazily by a native selection/media scenario
- `tests/probe_timing.py`: a separately identified helper with no frozen baseline
  and an exact reviewed current hash

Frozen application bytes and the original frozen fixture copies must still
match the unchanged inventory. Every other borrowed fixture must match that
inventory exactly. Unknown borrowed modules, origin escapes, module-name/path
mismatches, linked file or directory components, and unrecorded frozen package
or fixture payloads are rejected. Native-only test modules are individually
bound to the qualification's initial local source snapshot, including their
origin and exact import path; their naming prefix no longer bypasses those
checks. This is a trusted local source-content fence, not hostile-code isolation
or a hermetic-build claim.

Receipts use `import_provenance_before` and `import_provenance_after`, with explicit
`frozen-application`, `frozen-fixture`, `reviewed-fixture-adaptation`,
`reviewed-fixture-helper`, and `native-qualification-test` labels. The complete
reviewed manifest and its hash are retained separately. Existing imported
identities must remain stable; lazy imports receive the same checks. The source,
manifest, frozen resources, historical fixtures, and binary-artifact checks
remain required before a completed pass can be reported. No fixture was renamed,
no test result was normalized, and no frozen inventory or expected plan changed.

A separate reproduced early-preflight gap existed for malformed arguments and
optimized Python: explicit report and attempt paths were left untouched before
argument parsing. The aggregate now uses the canonical shared cleanup scanner,
verified AST-equivalent to `native_facade_qualification.clear_stale_report`.
Only canonical `--report PATH` and `--report=PATH` opt in; all repeated explicit
paths are invalidated, abbreviations are rejected, `--` ends scanning, and
unrequested paths remain untouched. Library imports do not clean caller reports.
A valid invocation writes a non-passing `started` receipt with `phase: preflight`
and zero tests; after successful preflight the phase becomes `running` without
claiming a completed count. Only completed result collection records the executed
scenario count and outcomes.

Final repair checks, retained under the isolated worktree's
`.local-tests/native-fixture-provenance/`:

- 53 focused provenance/artifact/shared-receipt tests passed without skips on
  both Python 3.14.6 and 3.11.15
- The actual aggregate CLI ran all 17 new provenance/receipt controls and emitted
  a passing, source-stable and artifact-stable scoped receipt
- Six selected real media/selection scenarios ran through aggregate preflight,
  execution and postflight, including the delayed probe and lazy lifecycle import;
  all passed without skips with both fences intact. The selection wrapper is
  retained as `run_selected_media.py`; it only selects the six named scenarios
  and does not replace production behavior, assertions or qualification checks
- Those real scenarios used the preserved `7c0cb4f` native artifact with SHA-256
  `6e318f24ab964e4c3a85d35d3882f5602eb9042aa77003bb5437b5ec7155a7ad`.
  They are segmented harness evidence, not qualification of current `721b80f`
  product artifacts
- Final discovery admitted 914 unique native scenario IDs with no discovery
  errors. Its receipt explicitly records discovery-only and zero executed tests
- The 518 frozen resources, archived historical receipt/artifacts, original
  failing log, Ruff checks, and `git diff --check` were verified

The source-content digest for those final checks is
`9b4f04f556323e085bb5ad6749bdfb4f690853067b9af7d66b4fa4e26b85572b`.
`verification.json` binds the logs, scoped receipts, final changed files, archived
artifacts and interpreter versions. Fresh independent final review and a full
aggregate on the final integrated product artifacts remain required. No Cargo
build, full aggregate run, product change, push, merge or release is claimed.

### Installed client dependency links

Integration at `40b9d1d808496b063fb16f761965d0e0a6d3da94` exposed a separate
source-fence portability issue before the aggregate started. The existing pinned
npm installation contains 19 normal executable aliases under
`tests/clients/node_modules/.bin`. Rejecting every source-tree link also rejected
those dependency aliases. No new aggregate scenarios ran in that attempt.

The bounded follow-up retains every historical source/dependency file and
content hash, including each alias's target-byte entry. Source snapshot schema 2
adds `dependency_links` and includes that map in the main `sha256`; the historical
file-map digest remains separately available as `files_sha256`. Each admitted
alias records its exact relative link text, in-package target path and SHA-256,
lockfile path and SHA-256, and locked package name/version.

Admission is limited to a canonical relative link directly under that one `.bin`
directory. Its alias and target must exactly match the owning package's `bin`
entry in the checked-in version-3 client `package-lock.json`. The target must be
a regular file inside that package. Absolute/escaping/noncanonical targets,
unknown or mismatched aliases, dangling links, directory targets, chained links,
and linked ancestors or lockfiles are rejected. All other source/dependency
links remain forbidden. Frozen and imported Python paths still use the strict
no-linked-component guard. No dependency tree or unrecognized regular payload
is silently omitted from the content inventory.

The follow-up evidence is retained in `.local-tests/native-dependency-links/`:

- All 62 focused provenance/artifact/shared-receipt tests passed without skips
  on Python 3.14.6 and 3.11.15
- The real aggregate CLI passed all 25 provenance/receipt controls with stable
  source and preserved-artifact fences
- Negative completed-receipt controls reject equal-byte link-only retargeting,
  target-byte changes and lockfile changes; direct controls cover malformed
  aliases, link/ancestor chains and unknown payload coverage
- Both interpreters ran the new snapshot against main's existing dependency
  layout using read-only inputs. All 14,318 historical file/hash entries remained
  identical, including 13,336 dependency paths and all 19 aliases. Their shared
  historical digest is
  `79f8042a8fe3ea6541477f3930b5c6175c79f691441b423e8cc0fe8158015ddc`;
  the schema-2 digest for those same main inputs is
  `cd99be9aea727e6dd32d869ef6b01e34c5899e05175a5d55db32cb1492c5c790`

These are source-fence and harness checks. Main, its installed dependencies, the
frozen oracle and archived artifacts were not edited. No npm installation,
generated-client rebuild, Cargo build or full aggregate replay was performed.
Fresh independent harness review remains required before integration.
