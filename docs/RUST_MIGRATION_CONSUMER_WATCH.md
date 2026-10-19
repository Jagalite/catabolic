# Preserved consumer watch adapter

This is a narrow development checkpoint for the opt-in
`catabolic-legacy-native consumer watch` leaf. Python remains the default. The
owner ledger records an adapted, **unqualified** leaf, not full leaf parity or a
completed migration milestone. M6 publication/recovery gates are unchanged.

## Ownership and frozen behavior

The adapter forwards each tick to the existing production
`catabolic_runtime::api::execute(consumers, run)` and then
`execute(notify, run)` owners. It does not duplicate claim, acknowledgement,
publication verification, notification policy, or restart decisions. Each owner
opens its own store, and the existing consumer/notification network paths detach
that store before remote work. No catalog handle exists in the supervising loop
or survives a tick into the idle interval.

The supervisor invokes a private tick entrypoint on its **own executable** using
the existing bounded subprocess owner. This is not a companion registration, a
PATH lookup, or a Python fallback. Input contains only database/profile/limit;
credential values are neither arguments nor protocol fields. SIGINT/SIGTERM
cancel and reap the tick's process group before the terminal reports exit 130.
An interrupted network operation leaves its durable lease to the existing owner
for ordinary expiry/restart; it does not claim an undelivered acknowledgement or
exactly-once remote delivery.

Frozen semantics retained:

- `--interval` defaults to 30 and accepts 1..60 seconds
- `--limit` defaults to 10; the CLI's general 1..1000 check is followed by the
  consumer owner's narrower 1..100 refusal, before catalog access
- Every completed tick prints and flushes one JSON line, even when idle and even
  without `--json`; `--machine` wraps the line in the frozen envelope
- Watch-line JSON uses Python's ASCII escaping and insertion order for the
  exercised report shapes; terminal interruption/error output uses the existing
  terminal renderer
- The envelope's per-tick exit code is based on the consumer report's `complete`
  field. An incomplete notification report does not silently change that field
- Owner errors terminate the loop. Catalog contention retains the frozen
  `CatalogBusy` exception type; failures do not become fabricated successful
  ticks or automatic retries
- Cancellation is checked before each tick and throughout idle waiting

## Qualification scope

`tests/test_rust_legacy_consumer_watch.py` compares the compiled native executable
with independent Python 3.14.6 processes importing pinned reference commit
`453fca983222c6665a48775eef67c489c96b527e`. Catalogs and output roots are disposable,
independently seeded with frozen fixture code. The only HTTP servers bind numeric
loopback, and all credential values are synthetic.

The selected checks cover exact ordinary/JSON/machine idle lines and
interruption channels; invalid limits/intervals and missing database/profile;
terminal writer contention; populated consumer and notification lines; consumer
repair and notification retry reports; per-tick notification limits; independent
consumer/notification completeness; writer availability during idle/network
work; consumer and notification in-flight SIGINT; expired-lease restart; and
native idle SIGTERM. The follow-on tests separately cover active consumer and
notification SIGTERM, observing the private tick PID before cancellation and its
reaping afterward, socket closure, writer reuse and expired-lease restart. Remote requests and affected delivery, attempt, event,
destination, and binding state are compared separately. Snapshot normalization
is limited to generated lease tokens, event IDs, and wall-clock timestamps;
notification payloads for pre-seeded events now compare as complete HTTP body
bytes as well as decoded JSON. The original receipt exposed a wire-byte gap:
frozen HTTP JSON ASCII-escaped Unicode while the native notification owner sent
UTF-8. The narrow [notification wire follow-up](RUST_MIGRATION_NOTIFICATION_WIRE_JSON.md)
closes that serialization gap at the ordinary HTTP send call only. Watch stdout
remains byte-exact in those scenarios; mapped-request bodies retain their frozen
UTF-8 encoding.

The adapter restores the consumer outcome's known field ordering in the shared
terminal renderer. That also benefits the existing `consumer run` route without
changing owner behavior or defaults.

## Remaining limits

The private tick protocol has a four-MiB combined stdout/stderr budget and an
additional four-hour whole-tick ceiling. These are explicit native limits, not
claims of equivalence for formerly unbounded Python behavior. Existing per-call
network budgets remain unchanged.

Blocked-stdout shutdown is not prompt-qualified. The independent supplemental
paired probe observed both frozen Python and native waiting for output to drain
before completing exit 130. The idle/network cancellation receipts must not be
read as a promise of prompt shutdown while stdout is backpressured.

This checkpoint does not qualify every possible report shape, every signal
window, process-parent death on every platform, macOS or installed artifacts,
optional Apprise descendants, real Plex/Jellyfin indexing, or all restart/lease
schedules. Existing consumer/notification owner and M6 gates remain necessary.
No broad definitions, filesystem, hardlink, source-access, observation, remount,
or watcher safety review was performed or retried. Fresh independent review and
artifact-bound aggregate evidence remain required before promotion.

## Local development receipt (2026-10-12)

On Linux, Python 3.14.6 and Rust 1.99.0, the final source/artifact-fenced pass
completed all **nine new watch test methods** and **15 existing legacy CLI test
methods**, with no skips or failures. The CLI's **six Rust unit tests**, scoped
formatting/Ruff checks, owner-ledger regeneration check, and dependency-inclusive
CLI Clippy with warnings denied also passed. The two pre-existing store Clippy
warnings were resolved by the parent's separately owned mechanical patch
`54fdbde` before this final pass; this adapter does not own that patch.

The tested `catabolic-legacy-native` development binary SHA-256 is
`78fd17c733e251cba1682d21414297f76779ceaded839e2151d2942ceea12ec8`.
The full frozen source inventory, native/source hashes, before/after fences,
scenario exit statuses and logs are retained under
`.local-tests/consumer-watch/` in the isolated development checkout. They prove
only this named local scope, not an installed, release, whole-workspace, or
independently reviewed qualification. Synthetic consumer delivery in this suite
uses Jellyfin; synthetic Plex watch and real-service acceptance remain outside
this receipt.

## SIGTERM coverage follow-up

Independent product review found no confirmed implementation defect, but the
original SIGTERM test ran after a completed tick line and therefore demonstrated
idle cancellation only. It is now named accordingly. Two additional deterministic
Linux tests wait for the disposable consumer/notification server's request
readiness, identify the active same-executable tick with `ps --ppid`, and verify
its PID is gone after SIGTERM, the socket closes, an unexpired lease is preserved
without repeat requests on restart, the catalog accepts a new committed write,
and the owner completes after synthetic lease expiry/restart.
This is a test/docs-only follow-up; product code and its previously tested binary
are unchanged. It does not strengthen any unrelated platform or release claim.

The focused rerun passed all three named idle/active SIGTERM test methods without
skips or failures. Ruff, formatting and diff checks passed. The previously
recorded product binary hash is unchanged; follow-on logs are retained as
`.local-tests/consumer-watch/sigterm-followup.log`.
