# Try these queries in order

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Fallback policies select an existing, usable representation of each logical item.
An original, another occurrence, and an approved rendition are candidates in the
same ordered policy. Resolution never starts an encode, copies media, mounts a
source, or changes trust settings.

## Configure a logical projection

Use an item-ID saved selection for **membership**: for example, favorite movies.
Use separate saved selections for **candidate files**: originals on your NAS,
originals on backup storage, then existing local renditions. Pin their immutable
query IDs in a policy JSON file:

```json
{
  "version": 1,
  "fallbacks": [
    {"name": "main", "query_id": "MAIN_QUERY_REVISION"},
    {"name": "backup", "query_id": "BACKUP_QUERY_REVISION"},
    {"name": "local", "query_id": "LOCAL_RENDITION_QUERY_REVISION"}
  ],
  "within_tier": {"tie_break": "file_id"},
  "lineage_requirement": "accepted_source_revision",
  "failback": {
    "mode": "stable",
    "minimum_healthy_seconds": 300,
    "minimum_successful_checks": 2
  }
}
```

```sh
catabolic --db catalog.sqlite3 fallback put cinema --definition fallback.json
catabolic --db catalog.sqlite3 fallback bind cinema \
  --policy POLICY_REVISION --query MEMBERSHIP_QUERY_REVISION --layout movie-layout
catabolic --db catalog.sqlite3 fallback preview cinema
catabolic --db catalog.sqlite3 fallback execute cinema \
  --expected-plan PLAN_ID --max-changes 100 --max-removals 1
```

The output catalog must already have a symlink output binding, and the layout must
already exist. Start with a separate logical catalog when migrating an existing
exact-file projection. Binding refuses to adopt existing mappings or owned output
implicitly. Existing projections keep their previous selection behavior.

Preview returns a digest, decisions, evaluated tiers, blockers and planned paths.
Apply reruns resolution and rejects a stale digest. Query errors, truncation and
budget exhaustion are blockers, never empty tiers. Lower tiers are evaluated
lazily; explicit preferences rank candidates within each tier. Without a
`file_id` tie break, equally ranked usable candidates block as ambiguous.

## Availability and publication

An unavailable preferred source allows a lower tier to win. Logical entry IDs
remain stable when the physical file changes. A same-format switch retargets the
owned symlink through the existing recovery journal. A container change uses the
new extension and requires removal allowance for the old path. New links are
prepared before old links are retired; a group of renames is not atomic.

If any logical entry cannot resolve, the initial whole-projection gate retains
existing owned output. Membership stays recorded as unresolved or blocked. No
cleanup scan is triggered for an unhealthy tree. Resolution generation and
verified publication generation are separate; recovery advances publication only
after filesystem verification. Foreign destination files are never overwritten.

`accepted_source_revision` accepts a rendition whose own live output revision
matches accepted artifact evidence even while its original is offline. It reports
that the original was **not live checked**, or that a recorded change is known.
`current_source_revision` (the default) also requires current, live ancestry.
Legacy rendition readiness remains strict. Ordinary resolution reads a small
header and filesystem metadata; it does not hash a movie or invoke FFmpeg.

## Supervised automatic fallback

```sh
catabolic --db catalog.sqlite3 fallback enable cinema \
  --interval 60 --max-changes 100 --max-removals 1
catabolic --db catalog.sqlite3 fallback worker
```

Run the worker under your service supervisor. `worker --once` performs one due
pass. Automatic fallback is independent of all-source scans, so an offline NAS
does not prevent reevaluation. Existing catalog-refresh events also schedule
explicitly enabled fallback projections. Polling conservatively reevaluates
queries; it does not infer SQL dependencies. Disable with `fallback disable`.

Stable failback persists the healthy period and successful checks across process
restarts. Previews do not advance them. `immediate` returns to the first usable
tier immediately; `manual` keeps a usable fallback until preview and execute both
use `--failback`. A failed current candidate bypasses failback delay.

Publication and CLI recovery share a separate operation lock while source probes
release the database writer. Eight durable probe slots bound concurrent isolated
helpers. A timed-out helper retains its slot until it exits; repeated timeouts
suppress that source for the current batch. There is no unbounded thread pool.

Use `fallback schema` to discover limits. Defaults are 32 maximum tiers, 1,000
logical entries, 10,000 candidate records, 128 live checks, a five-second query
budget and a one-second helper deadline. Candidate query pages also have a 4 MiB
materialization budget. Narrow large collections or raise the explicit bounded
limits. The change limit includes reconciliation repairs (creates, replacements, removals
and ownership cleanup), even when the selected file has not changed. Output-owner
initialization is separate. The worker bounds changes/removals and retains the latest 10,000 history
records per projection. `fallback history cinema` returns recent transitions.

Verified changes feed normal consumer delivery. Plex/Jellyfin scan acceptance,
projection health and rendition validity remain independent. Optional
`fallback_selected` and `fallback_unresolved` notifications report transitions,
not every repeated healthy check.

## Components and evidence

Candidates must share the selected item, role, part and explicit
`item_files.metadata.variant`. Resolution does not follow `edition_of` or infer
identity from filenames. Publish intentional editions as separate logical slots.

`slot_roles` defaults to `["primary"]`. Add roles such as `subtitle` or `artwork`
only when their associations are curated. File-specific sidecars must carry
`item_files.metadata.for_file_id`; they are eligible only for the selected primary.
Item-level artwork can omit it. Every required slot must resolve before publication.
Multipart associations require one explicit `metadata.representation_group` across
all selected parts. Unclassified single-file substitutions and mixed groups block.
Hardlink fallback is explicitly unsupported; it never becomes a copy or symlink.

Catalog identities, occurrences and lineage remain distinct. Byte equivalence is
not inferred from title, filename or size; absent accepted digest evidence is
reported as `not_established`. No duplicate files are merged or deleted.

## HTTP and processing

Public API **1.1.0** adds approved policies to the existing logical resolver:

```http
POST /v1/items/ITEM_ID/resolve
Authorization: Bearer TOKEN
Content-Type: application/json

{"fallback_policy_id":"POLICY_REVISION"}
```

The typed response contains state, tier, file ID, revision and a concrete content
path. The token needs policy approval through `fallback_policy_ids`, item metadata
access, and independent content permission/source exposure for the chosen file.
Policy approval does not widen resource grants. Private candidates and their
rejection details are hidden. HTTP resolution is stateless; projection failback
history does not govern an application's choice.

`GET /v1/projections/CATALOG/resolutions` exposes authorized profile-local decisions
and publication generations. GraphQL exposes `fallbackPolicy` and
`fallbackResolutions`; SQL exposes `catalog_fallback_policies`,
`catalog_projection_resolution` and `catalog_resolution_history`.

`POST /v1/rendition-requests/logical` accepts `item_id`, `fallback_policy_id` and
`operation_id`, plus optional `role`, `part` and `variant`. It requires an
`Idempotency-Key`. Processing permission can allow use of an original without
allowing its download. Admission pins the chosen file revision in the normal job;
idempotency replay and retry retain that input. To choose a different input after
failure, submit a new logical demand with a new key. No running attempt switches
sources.

Existing content URLs and tickets remain exact. An outage terminates a failed
transfer; the application resolves again and decides how to resume a different
representation. Byte offsets are never reused implicitly across encodes.

Both 1.0.0 and 1.1.0 OpenAPI/GraphQL artifacts ship in the wheel. Generated-client
acceptance compiles against 1.1.0 and exercises the real installed HTTP server.

## Portability and qualification

`program export --fallback-policy POLICY_REVISION` includes immutable definitions
and query dependencies in a version-2 bundle. Existing exports without fallback
policies stay version 1. Imports remap dependencies and leave policies unbound;
credentials, live winners, health and automatic activation are not exported.

Migration 023 adds fallback records without replacing media, mappings, jobs,
credentials or journal history. Upgrades never enable fallback or HTTP exposure.

Run `scripts/fallback_acceptance.py --python /path/to/installed/python --root NEW_DIR
--generated-client tests/clients/build/fallback.js` after generating clients. It
uses disposable real media to prove A → B → transcode → unavailable → B → stable A,
container changes, retained membership, worker/server restarts, ranged bytes and
exact old URLs. Linux and macOS CI run the same installed acceptance; a local pass
alone is not evidence that the remote CI lanes passed.
