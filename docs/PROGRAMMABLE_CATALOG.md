# A programmable media catalog

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Catabolic is a programmable media catalog.

**Queries decide what media you mean. Rules decide what should be produced or
analyzed. Projections decide how selected media should appear to another
application.** SQLite records the evidence connecting all three.

```text
committed catalog state
  └── query
       ├── rule → durable job → attempt → validation → committed catalog evidence
       │                                                  └── query again
       └── projection → desired mappings → journaled reconciliation → verification
```

There is no second workflow database or recursive event engine. A completion may
invalidate opted-in catalog outputs, but it never directly invokes another rule.
Run another bounded evaluation, or an explicit maintenance cycle, to discover
new work. Jobs and attempts survive changes to the query that originally selected
them; use job inspection, retry, cancellation and recovery commands for that work.

## Queries

A saved query is an immutable, named revision with one of three contracts:

- `selection`: a **complete** set of `item_id`, `file_id` or `association_id` values.
  Rules and projections accept this contract.
- `rows`: arbitrary read-only SQL results, retaining row caps and explicit truncation.
- `document`: an arbitrary bounded GraphQL document, retaining connection pagination.

The language implementations retain their existing capabilities. SQL can join
normalized catalog views and underlying tables. GraphQL retains nested objects,
variables, introspection and cursor connections. A saved selection drives its
pages to completion; an arbitrary document does not implicitly drain connections.

Save a definition, then pass its returned immutable `id` to consumers:

```json
{
  "version": 1,
  "mode": "selection",
  "entity": "item_id",
  "selection": {
    "language": "sql",
    "query": "SELECT item_id FROM catalog_items i WHERE kind='movie' AND NOT EXISTS (SELECT 1 FROM catalog_rendition_state r WHERE r.profile=:profile AND r.source_item_id=i.item_id AND r.purpose='transcode' AND r.recorded_current=1)"
  }
}
```

```sh
catabolic --db catalog.sqlite3 --machine query save missing-transcode --definition query.json
catabolic --db catalog.sqlite3 --machine query show QUERY_ID
catabolic --db catalog.sqlite3 --machine query run QUERY_ID
catabolic --db catalog.sqlite3 query --schema
catabolic graphql --schema
```

`query list --max-rows 100 --after ID` pages saved revisions. Saving identical
content under the same name reuses the revision. Changed content creates a new
revision; existing consumers keep the old ID until explicitly reconfigured.
Parameters and GraphQL variables are pinned in the definition.
Lists omit definitions larger than 8 KiB with `definition_omitted`; retrieve the
complete definition with `query show ID`. GraphQL discovery uses the same bound.

Compose selection revisions with `{"combine":"difference","queries":["A","B"]}`,
`union` or `intersection`. References must have the same profile and return the
same entity. Composition permits 2–16 inputs per node, at most 64 distinct nodes,
depth 16, a cumulative ID budget and an elapsed-time budget. Dependencies are
pinned IDs, so editing a named query cannot rewrite a composition.

Selections default to 10,000 IDs and 5 seconds; explicit limits permit up to
100,000 IDs and 60 seconds. Larger SQL selections require `page_size` (up to
9,999). See [selection pagination](QUERY_FOLDERS.md). Volatile SQL functions such
as `random()` and current-time expressions are rejected for mutation selections.
Empty is a valid complete result. Timeout, malformed IDs, unknown IDs and truncated
results are errors, never substitutes for an empty result. Displaying only part
of a complete saved selection sets `details_truncated`; it does not silently
change the selection used by a rule or projection.

GraphQL can express the same common gap:

```graphql
query Missing($after: String) {
  items(kind: "movie", missingRendition: {purpose: "transcode", current: true},
        first: 100, after: $after) {
    nodes { id }
    pageInfo { hasNextPage endCursor }
  }
}
```

`hasRendition` and `missingRendition` accept purpose, immutable operation (`recipe`),
output definition (`definition`) and recorded currentness (`current`). Omitting
`current` includes historical and declared outputs. Specify `current: true` when
asking whether current accepted output evidence fills a gap. SQL remains the
mechanism for arbitrary technical predicates, multi-table joins and recursive
lineage inspection; GraphQL is not an embedded SQL language.

`catalog_rendition_state` exposes source/output snapshots, recorded currentness,
acceptance, operation identity/version, checksums and current technical summary.
**Recorded currentness is not live readiness.** Inventory does not record ctime;
receipt source snapshots do not include bound root identity. Unknown evidence
stays NULL, and this view compares immediate source revisions. Live publication
validates root identity, ctime and the full rendition ancestry (bounded to 64).
Current accepted receipt evidence also does not mean a rendition is ready for
publication: media purposes such as transcodes still need the required current
probe facts. Queries for a codec or resolution should inspect those facts, not
infer them from a purpose label. An offline catalog cannot prove that bytes or
mounts have not changed since its last observation. Queries can inspect `catalog_facts`, observations and scans to
make evidence requirements explicit.

Examples of other gaps:

```sql
-- Files lacking a current full checksum.
SELECT f.file_id FROM catalog_files f
WHERE f.profile=:profile AND f.status='present'
AND NOT EXISTS (SELECT 1 FROM catalog_facts x
  WHERE x.profile=f.profile AND x.file_id=f.file_id
    AND x.operation='hash' AND x.current=1);

-- Select current generated copies directly for a projection or later rule.
SELECT file_id FROM catalog_rendition_state
WHERE profile=:profile AND purpose='transcode' AND recorded_current=1;

-- Results whose immediate source revision has changed in inventory.
SELECT file_id FROM catalog_rendition_state
WHERE profile=:profile AND source_current=0;
```

## Rules and operation definitions

`rule put NAME --query QUERY_ID --operation OPERATION_ID` connects a query to an
immutable operation. `--recipe` and inline `--selection FILE` remain compatible.
Definitions reuse the existing recipe registry; existing recipe IDs are operation
IDs. There are three execution kinds:

| Kind | Definition example | Existing execution owner | Recorded result |
| --- | --- | --- | --- |
| analysis | `{"kind":"analysis","operation":"hash"}` | `processing.py` | Jobs, attempts, facts and checksum baselines |
| render | `{"kind":"render","operation":"h264-720p"}` | `artifacts.py` / `rendering.py` | Validated artifact, occurrence, association and lineage |
| external | `{"kind":"external","operation":"processor-name","output_definition_id":"ID","options":{}}` | Fenced `processor` workers and receipt imports | Accepted receipt, occurrence, output and lineage |

```sh
catabolic operation types
catabolic --machine operation schema
catabolic --machine query contract
catabolic --machine projection schema
catabolic --db catalog.sqlite3 --machine operation put checksum --definition operation.json
catabolic --db catalog.sqlite3 --machine operation list
catabolic --db catalog.sqlite3 --machine rule put checksums --query QUERY_ID --operation OPERATION_ID
catabolic --db catalog.sqlite3 --machine rule preview RULE_ID
catabolic --db catalog.sqlite3 --machine rule apply RULE_ID --batch 100
catabolic --db catalog.sqlite3 --machine rule run RULE_ID --batch 100
```

Analysis supports `sniff`, `hash`, `verify`, `probe`, `text` and `decode`. Full
verification needs an existing checksum baseline; text extraction supports UTF-8
text, Markdown, SRT and VTT, plus explicit optional PDF-text and image-OCR backends.
Render operations retain the installed presets:
thumbnails, short previews, MKV remuxing, AAC/FLAC audio, SRT extraction, and
H.264 720p/1080p, AV1/Opus, HDR-to-SDR, waveform PNGs and normalized FLAC.
`artifact capabilities` checks the actual installed FFmpeg build.
Render rules require `--location` pointing to explicitly owned generated storage.
Item selections default to primary associations; explicit file or association
selections can choose other roles, including extracted audio. Generated inputs
still require `--allow-derived` and live rendition readiness checks.
Analysis rules require no output directory or logical item unless `--required`
needs an item association.

External rule application queues the existing processor jobs. `rule run RULE_ID
--worker WORKER --batch N` performs at most one network step per selected tracked
job, releasing the writer lock during HTTP and leaving unrelated jobs alone. Use
the same worker identity on a later invocation to resume polling. Without
`--worker`, `rule run` reports state and handoff. Existing `processor tick NAME
--worker WORKER` and explicit claim/dispatch/complete commands remain available. HTTP runs outside the
SQLite writer lock. Receipts remain fenced by generation, lease, source revision,
configuration and independently validated delivered files. External storage and
runtime are unknown; local byte budgets cannot authorize an external batch.
See [the processor protocol](PROCESSORS.md).

A rule's queueing result is distinct from execution success. Jobs may be queued,
running, failed, changed or cancelled; an attempt may succeed while publication
remains pending. A validated artifact and an accepted rendition are catalog
evidence; neither implies a healthy output projection. `--required` records a
persistent item requirement, including matches not yet queued, and tracks local
or external job evidence. A gap becoming empty does not delete that requirement.

New rule revisions start disabled for maintenance. Enable/disable controls future
maintenance evaluation, retaining jobs, receipts and history. Explicit per-rule
apply/run and explicit job commands remain available for a disabled revision.
`--retry-failed` is deliberate; changed sources need a new snapshot/job. Two rules
can adopt the same operation/source work without duplicating its execution.

Render rules exclude derived inputs by default. `--allow-derived` explicitly
admits cataloged renditions, revalidating readiness and ancestry before execution
and publication. A second query connects stages; no DAG or implicit cascade is
created. Existing job commands remain available when a previously queued job no
longer matches a dynamic query.

## Projections

A projection is a catalog output configured with a query, copy/rendition policies,
a reusable layout and a bound target directory. Catalog IDs, mappings and owned
links retain their existing meanings and IDs. As before, catalog mappings and
copy policies are shared across profiles; profiles bind machine-specific paths.
Use distinct catalog IDs for independent projections rather than treating two
profiles of the same catalog as independent desired libraries. One query can feed several
projections, and one layout can serve several queries.

```sh
catabolic --db catalog.sqlite3 --machine projection put mobile --query QUERY_ID --layout flat --renditions rendition-policy.json
catabolic --db catalog.sqlite3 --machine projection preview mobile
catabolic --db catalog.sqlite3 --machine projection execute mobile
catabolic --db catalog.sqlite3 --machine projection verify mobile
catabolic --db catalog.sqlite3 --machine projection enable-auto mobile
```

Definition shape schemas are discoverable without opening a database. They do
not replace semantic validation of references, budgets and selection completeness.

The catalog and layout must already exist; use `catalog bind NAME --root PATH` and `layout put` as
before. `--copies FILE` applies an existing copy policy; `--renditions FILE` applies
an existing rendition publication policy. For example `{"purpose":"transcode"}`
admits current validated transcodes. The copy planner explains exclusions,
unknown metadata and preferences. See [copy selection](ENRICHMENT.md) and
[rendition admission](RENDITION_WORKFLOWS.md) for supported policy fields.
A codec/quality preference does not imply that a corresponding encoder exists.

Preview stages mappings in a rollback transaction and runs the same reconciler
preflight as execution. It does not change persistent mappings or the filesystem.
Execution uses journaled reconciliation followed by verification. Previews retain full safety checks while bounding displayed action details;
`action_count`/`actions_truncated` distinguish the full plan from its display.
Default removal budget is zero; shrinking a published selection requires a reviewed explicit
`--max-removals N`. Retiring every generated mapping additionally requires
`--allow-empty`. Changing the layout owner requires `--replace-layout`.
Unrelated manually managed mappings and original files remain untouched.

Automatic updates reuse the durable catalog-refresh queue and bound query.
Unavailable destinations leave publication pending. `catalog-refresh run` retries
links without rendering again; `projection recover` recovers pending filesystem
journals. Automatic refresh does not invoke a Plex/Jellyfin scan. A projection
configuration change takes effect on explicit execution or a later refresh; it
is not itself an immediate filesystem mutation.

## Compatibility, evidence and limits

Manifest exports include [scoped statistics](OPEN_CATALOG.md#statistics-extension)
under `content.extra["catabolic:statistics"]`: recorded selection bytes/counts,
rule evaluation and job progress, observation freshness, and the last link
execution's action counts and verification. Saved execution statistics remain
fixed when later catalog state changes. Exports read stored data without
executing queries, rules or filesystem verification.

Schema **15** adds immutable saved queries, dependency edges and projection
bindings. Embedded legacy rule and layout selections are adopted as named query
snapshots; original JSON and IDs remain intact. Legacy rule IDs get query IDs
`legacy-rule-ID`. Layout snapshots have `legacy-layout-NAME` IDs and unique
migration-assigned names. Existing layouts continue to work without a binding.

Schema **16** classifies existing recipes as render operations, permits analysis
rules without destinations and file analysis without item identity, and adds
external rule/job provenance and completion references. Table reconstruction
retains existing columns, rows, IDs and foreign keys. The checked migration
runner backs up, rehearses, checks preservation and commits atomically. Opening a
database never upgrades it: preview with `db upgrade --dry-run`, then explicitly
run `db upgrade`.

Old human and `--json` commands remain. Opt-in `--machine` wraps dispatched command
results/errors in `{interface_version:1, command, operation, profile, outcome,
exit_code, data}`. Existing payloads retain their own completeness and recovery
fields. Exit 0 means that command succeeded, 3 means incomplete/unsafe, 2 means
error, and 130 means interruption. Argument-parser errors retain argparse's
stderr behavior. GraphQL exposes saved query/operation/projection definitions;
SQL schema discovery includes their normalized views. Portable interchange
schema versions are unchanged. Programming definitions can be transferred with
the separate configuration bundle described below.

Implemented and fixture-tested: query reuse/composition, analysis and render
rules, explicit derived chains, external queue/receipt convergence, projection
preview/reconciliation, migration preservation and machine envelopes. The new
end-to-end test uses real FFmpeg and disposable local files. Existing installed
wheel, real mount/unmount, Jellyfin and scale acceptance lanes remain separate
validation requirements; their presence is not a claim of a new real-application
run. See [the validation record](PROGRAMMABLE_CATALOG_VALIDATION.md) for executed checks.

The [native presets](ARTIFACTS.md) and [document adapters](ENRICHMENT.md) are
capability-gated and bounded: tone mapping requires tagged HDR and `zscale`, PDF
extracts embedded text, and OCR accepts single PNG/JPEG images. Arbitrary document
formats and scanned-PDF OCR still require external processing. No general
DAG scheduler, query-trigger daemon or automatic recursive rule execution is
introduced. Recorded-state SQL compares immediate lineage; live admission checks
all ancestors. Definition transfer does not transfer catalog records or execution evidence.

## Transfer programming definitions

`program export` writes a version 1 `catabolic.program` JSON bundle. Select one
or more roots with repeated `--query ID`, `--rule ID` and `--projection CATALOG`.
The bundle includes their saved query dependencies, operation/output definitions,
layouts and copy/rendition policies, bounded to 256 definitions, 32 dependency
levels and 4 MiB. This is a separate configuration format; Open Catalog manifests
and their frozen schemas are unchanged.

```sh
catabolic --db source.sqlite3 --json program export \
  --rule RULE_ID --projection mobile > program.json
catabolic --db destination.sqlite3 --machine program import \
  --file program.json --bindings bindings.json --prefix mobile --dry-run
catabolic --db destination.sqlite3 --machine program import \
  --file program.json --bindings bindings.json --prefix mobile
```

For this example, `bindings.json` maps exported resource names to existing local
resources, such as `{"catalogs":{"mobile":"phone"},"locations":{"generated":"derived"}}`.
External operations also require `"processors":{"remote":"local-worker"}`.
Create/bind those resources with their existing commands first. Imports do not
create mount bindings, generated directories, processors or credentials. Export
omits processor endpoints and credential configuration; operation options and
query parameters are included as supplied, so inspect them before sharing.

Import remaps composition/query IDs, operation/output IDs and rendition-policy
rule/output references. Structured selection profiles use the destination
`--profile`. SQL/GraphQL text, parameters, variables, metadata and operation options
remain literal: embedded item/file/recipe IDs, profile names and location predicates
must be reviewed for the destination. Import validates configuration without
executing queries, checking their results or transferring media identities.

Names receive `PREFIX.` (default `imported.`), subject to the usual 64-character
name bound. Saved queries and operations retain immutable revisions. Conflicting
rules/layouts/projections fail atomically; use another prefix or an unconfigured
destination catalog. A bundle must not contain conflicting rule revisions sharing
one name. Each imported projection needs a distinct destination catalog.

Dry-run exercises the same validators inside a rolled-back transaction; returned
new IDs are temporary preview IDs. A failed import preserves all prior definitions
and rule enablement. A successful repeat reuses IDs and preserves existing rule
enablement. Newly imported rules start disabled, and automatic refresh is not
enabled by import. Import creates no jobs, mappings or output files. Inspect the
returned references, run rule/projection previews, then explicitly enable or
execute the selected workflow using the ordinary commands and removal budgets.
