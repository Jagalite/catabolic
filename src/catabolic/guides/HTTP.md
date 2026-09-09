# HTTP catalog, content and rendition requests

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Catabolic's optional HTTP interface uses the same SQLite catalog, immutable
recipes, processing jobs, validated artifacts and projections as the CLI.
Applications can discover permitted media, request an approved durable rendition,
and retrieve its completed bytes. A request does not create a permanent rule,
change source trust, or configure Plex. Plex credentials and API credentials are
independent.

This interface does not implement uploads, remote URL ingestion, arbitrary encoder
commands, playback transcoding, HLS, browser accounts, sessions or OIDC. An encode
finishes and validates before its content becomes available.

## Install and start

From a checkout, install the optional dependencies alongside the normal package:

```sh
python -m pip install --require-hashes --only-binary=:all: -r requirements/http.lock
python -m pip install --no-deps --no-build-isolation -e .
```

An installed distribution also supports `pip install 'catabolic[http]'` when that
distribution is available from your package source. CLI-only installations do not
import FastAPI or require a server. Upgrade an existing catalog explicitly with
`catabolic --db catalog.db db upgrade`; review and back up the catalog using the
normal migration workflow first. Server startup only validates schema 22.

Create access locally using actual catalog IDs in `site-grant.json`:

```json
{
  "actions": ["metadata:read", "content:read", "events:read"],
  "item_ids": ["item-id"],
  "file_ids": ["file-id"],
  "metadata_fields": ["title", "year"]
}
```

```sh
catabolic --db catalog.db api principal-put site
catabolic --db catalog.db api grant-put site --file site-grant.json --id site-public
catabolic --db catalog.db api token-create site --grant site-public --ttl 3600 --output site-token.json
catabolic --db catalog.db api source-expose media
catabolic --db catalog.db api serve --host 127.0.0.1 --port 8421
```

The credential file must be new and is created with mode 0600. Keep it outside
media roots. Its `token` value is returned only in that protected file; the catalog
stores a SHA-256 digest. Send `Authorization: Bearer <token>` over trusted local
transport or TLS. Loopback clients must authenticate too. The example IDs must be
replaced; source exposure is an independent owner decision, not a file grant.

The default server mode permits queries and existing content, with no processing
admission or operator definition changes. It still writes ticket budgets, event
cursors and selection snapshots: this is not a read-only SQLite connection mode.
It never migrates, exposes sources or starts a worker automatically.

To enable owner-approved processing, approve an existing immutable render recipe
and an already bound generated destination, add its operation ID and
`processing:request` to the applicable grant, and run two supervised processes:

```sh
catabolic --db catalog.db api operation-approve RECIPE_REVISION --location generated --id browser-preview
catabolic --db catalog.db api source-expose generated
catabolic --db catalog.db api serve --enable-processing
catabolic --db catalog.db api worker --max-render-jobs 1
```

Use your service manager with absolute database, executable and credential paths;
keep the server and worker under the same local account. The worker command runs
in the foreground. `--once` processes at most one eligible job. Only one API worker
supervisor may own a database. No unmanaged daemon is started.

A minimal systemd service template for the server is:

```ini
[Unit]
Description=Catabolic HTTP catalog
After=network.target

[Service]
Type=simple
User=catabolic
ExecStart=/opt/catabolic/bin/catabolic --db /srv/catabolic/catalog.db api serve
Restart=on-failure
RestartSec=2
UMask=0077

[Install]
WantedBy=multi-user.target
```

For a separate worker unit, use the same account, database and executable with
`api worker --max-render-jobs 1` in ExecStart. Add `--enable-processing` to the
server only after configuring the intended grants and recipes. Adapt paths and
mount dependencies before installing either unit.

For launchd, the corresponding server job template is:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>local.catabolic.api</string>
  <key>ProgramArguments</key><array>
    <string>/absolute/venv/bin/catabolic</string>
    <string>--db</string><string>/absolute/catalog.db</string>
    <string>api</string><string>serve</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>2</integer>
</dict></plist>
```

Use a distinct `local.catabolic.worker` label and replace `serve` with `worker`
for the worker job. These are configuration templates; the acceptance script
checks foreground processes, not your host's service-manager installation.

## Access contracts

Actions are `metadata:read`, `content:read`, `processing:request`, `events:read`,
`sql:read` and `operator:write`. An action alone grants no resource access.
`operator: true` with the relevant action grants broad authority. Reserve it for
trusted operators. `actions: ["*"]` grants all actions, still subject to the grant's
resource policy unless it is an operator grant.

Grants accept explicit `item_ids`, `file_ids`, `operation_ids`, `projection_ids`,
`report_ids`, allowed `metadata_fields`, and optional `revisions` mapping file IDs
to exact revision strings. Item membership does not grant original file access.
`derivatives: true` grants derivatives of admitted items; use a separate content
grant with derivatives and no original file IDs when originals must remain private.
Processing permission can use an associated original without download permission.

A `query_id` pins an owner-approved saved selection revision returning complete
item IDs. Membership is reevaluated for each request. A changed query definition
requires a grant update. Explicit IDs/revisions provide snapshot grants. Partial
membership queries fail closed. Ordinary GraphQL sees authorized resources before
search, relationships, counts and pagination. Raw evidence, source paths and
worklogs are unavailable to scoped clients. Metadata JSON is filtered to permitted
keys (title and year by default). Report approval is an explicit disclosure grant:
`report_ids` can authorize a saved SQL report's broad output. App-created queries
cannot create grants.

Use `api tokens`, `token-rotate ID --output NEW_FILE`, `token-revoke ID`,
`principal-disable ID`, `grant-disable ID` and `operation-disable ID` locally.
`grant-put --id ID` replaces a grant and invalidates existing metadata cursors.
Token TTL is 1 second to 30 days. Current principal, token and grant revocation is
checked on every request, including tickets and event subscription polls.

## Versioned endpoints

Authenticated `GET /v1/openapi.json` publishes request schemas and the REST
contract. `GET /v1/me` returns effective identity/actions. `GET /v1/capabilities`
returns caller-visible approved operations, worker readiness and limits.

| Route | Contract |
| --- | --- |
| `POST /v1/query/sql` | Operator-only bounded read-only SQL; body `query`, `params`, `limit` |
| `POST /v1/query/graphql` | Standard GraphQL `query`, `variables`, `operationName`; normal data/errors response |
| `POST /v1/queries/{revision}/runs` | Saved selection, rows or document; body `limit`, optional `cursor` |
| `GET /v1/items`, `/v1/files`, `/v1/renditions` | Authorized metadata pages; `limit`, optional `cursor` |
| `GET /v1/items/{id}`, `/v1/files/{id}`, `/v1/renditions/{id}` | Individual metadata |
| `POST /v1/items/{id}/resolve` | Choose an existing eligible file, optionally narrowed by `file_id` or `definition_id`; never encode |
| `GET /v1/projections/{id}/entries` | Permitted output membership and relative paths |
| `POST /v1/items/snapshots` | Up to 10,000 currently visible IDs, retained for 15 minutes |
| `GET /v1/snapshots/{id}` | Principal-bound pages that recheck current access |
| `GET`, `HEAD /v1/files/{id}/content` | Require exact `revision`; optional narrow browser `ticket` |
| `GET`, `HEAD /v1/renditions/{id}/content` | Same contract, resolved through the rendition identity |
| `POST /v1/content-access` | Issue a revocable ticket for `file_id`, `revision`, optional `ttl`, `max_bytes` |
| `POST /v1/content-access/{id}/revoke` | Revoke an owned ticket |
| `POST /v1/rendition-requests` | Admit approved durable work with an `Idempotency-Key` header |
| `GET /v1/rendition-requests/{id}` | Current caller's fulfillment status |
| `POST /v1/rendition-requests/{id}/cancel`, `/retry` | Explicit demand lifecycle |
| `GET /v1/events` | Authorized polling; `cursor` for replay, `follow=true` for SSE |
| `POST /v1/operator/{queries,rules,projections}/{name}` | Preview and apply a validated definition with an expected plan digest |

Metadata pages distinguish `page_complete`, `has_more` and
`selection_complete`; pages are independent committed snapshots. Use ID snapshots
for consistent bulk enumeration. Revocation can remove IDs from later snapshot
pages. Cursors are opaque and tied to principal, token, grants and query scope.
Scoped authorization materialization is capped at 100,000 items/files and fails
closed above that bound. Page size is at most 1,000.

SQL retains its columns/rows contract and existing execution limits. It is broad
catalog-read authority: no string-appended WHERE clause simulates row security.
HTTP SQL denies security tables and secret-bearing worker/integration state,
including aliases and nested reads. Large SQL integers use `{"$integer":"..."}`;
file byte counts and nanosecond timestamps use decimal strings. GraphQL keeps its
existing scalar representations and error envelope.

REST failures use Problem Details (`application/problem+json`) with stable `code`,
request ID and remediation. Common outcomes are 401 `unauthorized`, 403 `forbidden`,
404 `not_found` (including inaccessible resources), 409 `revision_mismatch`,
`idempotency_conflict`, `stale_plan` or `resync_required`, 416
`range_not_satisfiable`, 429 limits, and retryable 503 `catalog_busy`.

## Content and browser use

Prefer a website backend that keeps the API token server-side. Issue short-lived
content tickets for browser media elements. At most 256 unexpired active tickets are permitted per principal. Tickets pin one resource revision,
allow reusable GET/HEAD range requests, expire after at most one hour (five minutes
by default), and cannot outlive their parent token. Their default byte allowance
is 1 GiB, configurable up to 100 GiB. Each admitted GET debits its requested bytes,
including transfers later interrupted; HEAD does not debit. New requests recheck
revocation. Already admitted transfers may finish after expiry or revocation.

Content admission checks source exposure, trust, availability, exact revision and
ready artifact state, then safely opens beneath the bound root. It streams that
descriptor after closing the Store. Catalog databases, credential files and
unregistered partial outputs are not content endpoints. No route accepts a path.
HTML and SVG are attachments; responses set nosniff, no-referrer and private/no-store.

GET/HEAD support full responses and single bounded, suffix and open-ended byte
ranges. Invalid or multiple ranges return 416 with `Content-Range: bytes */SIZE`.
Empty files return zero bytes. ETags are weak revision validators, not cryptographic
byte identities. `If-None-Match` supports 304; a specific `If-Match` fails because
no strong ETag is available. `If-Range` falls back to a full response. Source
replacement and in-place changes never silently select a new catalog revision.

An open descriptor protects against pathname replacement, not in-place writes.
Per-chunk mutation checks abort remaining delivery; bytes already sent cannot be
retracted. This release provides no managed immutable-copy API. Applications
requiring repeatable bytes should use separately managed immutable storage.

## Demand, execution and recovery

```json
{
  "item_id": "item-id",
  "source_file_id": "file-id",
  "source_revision": "revision-from-file-metadata",
  "operation_id": "browser-preview"
}
```

POST that body with a new `Idempotency-Key`. Pending admission returns 202 and a
Location/status URL; an eligible existing result returns 200. A changed body under
a reused key conflicts after access validation. Replays recheck current access.
The request record is separate from its shared existing job and artifact cache
identity. Cancelling a request does not cancel shared work or delete artifacts.
Retry is explicit for failed, blocked or cancelled demand. Missing worker readiness
rejects admission promptly. Unknown progress is `null`; blockers are explicit.

States are queued, running, validating, ready, blocked, failed, cancelled and
stale. Ready results include a rendition/file identity, revision and authenticated
content path. Source revisions and policies are revalidated; no read triggers
hidden prerequisite work. Results remain retained by default. Grant revocation
removes access immediately for new requests; admitted shared computation may finish.

Local analysis and render execution share claims with CLI execution and recovery.
Claims pin attempt, generation, source/options and destination; leases heartbeat
while the Store is closed during expensive computation. Completion fences against
the current claim. Recovery skips live workers. An interrupted attempt can require
explicit retry; repeated computation is possible, with one valid winning result.
CLI-originated job transitions update linked requests and the durable outbox.

Admission caps requests attached to unfinished jobs at 10 per principal and 100 globally. Cancelling demand does not refund that capacity while its shared job remains unfinished. One API render
runs at a time. Each unique job reserves its recipe's maximum output bytes on the
actual destination device, plus the recipe's free-space reserve. Shared demand
reserves once, terminal jobs release reservations, and retry must reserve again.
Recipe timeout, maximum output size and normal validation remain enforced.

Events retain 24 hours; worker iterations and HTTP writes perform bounded pruning. replay cursors expire after one
hour. Polling status is authoritative. SSE emits batches with opaque replay IDs,
reauthenticates each poll, closes after roughly one minute, and supports reconnect
via Last-Event-ID. Expired replay history requires resync. Idle subscriptions hold
no SQLite transaction and share stream limits. Event states describe current
request status, not an audit history of every intermediate state.

Operator definition preview runs existing validation inside a rolled-back
transaction. Apply requires the returned digest and rejects changed program state.
This endpoint edits definitions only; it does not execute a projection publication
or a rule backfill. Those consequential CLI workflows retain their normal preview
and execution contracts. Plans are bounded to 10,000 records per dependency table.

## Deployment limits and validation

Default limits are eight active streams globally, two per principal, 64 KiB
application chunks, 120 admitted requests per principal per minute and 1 MiB request
bodies. `--streams` and `--streams-per-principal` configure stream limits. Use one
API process and local SQLite storage. WAL is not enabled by this feature. Backup,
migration and recovery remain explicit local administration.

Non-loopback listening requires `--allow-network` and either `--tls-cert` with
`--tls-key` or `--trusted-local-transport` for an explicitly trusted reverse-proxy
hop. Set `--allowed-host` and optional narrow `--cors-origin` values. Forwarded
headers are ignored unless explicit `--trusted-proxy` addresses are configured;
wildcard proxy trust is rejected. Uvicorn access logging is disabled so ticket URLs
do not enter those logs. Configure the reverse proxy to redact query strings too.

Run the disposable installed journey with:

```sh
python scripts/http_acceptance.py --python /absolute/venv/bin/python --root /new/disposable/path
```

It creates local FFmpeg media and credentials, exercises GraphQL and a saved
selection, requests a rendition, restarts server/worker, checks recovery, obtains a
ticket, compares exact PNG range bytes, denies original download and replays events.
Reports exclude credentials. CI contains Linux/Python 3.11 and macOS/Python 3.14
installed lanes. Local macOS acceptance passed during implementation; Linux release
qualification requires that CI lane to pass. See [implementation evidence](HTTP_BACKEND_M0.md).
