<!-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors -->
<!-- SPDX-License-Identifier: MIT -->

# Experimental native HTTP 1.7 transport

This is an implementation and qualification checkpoint, not production approval
or default-backend enablement. Python remains the default. The schema stays at
32 and published contract artifacts stay byte-for-byte unchanged. The frozen
reference is `453fca983222c6665a48775eef67c489c96b527e`.

## Explicit entry points

The opt-in CLI is `catabolic-native --db DATABASE api serve
--experimental-native-http --host 127.0.0.1 --port 8421`. It is read-only unless
`--enable-processing` is supplied. Native listeners currently refuse non-loopback
addresses, wildcard hosts/origins, and zero limits. The standalone
`catabolic-http-experimental` binary exists only for disposable qualification.

A real in-process Python ASGI adapter now exists only behind a freshly minted
disposable-catalog capability. Ordinary path-based `create_app` still refuses;
remote/missing peers and forwarding headers refuse before callbacks or route
effects. It does not enable production HTTP or complete the preserved constructor
row. See [the partial ASGI checkpoint](RUST_MIGRATION_ASGI_FIXTURES.md).

`catabolic-native --db DATABASE api worker` selects the combined native worker;
`--once` performs one step. Its single `.api-worker` lock protects playback
recovery and work selection. Heartbeat/pruning precede playback, then rendition
work. One bounded notification task runs independently so slow receivers do not
stall playback supervision. Idle waits hold no catalog session. SIGINT/SIGTERM
cancel supervised rendering, join bounded delivery, and remove only this PID's
worker status. The local `api-worker` operation adapter rejects read-only or
nested-transaction owners before acquiring a supervisor lock.

## Ownership

`catabolic-http` owns routing, bounded bodies, strict request-schema validation,
Problem Details, explicit CORS/Host handling, per-principal rate/stream limits,
conditional/range responses and actual streaming. The route inventory comes from
the immutable 1.7.0 OpenAPI artifact: 55 paths and 62 operations. Historical 1.2
client-generation scripts remain intact; a separate `tests/clients/v1_7` lane
compiles from the 1.7.0 OpenAPI and GraphQL artifacts.

The transport never exposes the trusted local operation dispatcher. Ingress
constructs a request-owned authenticated `Access`. Native store owners implement
catalog metadata, ticket budgets, durable snapshots/events, approved SQL/saved
queries, scoped GraphQL, concrete/logical demands, playback sessions, webhooks,
approved outbound operations, mappings and watcher admission. Missing dependent
owners remain explicit failures; there is no Python business-logic fallback.

Authorized catalog TEMP views scope membership before GraphQL search, filtering,
counts, nested edges and pagination. Hidden metadata, paths, source credentials,
private SQL tables and other principals' objects do not become available through
aliases. Authority is bound to one database/profile/token/grant/snapshot marker.
A new Store or changed catalog epoch cannot reuse it. Approved SQL reports retain
their frozen global SQL semantics and restricted SQL authorizer. Administrator-
approved saved ID selections and dynamic grant membership use a private admission
proof bound to the immutable query/revision, token/grants, database/profile and
catalog epoch. Their SQL and GraphQL leaves evaluate complete IDs globally under
the strict selection grammar and independent composition/ID/byte/time budgets,
as in the frozen owner. Root definitions may explicitly raise the default 10,000
IDs/5 seconds up to the frozen 100,000 IDs/60 seconds; they do not accidentally
shrink a separate policy session's cumulative budget. HTTP saved-selection responses intersect those IDs with
current scoped catalog views before pagination; no intermediate global fields,
rows or counts are returned. Arbitrary ingress documents and saved documents
remain scoped. Admission caches cannot become scoped GraphQL caches, and failures
always retire the private admission. Cycles, incomplete results and private SQL
remain fail-closed.

SQL-backed HTTP mappings use the admitted raw evaluator; GraphQL mappings bind
the scoped authority. Neither bypasses operator or ownership checks. Logical
resolution moves only a sealed checkpoint across short scoped query sessions:
complete child caches, validated membership and consumed budgets are tied to the
exact current token digest, fingerprint, database/profile and epoch. Fresh Access
is mandatory on resume, query time excludes separately bounded probe waits, and
query reports plus expanded package evidence share one byte budget. No checkpoint
retains a Store or connection.

Ordinary fallback policies admit only their immutable declared query dependencies
for global complete-ID evaluation, then apply current content/processing authority
to every candidate. Package-policy SQL requires an explicit SQL operator grant.
Package GraphQL installs scoped catalog views lazily in actual child order: later
SQL remains subject to those scoped/private-table restrictions. The frozen repeated
AuthorizedContext initialization failure is preserved as generic `catalog_error`
500 rather than silently widening a package. A real detached probe discards TEMP
scope; cached query results alone do not recreate it. Static GraphQL validation
precedes scope construction. Final policy output reloads the current token/grants,
while stale-epoch failures retain their frozen precedence and generic envelope.

## Transfer lifetime and deliberate hardening

Content setup validates bearer/ticket, exact revision, source exposure, private
content guards, descriptor identity, conditional headers and stream admission.
HEAD, 304 and rejected stream admission do not debit ticket budgets. Ticket debit
occurs under the actual authorized mutation owner. No catalog connection or
writer lock remains open while waiting for network capacity.

Before each at-most-64-KiB delivery chunk, the bounded channel reserves capacity,
then a fresh session revalidates current token/principal/grants, file/revision,
and ticket revocation/expiry. Descriptor metadata is checked before and after
reading. Playback segments retain descriptor proof and revalidate current
session/job/cache identity at the same boundary. This live authorization check
after backpressure is intentional hardening beyond the frozen descriptor-only
per-chunk behavior. Bytes already handed to the operating system cannot be
recalled. Disconnects release the stream guard and retire the descriptor.

SSE likewise reserves capacity before computing each newly authorized event page,
uses fresh sessions between one-second polls, binds durable cursors to principals,
and emits a terminal `resync-required` frame on revocation or invalid replay.
Writer contention produces bounded keepalive behavior; no session spans a wait.

## Evidence and how to reproduce

Use the pinned Rust environment and the clean Python 3.14.6 reference environment.
`tests/test_rust_http_transport.py` and
`tests/test_rust_http_transport_review.py` launch actual loopback native listeners,
not an in-process router mock. The consolidated HTTP lane covers frozen auth/privacy,
range/HEAD/conditional/ticket behavior, body bounds, route/method/media coercion,
live revocation/grant/expiry/disable during a blocked 16-MiB transfer, source
mutation, opaque cursor/snapshot interoperability in both engines, authorized
GraphQL, SQL isolation, operator preview rollback, mapping SQL ownership, SSE
revocation/stream limits/disconnect and writer release. Additional actual-worker
tests cover shared HLS encoding, playlist and exact segment transport, principal
isolation, no rendition publication, singleton locking, idle writer release and
SIGTERM cleanup. A deliberately stalled tool-identity probe proves no writer is
held during subprocess waits and token revocation is rejected on resume. Logical demand tests prove immutable source pinning and cached
replay without a currently available worker. Generated TypeScript journeys cover
webhook/mapping delivery and repair, watcher execution/history, and logical
REST/GraphQL resolution. Configured CORS and read-only behavior are compared with
the frozen middleware. The independent dynamic-grant suite compares SQL/GraphQL
membership, compositions, literal unions, paging, live metadata/grant revisions,
approved-selection behavior and direct/document privacy on separate catalogs.
Fresh review regressions cover a 5,002-ID grant used by its own approved query,
a declared 20,000-ID limit selecting 10,002 actual IDs, dynamic SQL/GraphQL
processing admission without poisoning the writer, and separate bounded policy
tiers sharing their cumulative budget. Internal tests reject cross-database,
changed-token/grant/query/epoch proofs and checkpoint reuse, and prove global
admission caches cannot enter scoped GraphQL. Non-null GraphQL privacy errors
bubble through list members and aliases/fragments to the frozen nullable boundary.

`npm run test:1.7` in `tests/clients` regenerates and strictly compiles the parallel
1.7 client. Generation/type checking alone is not behavioral acceptance.
`scripts/qualify_native_http.py --native PATH --python REFERENCE_PYTHON --root NEW_DIR
--generated-client tests/clients/v1_7/build/acceptance.js` creates only disposable
fixtures with Python, then uses the native CLI for server/worker restart, rendered
content, ticket/range delivery, event replay and generated-client execution.
No live service, real credential or external publication is part of these gates.
The actual-client fixture honors bounded `catalog_busy` retries only for safe
reads, read-only query POSTs, and keyed rendition admission. It never auto-replays
content/ticket redemption, ticket/snapshot creation, cancel/revoke, or unkeyed
mutations. Both Python and generated-client busy attempts are recorded, including
non-retried failures, without credentials or ticket query strings.

## Latest scoped qualification

The 2026-10-11 source-stable receipt contains 104 unique tests with zero failures,
errors or skips: 85 actual-loopback HTTP cases, 14 native GraphQL differentials,
and five hermetic retry-policy/evidence tests. The runner asserts frozen source
and frozen fixture modules before loading the native test lanes. Its copied
HTTP/CLI binaries and SHA-256 values are recorded in
`.local-tests/rust-migration/m4-plus/http-source-stable-final/report.json`.
The generated 1.7.0 TypeScript journey independently passed against native server
and combined-worker restart, with 32 HTTP calls and no busy retries; its receipt
is `http-source-stable-final/client/report.json`. The earlier lost process-handle
run remains outcome-unknown and was not recovered or reused as evidence.

Private-proof, checkpoint, writer-state and GraphQL owner unit groups pass, as do
strict HTTP Clippy, Rust formatting, Python lint/format and generated TypeScript
compilation. Independent authority and replay-policy reviews have no outstanding
findings. This qualification is scoped to the experimental loopback backend.

## Open release gates

- Private, unregistered, orphaned database-sidecar aliases are an explicit
  unresolved content-release blocker. Registered aliases and normal live
  database sidecars passing tests do not close this gap. The bounded
  [recognizable WAL/SHM prefix refusal](RUST_MIGRATION_PRIVATE_SIDECAR_HEADERS.md)
  also leaves damaged/unrecognizable orphaned sidecars unqualified.
- Component-policy logical demand deliberately retains the frozen requirement
  for explicit local component packaging admission. Package resolution itself has
  a separate independent-catalog HTTP privacy gate, including child-order and
  probe-time authorization cases; its evidence is recorded in the parent report.
- TLS termination, trusted reverse-proxy behavior, remote listeners, macOS process
  supervision and cross-platform descriptor/filesystem behavior are unqualified.
- MIME lookup reproduces the frozen Python 3.14.6 Linux registry resource, whose
  provenance and licenses are recorded beside it. Other OS MIME databases are
  not claimed equivalent.
- Full installed-wheel/helper qualification, remaining route workflows and full
  system acceptance must be green before default HTTP enablement is considered.
