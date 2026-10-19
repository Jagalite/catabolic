# Native v1 destination mapping owner

This is a local M9 implementation checkpoint, not a migration or release gate.
Python remains the default. The preserved native CLI's mapping leaves remain
**partial**. Filesystem-v2 publication and every M6 effect gate stay unchanged.

## Complete ownership boundary

`catabolic_runtime::destination_mappings` owns v1 preview, reviewed apply and
readback-only recovery. It takes a database path rather than a foreign connection.
Its Store policy calls the existing saved-query Evaluation and expression owner;
its adapter calls the existing bounded, credential-aware consumer transport. The
new JSON POST and DELETE paths reuse that transport's redirect refusal, response
bounds and fresh-connection/no-replay behavior. There is no Python fallback.

- Frozen v1 definition keys, expressions, rows-only complete-query checks,
  duplicate-column/target checks, grouped scalars and bounded set values are used
- Plans pin the complete definition, database ID, evaluated query rows, stored
  connection including revision/evidence, remote library, scope, identities,
  snapshots and ownership ledger using the existing canonical JSON digest
- Production reducers implement scalar replace with conflict detection and
  owned-set merge. Manual values remain manual; omitted rows are retained;
  removal budgets count only removed owned values
- Plex scalar/genre/label metadata and Jellyfin metadata use the actual remote
  wire formats, identity/path/kind checks and locked-field/whole-DTO readback.
  Explicitly adopted collection IDs use per-membership identity/membership
  rechecks, without name-based collection creation
- Apply keeps the existing interoperable metadata writer lock. Every Store and
  SQLite snapshot ends before network work. Each item re-evaluates local inputs,
  connection and remote state before its durable `started` event and effect
- Successful readback records `verified`. Ordinary write/verification failure
  records `uncertain` and stops the batch, preserving previous item outcomes.
  Cancellation after durable start leaves `started`, like frozen
  KeyboardInterrupt; effects are never detached or retried
- Recovery resolves exactly one event from the existing combined history. It
  observes current remote state and can persist `verified`, `not_applied` or
  `uncertain`; it never writes, undoes or replays a destination effect. Terminal
  events return without contacting the destination. HTTP-v3 keeps its queue-only
  recovery rules; v2 is handed back as unavailable

## Explicit fail-closed details and limits

Only v1 remote metadata comparisons use frozen Python numeric equality, such as
an untouched Jellyfin DTO property changing from integer 7 to float 7.0. The
shared helper is confined to this new destination module and its adapters.
Canonical plan bytes/digests, authorization/grant IDs, query ownership, caches
and every other subsystem keep their existing comparisons and fail-closed rules.

The new owner refuses event writes if the catalog database identity has changed,
and refuses a completion update if its durable event disappeared. These are
explicit conservative checks beyond frozen's unchecked event UPDATE; they do not
claim recovery or success for an unprovable outcome. The extra checks are covered
by narrow Store tests. Existing frozen mixed-history behavior is not silently
relaxed: an ownership event lacking a scope (including a historical HTTP row in
that aggregate) fails before destination effects. Exact exception parity for
malformed/mixed stored events remains unqualified.

Known standard report fields retain their order. Arbitrary definition field
insertion order and Jellyfin editable DTO insertion order are not fully preserved
by the sorted native JSON value representation. Plan hashes and parsed reports
are compared independently; this is not an all-input byte-parity claim. Existing
consumer transport EOF/reset distinctions and malformed-input diagnostics remain
named qualification gaps. No live consumer, production credential, installed
release artifact, macOS/platform matrix or full M9 closure is claimed.

## Evidence

`tests/test_rust_legacy_destination_mappings.py` runs separate frozen Python and
native CLI processes against cloned-but-independent disposable catalogs and
separate fake Plex/Jellyfin receiver state on a numeric-loopback HTTP server.
The shared loopback URL makes complete reviewed plan hashes comparable. Tests
inspect durable event subjects, consumer mutation requests and receiver state,
not only returned reports. They cover stale plans, owned merge/removals, no-op
writes, query grouping/truncation/empty results, remote identity conflicts,
metadata lock contention, network-time catalog/SQLite lock release, uncertain
outcomes, observation-only recovery and actual SIGINT after write admission.

Native unit tests exercise the same production policy reducers and wire adapters,
including collection partial outcomes, cancellation between requests/effects,
encoding, bounds, redirect refusal and whole-DTO validation. Exact source,
artifact/oracle hashes, commands and completed outcomes are recorded in the local
checkpoint receipt after the final build. A fresh independent review covers only
this new owner and its narrow integration, not previously blocked broad reviews.

### Completed local checkpoint, 2026-10-12

[The compact source/artifact-bound receipt](RUST_MIGRATION_DESTINATION_MAPPINGS_CHECKPOINT.json)
records 36 paired methods passing without skips under both Python 3.14.6 and
Python 3.11.15. Both runs used the same frozen source inventory, unchanged native
source content and unchanged debug artifact. The fixtures explicitly pin and
verify the frozen `catabolic.http_worker` module in child HTTP worker processes.

The final artifact also passed 29 existing v3/history/owner-ledger regressions,
11 native wire-adapter tests, 6 policy/durability tests, 3 consumer-transport
controls, 6 presentation controls and the stdin guard. Rust formatting, selected
Python lint, Store/runtime and CLI Clippy with warnings denied, and whitespace
checks passed. Fresh independent review accepted the final artifact after the
numeric DTO comparison correction and independently replayed the large integer
budget and full-reference Python 3.11 cases. All receivers were disposable
numeric-loopback fixtures; no live consumer was contacted.

Full local run receipts and logs are retained in
`.local-tests/destination-mappings/qualified-{314,311}.json`, along with a copy of
the tested debug binary. The compact checked-in receipt hashes those records and
lists every unique paired method, changed source file, explicit conservative
exception and remaining gap. Earlier environment-preflight failures and
superseded source-binding attempts remain retained as diagnostic evidence, not
passing qualification evidence.

[The final independent acceptance report](RUST_MIGRATION_DESTINATION_MAPPINGS_REVIEW.md)
is preserved separately, including its post-removal-budget coverage and explicit
reviewer-reported evidence provenance.
