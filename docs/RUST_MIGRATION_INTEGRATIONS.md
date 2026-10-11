<!-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors -->
<!-- SPDX-License-Identifier: MIT -->

# Native outbound integrations checkpoint

This is a bounded M9 implementation checkpoint, not a claim that all HTTP,
consumer, playback, or platform acceptance work is complete. The frozen Python
reference is `453fca983222c6665a48775eef67c489c96b527e`.

## Ownership and entry points

`catabolic-store` owns notification destinations/events/deliveries, native bearer
and stored-token authorization, managed webhooks, OpenAPI operation definitions,
mapping plans/ledgers, consumer connections/bindings, refresh leases/attempts,
indexing evidence, and library-creation intents. `catabolic-runtime` owns HTTP and
the optional provider subprocess. No network call runs with a Store connection,
writer lock, or SQLite transaction open. Reopening fences the catalog identity;
delivery acknowledgement additionally fences the claim and destination/binding
revision. Consumer administration explicitly ends its own read-only snapshot
before remote reads and opens a new snapshot before checking revisions.

The companion operation API exposes:

- `notify`: configure/put, status, disable, retry, emit, run/drain
- `webhooks`: register, inspect, list, deliveries, disable, retry (bearer input
  through a credential environment reference)
- `http-operations`: compile, put, get, disable, retry
- `http-mappings`: run/preview/apply with expected-plan fencing
- `consumers`: connection-put, discover, bind, connections, bindings/status,
  attempts, events, creations, publications, disable, enable, retry, run/drain,
  verify-indexing, creation, create

These are native operation boundaries, not a complete public HTTP server. Local
OpenAPI/mapping administration has not been presented as an authenticated HTTP
entry point. Existing HTTP-managed deliveries reauthenticate their exact stored
creator and enqueue tokens, current grants, pinned revisions and approved origin.

## Delivery guarantees

Notification policy edits cancel obsolete work; idempotent reconfiguration
preserves live claims. Claims are expiring and acknowledgements are single-use.
Failures retain successful destinations, apply bounded retry/backoff, and expose
only constant sanitized outcomes. Explicit retry does not steal live notification
leases. Arbitrary producer event names remain compatible with Python; subscription
configuration retains its fixed allowlist.

Consumer refresh groups coalesce bindings by stable server/library identity.
Every enabled group member must have a healthy complete local publication. Busy
libraries defer without spending an attempt. Discovery is followed by a fresh
binding/generation/local-publication check before scan dispatch. A generation
published during a scan remains pending. Disable/rebind/token expiry fence stale
acknowledgements. Scan acceptance does not imply successful indexing or playback.
Native publication command-close handling invokes one bounded automatic consumer
drain. Indexing is bounded first-page, exact server-reported path evidence only.

Library creation commits an `uncertain` intent before its single POST. A timeout,
error, crash, or missing response never authorizes replay. Only one newly observed
matching library can reconcile the intent. Completed creation reuse rechecks
library identity. Creation and adoption do not delete remote libraries or force
metadata/trash operations.

## Network and optional adapter boundary

HTTP uses pinned `ureq` with rustls, global/connect deadlines, no ambient proxy,
and redirects disabled. Responses and credentials have explicit size bounds.
Plex XML uses pinned `roxmltree`, with DTD/entity declarations refused and a node
budget. Jellyfin JSON and mapped HTTP responses use the native strict JSON decoder.
Credential files are opened without following the final symlink, must be regular,
current-user-owned, private and single-link files, and have a read limit.

Only non-HTTP optional Apprise destinations retain a Python adapter. The embedded
MIT-licensed `apprise_worker.py` is derived from the frozen project's
`src/catabolic/notification_worker.py`; its header records provenance. It runs via
an isolated, bounded subprocess with a static protocol allowlist and summaries.
`CATABOLIC_APPRISE_PYTHON` selects the interpreter (default `python3`). Missing
interpreter/provider is an explicit repair outcome. It does not open a catalog,
evaluate policy or provide a Python fallback for HTTP. This checkpoint therefore
does not claim a Python-free optional Apprise installation.

OpenAPI compilation resolves only supported local references. JSON Schema uses
Draft 2020-12, no format checker, and a deny-all external retriever; HTTP/file
schema retrieval is disabled. Mapping plans require complete query connections,
have bounded rows/requests/body/plan sizes, and queue atomically against the
expected plan and prior ledger. Active delivery leases block supersession.

## Qualification

`tests/test_rust_notifications.py`, `tests/test_rust_http_mappings.py`, and
`tests/test_rust_consumers.py` use isolated catalogs, actual native executables,
fake credentials and disposable loopback HTTP servers. They cover exact Python
policy/plan/discovery parity, requests, lock release, partial delivery, revocation,
redirect/proxy refusal, malformed responses, atomic rollback, crash/reclaim,
creation uncertainty, automatic consumer publication, and stale acknowledgement.
Focused Rust tests cover transaction/lease invariants, producer-event admission,
origin parsing, external schema refusal, XML budgets, and retry/path boundaries.
No live external server or real credential was used for qualification.

## Remaining work outside this checkpoint

- Full public HTTP routing/transport, authenticated operation/mapping admission,
  and watcher HTTP reactions require their own qualification
- HTTP-context GraphQL grant membership currently fails closed; SQL membership
  is native
- Plex account/PIN login, library import and metadata matching/edit workflows are
  not implemented by these consumer modules
- Consumer watch-loop/scheduler orchestration and automatic human-notification
  drain at every publication boundary remain separate from explicit workers
- Consumer parsing accepts UTF-8 XML; other XML byte encodings are not qualified
- TLS against real services, DNS/platform failure behavior, macOS acceptance and
  end-to-end real Plex/Jellyfin/Apprise installations have not been qualified

The full migration's checkpoint and supported-feature matrix remain authoritative.
