# Native reviewed Plex matching checkpoint

Reference: `453fca983222c6665a48775eef67c489c96b527e`. This is an opt-in implementation checkpoint, not a closed migration or release gate. Python remains the default. No live Plex server, real credential, user catalog, or user output was used in qualification.

## Native interface and ownership

```sh
catabolic-native --db /disposable/catalog.db operation plex-match run --options \
  '{"binding_id":"fixture","mapping_id":"mapping","metadata_source":"association","max_items":10000}'
catabolic-native --db /disposable/catalog.db operation plex-match list --options \
  '{"binding_id":"fixture","limit":100,"after":""}'
```

Apply requires `"apply": true` and the exact preview `"expected_plan"`. The native operation reads all remote evidence again before admitting accepted provenance. This companion JSON interface does not claim frozen `consumer metadata-match` / `metadata-matches` argparse or renderer parity, nor a public `catabolic.plex_matching` facade.

- `catabolic-runtime/src/plex_matching.rs` owns the bounded complete-library scan, unique exact published-path selection, per-item detail identity checks, cancellation polling, and orchestration
- `catabolic-store/src/plex_matching.rs` owns published/source/association snapshots, complete review digests, saved-match publication fences, and accepted-match listing
- The existing managed consumer Adapter owns HTTP deadlines, credential intake/redaction, redirect policy, response/XML limits, and server/library identity
- The already qualified import page parser is reused without modification, including its unqualified-attribute rule, pagination bounds, multipart parsing, and HTTP/1.0 fresh-connection behavior
- Existing descriptor-relative source/output helpers validate a live source and the exact owned published link. Matching never creates, deletes, or rewrites a published link or media file
- Existing curation owns proposal admission and `accept_existing_association_evidence`, including transactions/savepoints, source revalidation, deterministic IDs, decisions, and idempotent replay

Both the CLI and PyO3 `execute_operation(..., "plex-match", "run", ...)` use `api::execute_plex_match_with_poll`. PyO3 preserves the original Python signal exception after joining the remote worker. The CLI returns 130 on interruption.

## Preserved review and bounded lifecycle

The scan accepts association or explicit item metadata selection and `max_items` from 1 through 10,000. It scans the entire library in pages of at most 100. A match requires exactly one remote record containing the translated published path. Memory retains one candidate and an ambiguity flag rather than accumulating a library's metadata. It reuses the import limits of 100 parts per record, 1,000 parts per page, and 256 KiB per parsed record, plus the Adapter's 8 MiB response/XML bounds.

Matching preserves the publication's identity-bearing binding fields, mapping, unique active primary association, source snapshot, source/output bindings, remote GUID, complete remote part set, rating key, and metadata source. Remote detail validation rejects malformed rating keys, wrong library/type, changed GUIDs/parts, and invalid/duplicate field-lock records even when no metadata field is selected. Detail XML attributes must be unqualified, matching ElementTree.

The plan ID is the frozen SHA-256 encoding of database ID, profile, complete connection, local snapshot, and evidence. A 4 MiB review ceiling is an explicit native safety bound. The existing curation evidence/snapshot limits apply to mutation admission. Preview is byte-for-byte read-only. Changed local/remote review requires another preview.

HTTP executes while the catalog is detached and no writer lock is held. Reattachment fences catalog identity; matching additionally compares the full connection and publication snapshot. Cancellation is checked between every remote request, including between final server and library validation, and every local admission stage. Polling runs every 25 ms. The worker is always joined; cancellation may wait for the current managed HTTP request's existing 15-second deadline, but no new remote read or local proposal is admitted afterward.

As in the frozen owner, proposal put and acceptance are separate durable steps. An interrupted or failed acceptance can leave a pending proposal for idempotent continuation; it does not undo previously committed work. Within an explicit native outer transaction, the existing Store savepoints allow both steps to roll back together. Native matching also revalidates the published snapshot between proposal put and acceptance, then checks cancellation again immediately before acceptance. This extra fence is an intentional safety strengthening against publication changes during admission. Independent review found the final validation initially lacked that following cancellation checkpoint; a deterministic SQLite progress hook demonstrated acceptance after cancellation during validation. The checkpoint now rejects that schedule while retaining the separately durable pending proposal for continuation.

`matched_publication` exposes the saved evidence/publication fence to a future native metadata owner. It permits metadata-value edits while rejecting changed binding/mapping/association structure, source revision, or output identity. Metadata field selection, desired-value construction, publication serialization, remote writes/readback, and durable uncertainty events remain the metadata owner's work.

## Executed evidence

All fixtures use disposable local sources/catalogs/output trees and loopback HTTP with fake tokens. Output trees are created only as isolated fixtures, before matching reads them. Native matching sends only HTTP GET requests.

- `tests/test_rust_plex_matching.py`: 26 passing subprocess cases under the exact frozen Python 3.14.6 source. All 14 frozen matching scenarios are retained; the legacy CLI case uses companion run/list instead. Ordinary previews compare the complete JSON and plan digest. Applied return values and catalog/proposal/decision rows are compared against a separate frozen-owner database copy; only `created_at` is normalized
- The frozen matching suite includes metadata publication behavior. In this harness, native matching creates the accepted evidence, while the frozen Python metadata owner consumes it against the fake service. Those assertions establish provenance compatibility, **not native metadata publication**
- Additional subprocess coverage: two-page boundary match; invalid options; changed connection during a detached read with writer progress; catalog replacement refusal; strict detail namespaces/identity/locks; malformed/pagination/part/field/response budgets; disabled binding/inactive mapping/changed link; invalid rating key; actual SIGINT during detail read and final identity read, proving no later request or local admission
- `catabolic-store/tests/plex_matching_review.rs`: independent deterministic regression for cancellation arriving during final publication revalidation, proving no accepted decision is recorded
- `catabolic-store/tests/plex_matching.rs`: six passing production-owner tests for cancellation after proposal put and idempotent continuation; decision-trigger rollback retaining a pending proposal; enclosing transaction rollback; publication change before acceptance; saved-match metadata-versus-structure/source fencing; stale plans/connections and review budget
- `scripts/native_plex_matching_embedding_tests.py`: two passing installed development-wheel tests under Python 3.14.6, run outside the checkout with `-I`. KeyboardInterrupt and an exact custom Python exception object survive bounded HTTP cleanup. Catalog bytes remain unchanged, the writer is available, no further request follows cancellation, and retry produces one accepted proposal without duplicate decisions
- Matching-specific Rust formatting, Python Ruff checks/formatting, and diff checks pass. Runtime all-target Clippy and Store library plus both matching-test targets pass with warnings denied. Whole-Store all-target Clippy and workspace formatting remain blocked by concurrent query/legacy-CLI edits; the complete aggregate must be rerun after those edits stabilize

Original receipts are under the worktree's `.local-tests/plex-matching/`; the corrected-checkpoint rebuild and reruns are under `.local-tests/plex-matching-review-fix/`. Development-wheel qualification does not substitute for a final helper-paired wheel, exact installed release-artifact qualification, independent review, rollback across releases, or macOS/platform testing. Legacy CLI/public Python facade parity and the native metadata publication owner remain open. Existing separately blocked review lanes are unaffected.
