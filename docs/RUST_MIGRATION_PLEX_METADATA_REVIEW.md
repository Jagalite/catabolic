# Independent native Plex metadata checkpoint review

Reviewed 2026-10-11 against frozen Python reference `453fca983222c6665a48775eef67c489c96b527e`, using Python 3.14.6. This is a scoped opt-in checkpoint review. Python remains the default; this is neither full parity nor release approval.

## Result

No additional native implementation blocker was found within the reviewed metadata scope. Two claims needed precision, now documented and tested:

1. **Historical import evidence does not always pin a full part set.** When `remote_paths` is absent, both frozen Python and native allow an added remote part after PUT if the GUID, individually proven path and selected fields remain valid. With explicit `remote_paths`, the identical added-part schedule produces `uncertain` in both implementations. The prior unconditional full-part-set guarantee was corrected. This limitation was not silently tightened away from frozen behavior.
2. **Exact-row acknowledgement is a native safety tightening.** Deleting the started event during PUT produces `metadata_attempt_changed` and no successful response in native. Frozen Python reports verified even though its UPDATE affects zero rows. The independent test establishes both outcomes. Native does not replay the PUT.

The previously recorded last-detached-read local fence remains a named native safety tightening. The shared UTF-16 XML limitation remains open and was deliberately not changed during this frozen review.

## Scope and findings

Source review covered the new Store/runtime metadata modules, `Adapter::put` and shared identity/library namespace lookups, companion CLI metadata routing, `plex_login::with_cli_signals`, and PyO3's `execute_plex_metadata_with_poll` route. Existing matched-publication, Store, definitions, and other owners were treated as collaborator APIs. Previously blocked broad definitions/interchange and filesystem/source-access safety reviews were not retried or replaced by this review.

- Whole-batch preview validation, selected fields, ordered explicit clears, encoded PUT limits, plan digest comparison, local evidence/connection checks, and before-state validation precede intent and PUT
- Started events are observable from another Store before the remote effect; verified updates use the same event ID
- Changed selected values and selected-but-unchanged values/locks are checked on readback; unselected fields are not written or accidentally included in verification
- GUID replacement and evidence-pinned full-part-set replacement after the effect produce durable uncertainty
- A PUT redirect produces durable uncertainty, does not follow the redirect, and does not repeat the request
- Scoped cancellation at post-PUT detail readback retains the started event, preserves the exact Python `BaseException` instance and existing signal handlers, and leaves the owner reusable without replay
- Invalid JSON/request shapes raise recoverable Python `RuntimeError`, with no network requests or intent
- A controlled Rust owner-poll panic unwinds the Store and publication locks; pre-cancelled execution also releases both locks with no intent

These are bounded test findings. They do not establish arbitrary worker-panic recovery, exhaustive instruction-level cancellation schedules, all process-crash windows, real services, real catalogs, macOS, performance, final paired artifacts, M6, or aggregate M4–M11 release qualification.

## Executed evidence

All tests used disposable catalogs, synthetic media and fake credentials. All HTTP reached numeric loopback. No commit, push, PR, release or live-service action occurred.

- Immutable CLI: **62 tests passed**, comprising all 48 metadata owner cases and 14 independent cases. The final 14-case review file was rerun after its final edits and also passed
- Installed development wheel, outside the checkout with `-I`: **2 independent cases passed**, covering recoverable request errors and deterministic post-PUT readback cancellation. This does not promote the development wheel to a final helper-paired artifact
- Immutable copied runtime test executable: **2 metadata units and 1 CLI signal-handler restoration test passed**
- New native integration file: **2 tests passed** under `cargo test --locked -p catabolic-runtime --test plex_metadata_review`
- `cargo clippy --locked -p catabolic-runtime --test plex_metadata_review -- -D warnings`, scoped rustfmt, Ruff lint and Ruff format checks passed

Independent tests are in `tests/test_rust_plex_metadata_review.py`, `scripts/native_plex_metadata_review_tests.py`, and `crates/catabolic-runtime/tests/plex_metadata_review.rs`. The independent Python subprocess tests reuse the owner's fixture transport and frozen setup but define their own schedules and expectations; they do not claim an independently implemented oracle. The Rust cleanup tests compile the current integration harness against the unchanged metadata implementation; they are separate from immutable-binary evidence.

The initial independent run deliberately retained its failed full-part-set expectation as historical evidence. Both implementations had returned verified; investigation established the optional historical provenance field. It was replaced by the explicit two-case regression described above, rather than being treated as an implementation fix.

## Artifact identities

- Immutable CLI SHA-256: `a0047b1c71069045048845ea3a434ac1d1a3bf532bc6421d29a0d0ab3efee812`
- Installed/copied extension SHA-256: `1d8c0b9ea860d9e5b02c5a719bd2c94301b055482f22e6f0968d610082e72bcd`
- Copied runtime test executable SHA-256: `86217884e111858ee17688fc4d7016692d3a803b51095286b04b81b7514e4c36`
- Runtime metadata source SHA-256: `5ea824d19492346450a2b405f2553bfaceb7f0740fb8be0403418a5a2cd2bbd8`
- Store metadata source SHA-256: `d26be6fea5088519d56c35a6fea4f01729e766880457614c28f22626f36a7931`

The new tests and documentation are review changes; the metadata implementation and copied artifacts were not modified. The original owner documentation hash therefore remains a historical handback identity, not the identity of the corrected document. Local review receipts retain commands, outputs and expanded source/test hashes.

## Rerunning the review driver

The current driver requires either `--wheel PATH` for strict installed-wheel verification, or explicit `--dev --expected-extension-sha256 SHA256` for development-only evidence. The historical extension identity above is not an implicit default. These modes are mutually exclusive.

Both modes require exactly the two named independent cases to finish successfully. Empty, skipped, duplicated, unexpected, missing, errored, and failed cases cannot produce a passing receipt. Receipts record successful case IDs and the shared result-verifier hash; strict mode rechecks the installed wheel after the cases, and both modes recheck the extension bytes. These driver changes do not rewrite or upgrade the historical receipts above.
