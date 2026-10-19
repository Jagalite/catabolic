# Native reviewed Plex metadata checkpoint

Reference: `453fca983222c6665a48775eef67c489c96b527e`. This is an opt-in implementation checkpoint, not a completed migration or release gate. Python remains the default. All qualification uses separate disposable catalogs, synthetic files, fake credentials and loopback HTTP services. No live Plex service or real user catalog was used.

## Interface and owners

```sh
catabolic-native --db /disposable/catalog.db operation plex-metadata run --options \
  '{"connection":"home","library_id":"7","item_ids":["fixture-item"],"fields":["title","summary"]}'
```

Select imported `item_ids` or accepted `match_ids`, with `fields`, optional `clear_fields`, `lock_fields` / `unlock_fields`, and `apply` / `expected_plan`. Apply requires the exact preview digest and rereads the full local and remote evidence. The separate legacy CLI owner routes `consumer metadata`; the companion JSON interface itself does not establish legacy renderer/exit parity or a public persistent `catabolic.plex_metadata` facade.

- `catabolic-store/src/plex_metadata.rs` owns selection/options, kind-specific scalar values, imported provenance, matched publication snapshots, complete reviewed-plan hashes, and durable consumer events
- Matched snapshots call the existing `plex_matching::matched_publication` owner. That owner retains source, binding, mapping, primary association and live published-link fences. This metadata owner neither changes published links nor bypasses the M6 managed-output release restriction
- `catabolic-runtime/src/plex_metadata.rs` owns detached bounded reads, complete detail identity and lock validation, exactly one PUT per changed item, verification, cancellation and batch-stop behavior
- The existing consumer Adapter owns credential intake, no-redirect HTTP, fresh connections, time/byte/XML limits and failure classification. A narrow `put` method reuses that transport without automatic request replay
- `api::execute_plex_metadata_with_poll` is the shared cancellable boundary. PyO3 retains the original signal exception after cleanup. The companion and legacy executable paths use a synchronous atomic-only scoped signal guard shared with Plex login; embedded execution never installs process-global signal handlers

## Review and publication guarantees

The whole bounded batch is validated before any intent or remote write. Imported items require exactly one Plex server/library/rating-key identity and accepted import provenance tied to an active primary file association. Accepted matches reuse their original metadata source and publication evidence. Duplicate remote targets, changed GUIDs, missing proven paths, malformed field locks, missing fields and incompatible item kinds are refused. Full part sets are pinned only when the provenance includes `remote_paths`. Historical imported evidence without that field proves its individual `remote_path`; matching frozen behavior, an additional part appearing after PUT can still produce a verified result if the GUID, proven path and selected fields remain valid. This is not a full-part-set readback guarantee for that historical evidence.

Selections contain 1–100 supplied IDs before deduplication. Scalar text is limited to 8,192 characters, encoded PUT parameters to 16 KiB, and the complete review payload to 4 MiB. The digest includes profile, complete connection, library identity, database/local provenance, selected fields, lock options, ordered explicit clears and every remote before-state/change. Selected field locks are preserved unless explicitly changed. Unselected fields are never sent. Selected unchanged fields are checked again on readback.

Apply holds the frozen-compatible `<database>.plex-metadata.lock` publication lock. It releases the independent Store connection and writer lock during every HTTP request, allowing ordinary catalog writers to progress while refusing another publisher for the same catalog. Before each changed item it validates the server/library, whole local batch and remote before-state. It persists a `metadata_publish` started event before PUT, then validates identity, reads back the item, compares all selected values/locks, and rechecks the whole local batch before a verified acknowledgement.

An ignored write, HTTP error, lost response, identity change or verification mismatch after the intent becomes `uncertain`; it never silently retries PUT. A pre-intent conflict is `blocked`. The batch stops at the first such outcome and reports remaining item/match IDs. Cancellation leaves the durable started event if admitted, joins the outstanding request, and prevents the next request or verified acknowledgement. A replaced/unavailable catalog that cannot safely be reattached is never used to acknowledge success; its existing started event remains unacknowledged. A new preview is required for recovery, and already-observed matching values produce `unchanged` without another write.

Compatibility limits and safety differences are explicit:

1. Native rechecks the whole local snapshot after the final detached pre-PUT detail read. The frozen owner could send a write after a concurrent local edit at that point, then report uncertainty after the effect; native blocks before PUT
2. The frozen `Plex.xml` parser accepts UTF-16 XML bytes, whereas the shared native XML adapter currently only accepts UTF-8. Native fails closed with `invalid_response`. This is a recorded compatibility gap, not qualified parity; the fixture demonstrates both behaviors. Safe bounded decoding and DTD handling require a separate shared-adapter change
3. Native acknowledgement requires updating exactly one matching `metadata_publish` event. If an ordinary catalog writer deletes the admitted intent during PUT, native refuses a verified acknowledgement with `metadata_attempt_changed`; frozen Python does not check that UPDATE's affected-row count. This is a fail-closed safety tightening, not exact parity for that concurrent-deletion schedule

Shared discovery now follows ElementTree's namespace behavior: identity attributes, library attributes, Directory and Location children must be unqualified; a foreign namespace cannot shadow a genuine field. `inspect` still does not impose a new root-tag requirement. Other legacy consumer XML leaf readers and broad encoding compatibility remain separate shared-adapter work.

## Executed and pending evidence

`tests/test_rust_plex_metadata.py` retains all twenty frozen metadata scenarios, including the wire test through a real native loopback PUT, and downstream accepted-match cases. Preview/apply comparisons use isolated Python/Rust databases and independent fake remote states, compare entire plans including digests, emitted PUT parameters, resulting catalog/provenance/event rows and remote state. Only event UUIDs and event creation timestamps are normalized. Additional cases cover real process interruption, no request after cancellation, store-writer progress, publication contention, lost responses, replacement fences, namespaces, value/request/review bounds and the explicit UTF-16 gap.

`scripts/native_plex_metadata_embedding_tests.py` runs outside the checkout with an installed wheel. It independently checks interruption before/after an effect, exact custom Python exception identity, joined worker cleanup, durable started/uncertain outcomes, and no replay after a new preview observes the existing effect. The development wheel does not contain a final paired helper: its test receipt is development evidence, not final artifact qualification. Supplying `--wheel` intentionally requires the stricter existing final-wheel verifier.

Focused native unit tests exercise scalar/date bounds, option normalization, event mutation ownership, encoded parameters and full detail identity/selected values. The shared login signal-handler restoration and publication interruption regressions are rerun after extracting its CLI guard. Shared consumer/import/matching regressions cover the narrowly changed adapter behavior. The immutable copied CLI passed 130 subprocess tests: 48 metadata (including one explicit known-gap UTF-16 test), 32 import, 26 matching and 24 shared consumer cases. Five metadata Rust tests, four installed-development-wheel cases, the login handler-restoration test and the eight-schedule CLI/Python login publication-interruption regression passed. Strict Store/runtime all-target Clippy and scoped Rust/Python formatting/lint checks passed. Exact commands/results and hashes are retained in the local qualification receipts.

A [scoped independent metadata review](RUST_MIGRATION_PLEX_METADATA_REVIEW.md) subsequently reran all 48 metadata scenarios against the immutable CLI and added 14 independent subprocess cases, two installed-development-wheel cases, and two native cancellation/panic cleanup cases. It corrected the historical part-set guarantee above and documented the exact-row acknowledgement difference. That review covers this new metadata owner and its narrow adapter/signal/embedding routing changes; it does not substitute for the separately blocked underlying safety reviews or the remaining release gates.

No Linux-only fixture result establishes macOS behavior, real-service acceptance, exhaustive crash/cancellation coverage, safe real-catalog handover, final paired-artifact integrity, M6 output qualification or an M4–M11 release gate.
