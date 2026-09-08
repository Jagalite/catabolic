# Publishing renditions and importing external results

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Schema 12 connects generated media to catalog-specific output libraries, durable
rule requirements and externally delivered results. Run `catabolic docs renditions`
offline. Upgrade older databases explicitly with `db upgrade --dry-run`, then
`db upgrade`; the normal backup and preservation checks apply.

## A library of all transcoded videos

Select your database/profile. Create an empty output folder and bind it:

```sh
mkdir /absolute/path/transcoded-links
catabolic catalog bind transcodes --root /absolute/path/transcoded-links
catabolic rendition policy --catalog transcodes \
  --definition '{"purpose":"transcode","mode":"all"}'
catabolic layout put transcodes --preset catabolic
catabolic layout preview transcodes --catalog transcodes
catabolic layout apply transcodes --catalog transcodes
catabolic sync --catalog transcodes --dry-run
catabolic sync --catalog transcodes
catabolic verify --catalog transcodes
catabolic manifest --catalog transcodes --in-catalog
```

Review each preview before applying it. This publishes links to ready renditions
without activating their associations globally. Other catalogs retain their own
selection. Catabolic naming includes file IDs, so multiple versions can coexist.
Plex/Jellyfin naming can also be used when metadata is sufficient, but multiple
renditions may collide; choose a preferred copy or a more specific selection.

After saving a layout, `maintenance --catalog transcodes --manifest` refreshes it.
Adding `--rules --render-rules 1` also evaluates enabled rules and permits one
render. Source media and previous generated files remain intact.

## Automatic link updates

After applying a saved layout, enable `catalog-refresh enable --catalog transcodes`
to update links after rendition completion. Use `catalog-refresh watch` for durable
retries after restarts or unavailable destinations. This path never calls a media
server or queues library scans. See [automatic catalog links](CATALOG_REFRESH.md).

## Publication policy

`rendition policy --catalog NAME` reads the current profile's policy. Supplying
`--definition` saves it. Policies require at least one of:

| Field | Meaning |
| --- | --- |
| `purpose` | transcode, remux, preview, thumbnail, audio, subtitle or custom |
| `definition_id` | Exact immutable output-definition revision |
| `rule_id` | Exact rule revision in this profile; admits outputs from its tracked jobs |
| `mode` | `all` admits all eligible candidates and bypasses copy ranking; `preferred` requires an existing catalog copy policy |
| `include_originals` | Default false; true also admits ordinary active source associations before selection/ranking |
| `version` | Policy contract version, currently 1 |

Multiple filters are combined. A policy follows a rule **revision ID**, never a
mutable rule name. Changing a rule does not silently change publication policy.
`include_originals` supplies original candidates; whether one wins is determined
by the selection, naming and optional copy policy. See [copy selection](ENRICHMENT.md).
Files registered as renditions in any profile are not treated as originals.

Saved SQL/GraphQL layout selections further narrow the admitted candidates. For
example, a SQL selection can return `file_id` from `catalog_renditions` filtered by
purpose or producer. It can select an inactive rendition only when that catalog's
policy has admitted it. Other inactive associations remain excluded. Policy and
selection must use the same profile.

Policies evaluate up to 10,000 matching renditions and report eligibility failures.
Unknown/stale output evidence, unavailable sources and unverified external
registrations are excluded. This can propose link removals; normal preview and
removal budgets still apply. Clearing an entire managed layout requires
`--allow-empty` on layout application. Maintenance does not approve that implicitly.
Publication checks the live file revision, including change time. In-place edits
that preserve size and modification time invalidate the evidence. A fresh checksum
verification matching the recorded output digest can restore eligibility; older
verification facts cannot. Even metadata changes or creating a hardlink can require
fresh verification. For external files without a checksum baseline, run `process
enqueue hash` before `process enqueue verify`; publication still compares against
the original receipt digest.

When including originals without a saved query, planning refuses inventories over
100,000 active associations before filtering. Narrow the catalog with a saved
selection; a truncated inventory is never treated as a complete removal plan.

Exclude a particular rendition from one catalog, with a reason:

```sh
catabolic rendition exclude OUTPUT_ID --catalog transcodes --reason 'Keep the newer version'
catabolic rendition allow OUTPUT_ID --catalog transcodes --reason 'Reviewed and wanted here'
```

`allow` clears that catalog's exclusion; it does not bypass policy or readiness
checks. Decisions persist through maintenance and do not delete media. A disabled
global association is not a catalog-specific rejection; use `rendition exclude`
for that purpose. Existing explicit mappings are not silently adopted or removed
by a policy; the normal layout ownership checks apply.

Manifests describe the **active catalog projection**. A rendition admitted to one
catalog is exported as an active association in that snapshot while its global
database flag remains unchanged. This uses the existing Open Catalog contract;
no frozen schemas were edited.

## Rendition purpose and provenance

New output definitions may declare purpose explicitly:

```sh
catabolic rendition define mobile \
  --definition '{"version":2,"purpose":"transcode","role":"primary"}'
```

Purpose implies definition version 2 when version is omitted. Existing version 1
definitions retain their exact content/digest. Known built-in producing presets
supply a derived purpose for historical generated renditions. Unknown/custom
history is not guessed from codec, path or filename.

`rendition list`, `rendition show`, SQL `catalog_renditions`, and GraphQL
`rendition`/`renditions` expose `purpose`, `recipe_id`, `producer` and
`producer_instance`. External processor identities are producer claims stored in
an accepted receipt; ordinary registrations may have no producer.

```sh
catabolic query "SELECT file_id,purpose,recipe_id,producer FROM catalog_renditions WHERE profile=:profile AND purpose='transcode'"
catabolic graphql '{ renditions(first:20) { nodes pageInfo { endCursor hasNextPage } } }'
```

## External result receipt workflow

This is a local CLI exchange contract, not a built-in Tdarr/FileFlows/Unmanic
network adapter. A processor or wrapper records the source before processing,
writes outputs to a separate source location, and emits a receipt after delivery.
It must preserve original files. Keep external workers out of Catabolic-owned
generated locations and link catalogs.

1. Capture the source identity/revision before external work:

   ```sh
   catabolic --json rendition receipt-source --file-id SOURCE_FILE_ID --item-id ITEM_ID > source.json
   ```

2. Run the external processor. Bind its output directory as a source location and
   scan it after delivery. The directory must satisfy the normal binding rules.
3. Create `receipt.json` using the captured `database_id`, `profile` and `source`,
   and the remaining fields below. Outputs use bound location IDs and relative
   paths, never arbitrary absolute paths.
4. Import, obtain current probe facts when needed, then preview publication:

   ```sh
   catabolic --json rendition import-receipt --file receipt.json
   catabolic process enqueue probe --location external
   catabolic process run --limit 100
   catabolic layout preview transcodes --catalog transcodes
   ```

Receipt shape (replace example IDs, sizes, timestamps and digest):

```json
{
  "version": 1,
  "database_id": "DATABASE_UUID",
  "profile": "default",
  "producer": "external-processor",
  "instance": "home-server",
  "job_id": "job-123",
  "attempt": "1",
  "configuration_digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "tools": {"ffmpeg": "reported version"},
  "outcome": "complete",
  "source": {
    "file_id": "SOURCE_FILE_ID",
    "item_id": "ITEM_ID",
    "revision": {"size": 1000, "mtime_ns": 1, "device": 1, "inode": 1, "ctime_ns": 1}
  },
  "outputs": [
    {"location": "external", "path": "movie-mobile.mp4", "definition_id": "DEFINITION_ID", "size": 500}
  ]
}
```

`configuration_digest` is a lowercase SHA-256 of the producer configuration;
`attempt` is a nonempty string. Each output can also declare a lowercase `sha256`.
The importer always hashes delivered bytes and compares a declared checksum if
provided. It revalidates the source revision and all open output descriptors
before committing. All registrations in one receipt commit together or roll back.
Hash reads happen outside SQLite write transactions under the command's writer
lock. No source files are written.

Limits: 1 MiB JSON, 1–32 outputs, 1–32 declared tool versions, and a one-hour
hash-loop deadline. The deadline is checked between reads; it cannot bound an
individual stalled filesystem read. Outputs must already be scanned and present.
Stale sources, duplicate output paths, generated destinations, symlinks, checksum
mismatches and conflicting registrations are rejected. A source unavailable at
import time is not accepted through an offline bypass.

The idempotency key is profile + producer + instance + job ID + attempt. Repeating
the same payload returns the existing receipt without hashing again; this is not
a fresh verification. A different payload under that key is an error. Changing
job IDs cannot adopt an already registered output. Existing plain registrations
remain user-declared; receipt import does not silently upgrade their provenance.

Receipt acceptance validates delivery, not the claimed conversion or perceptual
quality. Automatic publication additionally checks current output metadata and
source revision. Transcode/preview/thumbnail/audio/subtitle purposes require a
current probe containing the corresponding stream kind. Other semantic quality
checks remain explicit processing/review work. `rendition register` is still
available for manually cataloged outputs, which can be published through the
existing explicit association workflow.

## Required rules and retries

Applying a `rule put --required` rule now records a semantic requirement for every
matched input/item pair after a full successful evaluation and storage preflight,
including matches deferred for missing probe facts and matches beyond the queue
batch. Merely saving or previewing a rule does not create requirements.

Each requirement tracks the current job evidence. An explicit retry can replace
that evidence; the failed/cancelled job remains in history and the worklog records
the replacement. Item completion uses the existing recorded-evidence evaluator,
shared by CLI, SQL and GraphQL. No query launches a scan or render.

Removed selections, disabled rules and new rule revisions do not erase old
requirements. Waive obsolete requirements with a reason through
`item resolve REQUIREMENT_ID --state waived --note '...'`. Reapplying a rule keeps
the waiver. Legacy schema-11 job requirements remain intact after migration and
may still need explicit waivers; migration does not reinterpret past decisions.
Storage-blocked or incomplete evaluations do not create or clear requirements.

## Acceptance checks and actual usage

Recipe options now include `max_size_ratio` (output bytes / input bytes) and
`require_duration`. They are opt-in and create a new immutable recipe definition.
For example, a ratio of 0.8 requires an output no larger than 80% of its input;
it does not guarantee visual quality. Duration requirements fail when necessary
duration evidence is unknown. Existing codec, stream, duration, byte-reserve and
timeout checks still apply.

```sh
catabolic --json rule stats RULE_ID
```

Statistics report recorded attempt counts, output bytes and elapsed seconds by
state, including old retained outputs. They do not infer space savings or an ETA.
Missing measurements remain null. Retroactive estimates and batch budgets remain
separate from actual historical usage. Local rendering uses a partial file in
the final filesystem; receipt import adds no media copy. External processors'
temporary storage is outside Catabolic's current estimates.

Local rendering remains serial. Schema 13 adds HTTP processor submission/polling,
distributed worker leases, automatic estimate calibration, and larger paginated
rule evaluation. See [network processors and workers](PROCESSORS.md) for commands,
the receipt-v1 integration contract, and the enforced limits.
