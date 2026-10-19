# Preserved legacy Jellyfin refresh adapters

<!-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors -->
<!-- SPDX-License-Identifier: MIT -->

This is an opt-in, **adapted but unqualified** implementation checkpoint for
`refresh configure`, `refresh list`, `refresh run`, and `refresh retry`.
It does not qualify catalog refresh, TMDB identification, the existing publication
owner, the public Python `Refresh` class, live Jellyfin indexing, or any release gate.
Python remains the default; no failure falls back to Python.

## Ownership and retained behavior

- The CLI's frozen parser/presentation delegate to
  `catabolic_store::legacy_refresh` and `catabolic_runtime::legacy_refresh`
- Configuration is profile/catalog-specific, strips trailing slashes, stores only
  an environment-variable reference, and does not retarget already queued events
- The existing filesystem publication owner still creates `refresh_dirty` and
  `refresh_events`. These adapters neither publish links nor synthesize events
- List selects ascending IDs, fetches `limit + 1`, decodes the target, and preserves
  the frozen event column order and `next_after` contract
- Run selects non-complete events with attempts below three and an absent/expired
  lease. Before every claim it invokes the existing current publication verifier
- Unhealthy publication produces one `blocked/publication_unhealthy` result,
  stops this drain, and does not consume an attempt or persist a blocked state
- A claim increments attempts and stores a random token plus a 60-second lease
  before releasing the SQLite connection and advisory writer through `Store::detached`
- The private native helper uses the existing consumer credential/HTTP adapter,
  never Python. Its stdin is limited to 256 KiB and contains a target reference;
  neither arguments nor protocol output contain credentials. It only issues the
  Jellyfin POST `/Library/Refresh`, with no caller-supplied methods or headers
- The existing process supervisor bounds the full request to 15 seconds, including
  DNS/TLS, and owns cancellation, group termination, and reaping. Response bytes
  are limited to 1 MiB; redirects and ambient proxies are refused
- Completion is fenced by profile, event ID, and lease token. Remote/error details
  are replaced by the frozen generic diagnostic. A failed request stops the drain
- `unresolved` counts all non-complete events, including leased and exhausted rows.
  `complete` means none remain; `indexing_verified` is always false
- Frozen quirks are retained: retry can clear a live lease, preserves `finished_at`,
  resets attempts to zero, and refuses completed events. A displaced successful
  acknowledgment still appears successful in the returned event report, while
  the row remains queued and `unresolved` truthfully makes the overall result incomplete
- Cancellation leaves an admitted lease durable rather than inventing a definitive
  delivery failure. Resuming requires lease expiry or explicit retry; a request
  could already have reached the server, so exactly-once delivery is not claimed

## Explicit differences and unqualified boundaries

1. Configuration uses the existing native destination validator. The frozen
   Python `urlsplit(...).hostname` check accepts some unusable ports and certain
   whitespace/control/backslash forms; native rejects them before persistence.
   The suite explicitly records the out-of-range-port difference. Validation
   precedence on combinations of malformed and overlong URLs is not fully qualified
2. SQLite integer overflow in `retry`/`list --after` escapes the frozen CLI as an
   `OverflowError` traceback and exit 1. Native reports the same explanation in its
   normal error envelope with exit 2; it does not recreate a Python traceback
3. Native list/claim reads retain the established 4 MiB result budget. Oversized
   historical rows are refused rather than silently truncated. The frozen owner
   has no corresponding whole-result bound
4. The shared HTTP adapter adds its existing `Accept: application/json` and
   `Connection: close` headers. The preserved refresh path encodes credential
   header values as Latin-1, matching frozen urllib, and rejects code points above
   255 before network delivery. Other consumer/Plex transports retain their
   existing native UTF-8 behavior. Remaining control-character admission differences
   are not qualified; native HTTP header validation is not weakened.
   It refuses an over-limit declared Content-Length before consuming the body.
   A 200 response declaring 100 bytes but closing cleanly after one byte is
   complete in frozen urllib and failed natively. The pinned ureq body reader
   collapses clean premature EOF and connection reset into the same error; accepting
   that error would also admit resets, which frozen rejects. Native retains its
   fail-closed behavior pending a separately scoped transport design. Native URL
   parsing, TLS, DNS, uncommon response statuses and other transport diagnostics
   are not a full urllib compatibility claim
5. Native detached reopening checks catalog identity before acknowledgment. This
   is inherited native Store behavior, not a claim that frozen reopening had the
   same identity fence. No publication-owner qualification is asserted here
6. The private helper refuses malformed/non-object/non-Jellyfin targets with a
   redacted delivery failure. Valid frozen producer records are covered; malformed
   historical-record exception details are not promised equivalent

## Verification

`tests/test_rust_legacy_refresh.py` uses independent disposable databases, the
frozen `453fca983222c6665a48775eef67c489c96b527e` source tree, and the pinned Python
3.14.6 executable. Only numeric loopback and fake credentials are used. Exact
stdout/stderr/exit pairs cover text, JSON and machine formats, configuration,
pagination, parser errors, retries, health, lease eligibility, attempt exhaustion,
HTTP failures, redirects and response bounds. Durable post-state comparisons
normalize only actual SQLite completion timestamps; no output bytes are normalized
for delivered runs. Configure/list checks only normalize distinct database paths.

Native-only checks establish the 15-second helper deadline, interruption/connection
closure, bounded/malformed helper input, and explicit compatibility differences.
The Store unit cases cover lease boundaries, displaced acknowledgment, profile
fencing, readonly refusal, and result-budget refusal. The runtime cancellation unit
case checks `waitpid(..., WNOHANG)` returns `ECHILD` after cancellation, proving its
own direct helper was reaped without depending on executor `/proc` PID mapping.

Build/test commands:

```sh
cargo test --locked -p catabolic-store legacy_refresh --lib
cargo test --locked -p catabolic-runtime legacy_refresh --lib
cargo build --locked -p catabolic-cli --bin catabolic-legacy-native
CATABOLIC_LEGACY_BINARY=/absolute/copied/catabolic-legacy-native \
  /absolute/pinned-python-3.14.6/bin/python -m unittest tests.test_rust_legacy_refresh -v
```

Use a copied binary, record its checksum and relevant source checksums before and
after the run, and preserve failure logs/fixtures with `CATABOLIC_REFRESH_EVIDENCE`.
Focused green evidence does not close broader platform, installation, concurrency,
release, or previously blocked owner-review gates.

Recorded local checkpoint, October 11, 2026: all 14 integration cases passed
against the copied legacy executable with SHA-256
`89055cf9c1f7a99f4237aeb148bacc7481ca39ffbcb3b84621ad7ade0a7be674`.
The scoped source/artifact before-and-after fence was unchanged. Five focused
Rust tests passed (three Store, one runtime reaping, one presentation), as did
Store/Runtime/CLI all-target Clippy with warnings denied and the owner-ledger
completeness guard. Initial failures and their disposable fixtures were retained;
they were not counted as green evidence. These local results are selected adapter
evidence, not installed-wheel or release qualification.
