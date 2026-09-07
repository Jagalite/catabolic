# Network processors and distributed workers

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Schema 13 adds a versioned HTTP receipt adapter, durable processor jobs and fenced
worker leases. It builds on schema 12's validated external receipts and catalog
publication. Local FFmpeg rendering still uses the existing artifact executor;
remote work uses an explicit processor endpoint. No daemon starts on scan or
ordinary maintenance.

## Configure and queue work

A processor is a service implementing the receipt-v1 contract below. Tdarr,
Unmanic and FileFlows require a bridge implementing that contract; these commands
do not speak their proprietary APIs. No vendor service is configured implicitly.
Processor definitions are immutable; choose a new name when an endpoint, capacity
or credential environment changes. Secrets belong in the named environment
variable, never in endpoint URLs, configuration JSON or the catalog database.

```sh
catabolic processor put encode-server --definition '{"endpoint":"https://processor.example/api/catabolic","credential_env":"CATABOLIC_PROCESSOR_TOKEN","capacity":2,"timeout":15}'
catabolic rendition define external-mobile --definition '{"purpose":"transcode","role":"primary"}'
catabolic processor enqueue encode-server --file-id SOURCE_FILE_ID --item-id ITEM_ID --output-definition DEFINITION_ID --configuration '{"preset":"mobile","output_location":"delivered"}'
catabolic processor jobs encode-server
```

Capture happens at enqueue: the request pins database/profile, source identity
and revision, output definition and configuration digest. Repeating the same
request returns the same job. A changed source/configuration produces a distinct
job. A failed request is not automatically restarted: use `processor retry JOB_ID`.
Source changes require rescanning and enqueueing the new source revision.

Bind the delivery directory as a normal source location and scan it before receipt
acceptance. External services must retain originals and deliver separate files;
Catabolic-owned generated locations and catalog link directories are not external
worker destinations. The service maps logical source locations/relative paths to
its own mounts; the coordinator does not send arbitrary host roots or transfer
media bytes over this protocol. Delivery configuration and mount maps belong to
the service or bridge.

## Run workers

Each worker has a distinct stable name. A tick claims or resumes one of that
worker's live leases, extends it, and performs one bounded submission/status check.
Schedule repeated ticks from your supervisor at intervals shorter than the lease.
The timeout bounds the entire HTTP request, including DNS and slow response bodies.

```sh
catabolic processor tick encode-server --worker node-a --lease-seconds 300
catabolic processor tick encode-server --worker node-b --lease-seconds 300
catabolic processor job JOB_ID
```

The coordinator's database stays on its local filesystem. Distributed executors
communicate through the configured HTTP service. If controllers run on different
hosts, invoke the CLI on the coordinator through your existing authenticated
transport; do not share a writable SQLite database across network mounts. Capacity
limits simultaneously live leases per processor. A competing CLI may receive the
normal writer-busy error; retry that tick later. HTTP requests release the writer
lock so other workers can heartbeat, claim and finish during network I/O.

Manual worker control is also available:

```sh
catabolic --json processor claim encode-server --worker node-a > lease.json
catabolic processor dispatch --lease-file lease.json
catabolic processor heartbeat --lease-file lease.json --lease-seconds 600
catabolic processor complete --lease-file lease.json --receipt-file receipt.json
catabolic processor cancel JOB_ID
```

Treat lease files as capability tokens. Ordinary job/list responses omit tokens.
Cancellation fences local acceptance immediately; it does not kill an already
running external encoder. The service must manage its own resource limits and
cancellation. Reclaiming an expired lease increments its generation and preserves
the previous attempt as expired. An old worker cannot heartbeat, change status or
register outputs after replacement. Receipt hashing and final job completion share
one transaction; expiry during validation rolls back both registration and job
completion. Long receipts may require a longer lease (maximum one hour).

## HTTP receipt-v1 contract

All requests use the configured endpoint, refuse redirects, and optionally carry
`Authorization: Bearer <environment value>`. No cookies, ambient proxy credentials
or server-supplied polling URLs are used. Responses are limited to 1 MiB.

| Request | Contract |
| --- | --- |
| `PUT /jobs/{job_id}/{generation}` | Submit the JSON request, or return the existing state for this exact attempt. Repeated PUTs must be idempotent, including after a lost response. |
| `GET /jobs/{job_id}/{generation}` | Return the current state for that exact attempt. |

The PUT body contains `version:1`, `database_id`, `profile`, `source` (the receipt
source identity/revision), `source_location` (`location` and relative `path`),
`definition_id`, `output_definition`, `configuration`, `configuration_digest`,
`job_id`, `attempt` (the generation as a string), `producer` (processor name), and
`instance` (controller worker name). It contains no lease token.

Responses are JSON objects with `state` equal to `queued`, `running`, `failed` or
`complete`. A complete response must include `receipt`, using the exact schema in
[rendition workflows](RENDITION_WORKFLOWS.md). The receipt must echo the requested
source, database, profile, producer, instance, job ID, attempt and configuration
digest. Every delivered output must use the requested output definition. The
normal importer hashes files and checks delivery, lineage, bindings and revisions.
Automatic catalog admission still requires the appropriate current probe evidence.

The service must reject older generations once it accepts a newer one, use
attempt-specific output paths, and never overwrite files delivered by another
attempt. Distributed execution is at least once: network failures or an expired
lease can leave remote work running. Fencing prevents stale result acceptance;
it cannot make a noncompliant service stop encoding or modifying a shared file.
Do not publish unfinished outputs or advertise complete until delivery is durable.

A lost PUT acknowledgement leaves the local job leased; the next tick repeats the
same PUT. An accepted running response changes it to submitted; later ticks GET
that attempt. Invalid/unscanned receipts leave the lease pending, so fix delivery
or scan the output location and retry before expiry. Publication remains explicit:
select a rendition policy, preview/apply its layout, sync and verify.

## Calibration and larger rules

Rule previews and statistics automatically calibrate expected bytes after three
distinct source files have completed local renders for the same recipe/profile.
The sample contains at most the newest 100 distinct source files, using each
source's latest successful output and its saved input probe. Retried renders of
one file do not multiply its weight. The median actual/heuristic ratio adjusts
expected bytes; sample extrema describe a range. Planning uses at least the old
high estimate and may increase it with measured output plus 25% allowance.
Unknown estimates remain unknown. Explicit `--estimate-video-kbps` assumptions
take precedence. This does not change recipes, encoder settings, existing outputs,
or infer savings or quality. External receipts lack locally validated recipe
measurements and do not train this estimator.

Large saved selections can opt into complete pagination:

```json
{
  "language": "sql",
  "query": "SELECT item_id FROM catalog_items WHERE kind='movie'",
  "page_size": 1000,
  "max_ids": 100000,
  "timeout_ms": 60000
}
```

SQL reads sorted, distinct IDs in keyset pages against the locked catalog state.
Volatile random/time functions are rejected for paged SQL; pass a fixed cutoff as
a query parameter instead. `max_ids` defaults to 10,000 and is capped at 100,000;
large SQL selections require `page_size` (1..9,999). GraphQL retains its existing
cursor contract and also accepts `max_ids` up to 100,000. Expanded associations and
rule inputs are capped at 100,000. Details and render/enqueue batches stay bounded.

Any page error, timeout, invalid ID or limit overflow aborts the complete evaluation
before required-rule records or mappings change. Pagination bounds query pages;
the final ID set and rule plan remain in memory up to the documented cap. This is
not an unbounded streaming engine or a resumable evaluation across CLI invocations.
