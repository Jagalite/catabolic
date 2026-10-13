# Preserved mapping history and HTTP queue recovery

This narrow follow-on adds a Rust local-owner history query and preserved CLI
adapters for `projection mapping-events` and the read-only HTTP-v3 branch of
`projection mapping-recover`. Both leaves are **partial**, not behaviorally
qualified. Python remains the default; no milestone, release, installed-artifact,
platform, live-service or underlying destination-owner qualification is claimed.

## Implemented contract

- `catabolic_store::mapping_history` reads the local Store directly. It does not
  repurpose the Access-scoped, operation-filtered HTTP deliveries endpoint or add
  an HTTP route. No token, operation grant or enabled-operation restriction is
  added to the frozen local catalog-owner authority
- All matching profile `mapping_publish` subjects are decoded in event rowid
  order, then the newest 1,000 joined HTTP delivery rows are appended in reverse
  order. That is an aggregate of two histories, not global chronological order
- An explicit nonempty event ID selects the first matching complete event from
  that aggregate. Unselected lists take its last 100 rows, return only the five
  frozen summary fields and report truncation against the aggregate length.
  Empty `--event` is the listing path. Unknown profiles have empty histories
- Recovery requires exactly one match in the same aggregate before inspecting
  version. Numeric version three returns the queue state and reports `complete`
  and `delivery_acknowledged` only for state `complete`. Other states retain CLI
  exit 3. Resolution and expected-plan options are ignored for v3, as frozen
- HTTP-v3 `--apply` refuses with the original
  `mapping_http_recovery_uses_notification_queue` ConsumerError. Unknown events
  retain the different mapping-events CatabolicError and mapping-recover
  ConsumerError provenance
- `--apply` obtains the existing interoperable metadata writer lock before Store
  admission and event lookup, even for a refused request. The Store is still
  read-only; neither a catalog writer lock nor a delivery worker is invoked
- The ordinary SQL interrupt bridge covers history queries and decoded rows.
  Consuming the Store confines its SQL hook and read transaction to this call;
  the metadata writer guard releases on error, return or interruption

## Explicitly unsupported and unqualified

Versions one/two and other non-v3 versions return an explicit unavailable native
owner error. Actual metadata/publication recovery dispatch is still an
implementation dependency, and its destination mutation paths also have their
existing qualification gates. Neither absence is treated as proof that ordinary
read-only adapter work is unsafe. No filesystem mutation owner or gate changed.

The later [complete-event presentation adapter](RUST_MIGRATION_MAPPING_EVENT_ORDER.md)
retains arbitrary nested stored-subject insertion order using provenance from
this same Store snapshot. Publication subjects that mimic the HTTP row shape are
now distinguished from real joined HTTP rows. HTTP rows and summaries retain
their frozen field order. Malformed subjects, duplicate subject IDs, missing
summary fields and unusual catalog/path admission diagnostics are still not
fully equivalent to frozen exception classes and messages. These remaining gaps
are not silently counted as full-leaf parity.

The separate HTTP-v3 definition/stdin adapter's qualification gaps still apply to
that adapter. This change neither patches nor qualifies its stdin transport.

## Selected evidence

`tests/test_rust_legacy_mapping_history.py` uses independent native and frozen
Python 3.14.6 processes against separate disposable catalogs. It verifies local
owner authority, profile filtering, interleaved insertion order, last-100 and
last-1,000 cutoffs, complete events, duplicate aggregate lookup rules, queue
states and ignored options, error provenance/status, lock precedence/release,
missing-database admission, unchanged catalog snapshots and real SIGINT cleanup.
Many legitimate fixture events keep a read in flight for real-signal cleanup; a
separate owner test interrupts specifically through SQLite progress callbacks.
No production delay hook, delivery server, credential or real output root is used.
The later ordered-output tests require exact noncanonical stored JSON bytes.

The native `mapping_history` checks cover numeric version dispatch, missing
recovery-owner handback, refusal without catalog effects, durable-ID protection,
local profile/full-subject reads and cancellation followed by Store reuse.
`test_native_cli_owner_ledger` verifies the two exact entrypoints remain partial.

Exact local source/artifact hashes, commands and completed outcomes are recorded
in the follow-on evidence receipt. Historical HTTP-v3 or installed-wheel receipts
do not qualify this source.

## Later recovery owner

The [ordinary v1 owner](RUST_MIGRATION_DESTINATION_MAPPINGS.md) now consumes the
same production aggregate for durable readback-only recovery. HTTP-v3 semantics
remain queue-only and v2 remains unavailable in this checkpoint. Historical
receipts above do not qualify the new owner.
