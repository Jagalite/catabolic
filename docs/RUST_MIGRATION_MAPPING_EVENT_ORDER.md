# Preserved complete mapping-event order

This is narrow M9/M10 CLI presentation work from `343d7e8`. Python remains the
default and `projection mapping-events` remains partial. No milestone, installed
artifact, platform, release or underlying mutation-owner gate is closed.

## Ownership and presentation

The production `mapping_history` aggregate still reads every profile-matching
publication subject in rowid order before its bounded joined HTTP history. The
same aggregate selection function serves existing `events` and the new
`events_with_subject` companion. The latter retains only the selected publication
subject's original text from its existing row. It opens no second Store and adds
no SQL query. A read snapshot test updates the catalog from a concurrent writer,
then verifies the old value and old subject order together; an authorizer counts
exactly the original two SELECTs.

The CLI scans container order only after the complete aggregate validates and
selects successfully. It prepends the durable ID and emits every scalar from the
validated native Value using the existing encoders. Duplicate JSON keys retain
the first insertion position and the last value/container order, matching the
frozen json.loads behavior. Escaped-equivalent keys and literal serde private
number objects remain ordinary keys. Opaque objects never enter report-shape
ordering heuristics. In particular, a publication subject with the eight fields
of an HTTP row retains its stored order; only real joined HTTP rows use their
fixed SQL-row presentation.

Order recovery, shape checking and rendering run inside the existing supervised
interrupt scope. The adapter uses a command-local one-slot channel for a completed
output, with nonblocking send/receive and no global mutable cache. Owner failure
or cancellation wins over any staged output. Missing/inconsistent order fails;
raw subject values are never substituted. Standard output is published only by
the existing main/transport completion boundary. Envelope fields and exit status
still come from the ordinary presenter.

Summary selection, empty `--event`, profile filtering, publication/HTTP ordering,
last-100 and last-1,000 limits, first aggregate match and whole-aggregate error
precedence are unchanged. Recovery and effects are unchanged. There are no public
JSON marker fields, schema changes, canonical serializer changes, new dependency
features, plan/digest changes or Python fallback. M6 remains fail-closed.

## Evidence and remaining gaps

`tests/test_rust_legacy_mapping_event_order.py` compares stdout bytes, stderr bytes
and status directly against the pinned frozen Python reference, without
normalization. Its separate disposable catalog copies exercise eight recursive
key permutations, opaque report-shaped data, Unicode/escaped keys, nested arrays,
empty containers, huge integers, finite float token normalization, negative zero,
duplicate/escaped-equivalent keys, HTTP-shaped publication/HTTP collisions and
incomplete status. Real SIGINT during a selected-event read checks discarded
output, unchanged catalog snapshots and immediate exclusive-transaction reuse.

The existing history suite retains local authority, filter/cap, full-event,
unknown/missing admission, metadata-lock precedence, queue states/refusals and
recovery interruption coverage. Its former noncanonical order gap witness now
requires exact bytes in text/JSON/machine output. Native tests cover same-snapshot
values/order and query counts, error precedence after a selected row, unchanged
empty/listing behavior, duplicate handling, inconsistent shape refusal,
channel-drop/no-message/error nonblocking behavior and interrupted order work.

Malformed-subject diagnostics, duplicate top-level subject IDs, missing summary
fields and broader catalog/path admission still retain their existing differences
from frozen exception classes/messages. Extreme-depth text output also retains
an unqualified frozen Python renderer recursion-limit edge (observed at depth
510); exact nested-order review evidence extends through depth 400. Full-leaf,
installed-artifact and platform qualification remain open. Exact completed local outcomes and artifact identities
belong in the separate evidence receipt; earlier receipts do not qualify this
source. A known unrelated ledger test at the base still demands the superseded
literal `SIGTERM differences`; it is not corrected by this presentation work.

The two previously denied broad reviews are neither retried nor resolved here.
Fresh review is limited to this command-local presentation, Store companion and
its fixtures.

## Completed local checkpoint

The [source/artifact receipt](RUST_MIGRATION_MAPPING_EVENT_ORDER_CHECKPOINT.json)
records the passing corrected source `7f3c3f2`: 17 frozen/native Python 3.14.6
methods, seven Python 3.11.15 methods, ten Store owner units and all 18 legacy CLI
units. Scoped CLI/Store Clippy, Rustfmt/Ruff, exact owner-ledger regeneration and
the two relevant ledger methods passed. Fresh narrow review found no remaining
actionable issue after fixing an omitted-database dispatch regression; the failed
initial artifact and its failing receipt remain preserved.

Python 3.14 discovery/provenance admitted 1,132 unique native scenario IDs and
244 imported modules, with zero product scenarios executed. An unchanged,
source-fenced but unregistered helper was refused. Optional Python 3.11 aggregate
discovery stopped before execution because that reference interpreter lacks
httpx/fastapi; its selected seven-method run above passed. The unrelated base
SIGTERM ledger assertion remains a separately recorded failure. None of these
selected checks closes the full qualification gates.
