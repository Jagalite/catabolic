# Native fallback consumer integration checkpoint

Date: October 11, 2026. This is a disposable-fixture implementation receipt,
not a filesystem qualification, supported-server acceptance, or release gate.
Python remains the default. Native fallback managed-output mutations still
require a debug build and explicit `disposable_fixture=true`; release builds
remain closed to those writes.

## Ownership and delivery

The existing native consumer owner now takes a mutable Store and the runtime's
bounded fallback probe callback for initial binding verification, claim-time
verification, pre-send verification, and indexing admission. It uses the same
filesystem publication verifier and journal, not a second publisher or cached
claim-time authorization. The runtime continues to own only bounded external
Plex/Jellyfin requests; all durable consumer SQL remains in the Store.

A detached probe allows independent writers to progress. Consumer admission
therefore captures binding revisions, generations, scheduling state and
connection revisions, then checks the current delivery and bindings after
verification. The complete multi-catalog validation is fenced by the fallback
epoch. Lease acquisition also conditionally checks current delivery eligibility
inside its transaction. A worker that changed the group while verification was
detached retains its lease, attempts and error state.

Immediately before a remote refresh, the consumer performs fresh complete
publication verification and checks the lease, connection, bindings and exact
generations again, including when verification fails. Existing acknowledgment
semantics remain: an accepted scan can acknowledge only its captured generation;
a newer generation stays pending. A scan request is not proof of completed
indexing or playback readiness.

Indexing admission selects effective logical fallback entries when a fallback
binding exists, rather than the ordinary exact mappings table. It preserves
profile/subtree scope, applies the `limit + 1` bound in SQL, validates translated
paths, and retains the existing post-network generation/connection fence.
Ordinary exact catalogs continue through the existing verifier and adapters.

## Focused qualification

`crates/catabolic-runtime/tests/fallback_consumers.rs` exercises production
publication, Store consumer policy, and real source checking with deterministic
writers at detached probe boundaries:

- A competing lease introduced during either claim-time verification pass is
  neither stolen nor rewritten, and no consumer attempt is created
- Generation changes, connection revisions, disabled bindings, replaced leases
  and expired leases during pre-send probes prevent send admission and stale
  acknowledgment
- Pre-send checking performs new live work; failed evidence remains retryable
  without acknowledgment
- Indexing uses effective fallback paths and rejects generations changed either
  during local verification or before the remote observation is recorded

`tests/test_rust_fallback_consumers.py` adds seven disposable loopback cases:
actual logical-publication scan and indexing, indexing overflow before remote
discovery, one automatic drain and idempotent initial scan, loss of source after remote discovery, competing lease preservation,
a newer generation arriving during a scan, and unresolved retained membership.
The existing 21 `NativeConsumerTest` cases cover ordinary catalog behavior,
Plex/Jellyfin request shape, error handling, connection identity, process death,
lease expiry, indexing, and detached writer availability.

Results: the four native Rust tests passed, including both claim-verification
contention points. The combined six-case fallback and 21-case ordinary consumer
run passed all 27 tests; the additional fallback indexing-overflow test also
passed. Strict Clippy was attempted but encountered concurrently edited
`access.rs` and `embedding.rs` warnings outside the consumer scope; aggregate
strict lint qualification remains a separate checkpoint.

The subprocess suite uses one immutable native CLI copied after building, with
SHA-256 `7171b1094a95f2ff32b4875340e3bc8af38ded4b7e9123cec0681db79af8bcc4`.
These are Linux-local disposable fixtures, not real Plex/Jellyfin deployments,
macOS qualification, or packaged release-artifact acceptance.

## Reproduction

Build the native CLI once, copy it to a stable path, and set
`CATABOLIC_NATIVE_BINARY` to that copy before running subprocess fixtures. Do not
replace that file while tests are running.

```sh
cargo test -p catabolic-runtime --test fallback_consumers --offline
cargo clippy -p catabolic-runtime --test fallback_consumers --offline -- -D warnings
python -m unittest -v \
  tests.test_rust_fallback_consumers.NativeFallbackConsumerTest \
  tests.test_rust_consumers.NativeConsumerTest
```

No schema change, new network destination, expanded release permission, or
fallback filesystem/definition safety-review completion is asserted here.
