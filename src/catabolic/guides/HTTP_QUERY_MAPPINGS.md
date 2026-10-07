# Query-driven HTTP destinations

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Saved queries select data, version 3 destination mappings construct requests, and
watchers reevaluate the selection. Applying a reviewed plan freezes new or changed
requests in the existing durable notification queue. Queries and previews never
make network requests. A declared HTTP 2xx response that passes its pinned response schema records delivery
acknowledgment, not completion of
work performed by the receiving service. Media-producing external work continues
to use the processor/receipt protocol.

Requires schema 32 and the optional `catabolic[openapi]` extra. For a pinned
installation use `requirements/openapi.lock` with `--require-hashes
--only-binary=:all:`. Upgrade an existing catalog explicitly with
`catabolic --db catalog.db db upgrade`. HTTP management is part of API 1.7.0 and
also requires the HTTP extra.

## Approve a receiver operation

Supply a local OpenAPI 3.1 JSON document, a unique operationId, and an explicitly
chosen base URL. Import never downloads the specification, follows remote schema
references, or chooses a server from the document. The operation's path is appended
to the base URL, including any base path.

```sh
catabolic --db catalog.db --json projection http-operation-put receiver \
  --spec receiver-openapi.json --operation-id notifyReady \
  --base-url https://receiver.example/api --credential-env RECEIVER_TOKEN
```

The selected operation must use POST, PUT, or PATCH. Supported request bodies are
`application/json`; path and query parameters must be scalar strings, numbers,
integers or booleans with the standard simple/form encoding. Runtime path values
cannot contain slash, backslash, percent, controls, or dot-only path segments.
Authentication may be one HTTP Bearer or header API-key security scheme. The named
environment variable holds its value on the delivery worker; omit
`--credential-env` for an operation with no security requirement. Catabolic's own
Bearer credential is never forwarded. OAuth flows, cookie/query API keys, mapped
headers, multipart bodies, alternate parameter serialization and OpenAPI 3.0 are
not implemented.

Resolved request schemas are validated with JSON Schema 2020-12. Local acyclic
`$ref` values are supported; external references, recursive/dynamic references,
`$id`, reference siblings other than descriptions, alternate dialects and regex
schema keywords are rejected. Formats are annotations. These are deliberate,
reported import limits, not silently ignored validation. Specifications are
limited to 1 MiB, reference expansion to 4096 nodes and depth 32. Importing a spec
is local-owner approval of the selected operation and its validation rules.

The returned ID pins the selected method, path, schemas, credential reference,
base URL and specification digest. Changed definitions create a new revision;
existing mappings retain their old operation ID. Old revisions remain enabled
until explicitly disabled. Identical imports reuse their revision, including its
current disabled state.

```sh
catabolic --db catalog.db projection http-operation-list
catabolic --db catalog.db projection http-operation-show OPERATION_ID
catabolic --db catalog.db projection http-operation-disable OPERATION_ID
```

## Map a saved query

Save a `rows` SQL query using the ordinary query commands. For example:

```json
{
  "version": 1,
  "mode": "rows",
  "selection": {
    "language": "sql",
    "query": "SELECT item_id AS id, title FROM catalog_items WHERE kind='movie'"
  }
}
```

```sh
catabolic --db catalog.db --json query save receiver-items --definition query.json
```

Use the returned query revision and operation ID in `mapping.json`. This example
assumes the receiver's approved operation defines `/items/{id}/ready` and accepts
an object with `id` and `title` fields:

```json
{
  "version": 3,
  "id": "notify-ready-v1",
  "query": "QUERY_REVISION_ID",
  "operation": "OPERATION_ID",
  "key": {"column": "id"},
  "parameters": {"path": {"id": {"column": "id"}}},
  "body": {
    "id": {"column": "id"},
    "title": {"column": "title"}
  }
}
```

Bodies can nest objects and arrays of the existing column/constant/lookup/JSON
expressions. Use `{"constant": value}` for a literal, including null or a whole
object. Mapped parameter names and bodies must satisfy the pinned request schema.
Keys are nonempty strings and identify a record within this mapping. Conflicting
rows for one key fail the whole plan; identical rows collapse into one request.

```sh
catabolic --db catalog.db --json projection mapping-preview --definition mapping.json
catabolic --db catalog.db --json projection mapping-apply \
  --definition mapping.json --expected-plan PLAN_ID
catabolic --db catalog.db notify run --limit 100
catabolic --db catalog.db projection mapping-events
```

Preview returns exact method, URL and body snapshots, without authentication
values. Apply reevaluates the query and rejects a changed plan. Admission is
atomic: validation or change-budget failure queues nothing. A mapping ID pins its
definition after first apply; use a new mapping ID for a changed definition.

A mapping evaluates at most 1000 complete rows and groups at most 100 requests.
Each JSON body is at most 64 KiB and the complete plan is at most 4 MiB. Splitting
large selections into explicitly separate query/mapping definitions preserves
these completeness bounds. Truncation never substitutes for a complete selection.

A GraphQL `document` query is also supported. Add `"rows_path": ["items"]` to
select a connection in its `data`. That connection must contain object `nodes`
and explicit `pageInfo.hasNextPage: false`. For example:

```graphql
{ items(first: 100) { nodes { id title } pageInfo { hasNextPage } } }
```

Nested connections also require explicit complete pageInfo. The mapper does not
automatically paginate arbitrary GraphQL documents. A partial connection fails
without advancing the watcher baseline or queueing requests.

## Reevaluate with a watcher

Put the same mapping definition in a watcher using `reaction: "http"`:

```json
{
  "plan": {"kind": "query", "query_id": "QUERY_REVISION_ID"},
  "schedule": {"kind": "interval", "seconds": 60},
  "reaction": "http",
  "mapping": {
    "version": 3,
    "id": "notify-ready-v1",
    "query": "QUERY_REVISION_ID",
    "operation": "OPERATION_ID",
    "key": {"column": "id"},
    "parameters": {"path": {"id": {"column": "id"}}},
    "body": {"id": {"column": "id"}, "title": {"column": "title"}}
  },
  "max_changes": 100
}
```

```sh
catabolic --db catalog.db watcher put notify-ready --file watcher.json
catabolic --db catalog.db watcher run notify-ready
catabolic --db catalog.db watcher enable notify-ready
catabolic --db catalog.db supervise
```

`watcher run`/`supervise` evaluate and queue; run `api worker` separately for
background delivery, or periodically invoke `notify run`. Enabling a watcher
explicitly authorizes its bounded future admissions. No hook recursively invokes
another rule. Existing watcher observation/freshness controls still apply, including
`require_complete_inventory` when complete current source coverage is necessary.

The ledger compares each key's most recently admitted request digest. Unchanged
keys do not resend, even after failure: delivery retries belong to the queue.
Changed requests replace older unsent versions; a live delivery lease blocks
replacement and the whole admission rolls back. Missing keys never send deletes
or cancel historical deliveries. Reappearing unchanged keys do not resend.
A query change after admission does not rewrite the frozen request; explicit
reevaluation may supersede it if it has not been dispatched.

Deliveries retain one event ID across retries and send it as `Idempotency-Key`.
Receivers must deduplicate it; response loss can cause duplicate execution if the
receiver does not honor that key. Disabling an operation cancels queued work.
Authentication values are resolved at dispatch and never stored in the payload
or logs. Raw mapped payloads are stored privately and can contain catalog data.
Requests have the existing 20-second socket and 30-second process deadlines, no
redirects or environment proxies, and five attempts with bounded backoff.

```sh
catabolic --db catalog.db projection http-operation-retry OPERATION_ID
```

Retry refuses active leases and disabled operations. It never revives cancelled
or superseded requests. `mapping-recover` can inspect delivery state for these
mappings but does not read back or verify the remote service's state. For remote
processing results, use an external processor and an accepted receipt.

## HTTP management

All endpoints below require operator grants for both `webhooks:manage` and
`sql:read`. Read-only servers permit preview and inspection but reject mutations.
Saved-query evaluation retains normal HTTP SQL privacy restrictions and GraphQL
resource/field permissions. Other principals' API-owned operations are hidden;
local-owner CLI operations may be used by an authorized operator.

| Endpoint | Purpose |
| --- | --- |
| `POST /v1/http-operations` | Approve `{name,spec,operation_id,base_url,credential_env?}` |
| `GET /v1/http-operations` | Page visible operation revisions |
| `GET /v1/http-operations/{id}` | Inspect the pinned operation |
| `POST /v1/http-operations/{id}/disable` | Disable and cancel unfinished delivery |
| `POST /v1/http-operations/{id}/retry` | Reauthorize and retry unfinished delivery |
| `GET /v1/http-operations/{id}/deliveries` | Page delivery acknowledgment and attempt history |
| `POST /v1/http-mappings/preview` | Preview `{definition}` |
| `POST /v1/http-mappings/apply` | Admit `{definition,expected_plan,max_changes?}` |

List endpoints accept `limit` (1–1000, default 100) and `after`; pass the returned
`next_cursor` as `after`. API approvals and admissions retain their authorizing
token IDs. Revoked/expired permissions block delivery; explicit retry binds the
unfinished operation deliveries to the current authorized token. Local CLI retry
is an explicit local-owner reauthorization. Acknowledgment remains distinct from
remote processing completion in both CLI and HTTP responses.

## Validate receiver responses

Approval pins success responses as well as requests. Delivery matches an exact
2xx status first, then `2XX`, then `default`. Undeclared success statuses fail
with `unexpected_response`. JSON response bodies must be UTF-8
`application/json`, at most 64 KiB, and satisfy the pinned JSON Schema. Missing
fields, malformed JSON, duplicate keys, nonfinite numbers, wrong media types,
and oversized bodies fail with `invalid_response`. A response without declared
content must have no body. Response headers other than content metadata and
compressed or non-JSON response bodies are not validated/supported.

These failures enter `repair`; response bodies are never stored or exposed.
Use the operation retry command after repairing the receiver. Retried requests
retain their idempotency key because the receiver may already have performed
the action before returning an invalid response. Non-2xx and transport failures
retain bounded automatic retries. Generic notification webhooks keep their
existing 2xx acknowledgment behavior.

Only selected success/default response schemas are resolved; error response
schemas do not participate in successful acknowledgment. The same local-reference
and schema limits apply to responses, with a 64 KiB compiled response-contract
budget. Previously approved operations without a response contract must be
reimported and mapped using their new operation revision; old deliveries enter
repair. No queued request is silently rebound to a new contract.

Contract validation confirms the receiver's declared response. A real service's
resulting state still requires a service-specific read-back or completion hook.
The local integration fixture verifies both the response and an HTTP read-back
of the stored item, including idempotent retry behavior.
