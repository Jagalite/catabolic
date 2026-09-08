# Processing rules and retroactive storage estimates

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Rules connect a saved query or inline SQL/GraphQL selection to an immutable
operation. Analysis records facts, rendering produces files in generated storage,
and external rules queue fenced processor jobs. See [the common rule model](PROGRAMMABLE_CATALOG.md).
The examples below describe render rules over existing media and maintenance cycles.
Preview reports approximately how much **additional space** the full backfill
needs before queuing or encoding. Run `catabolic docs rules` to read this offline.

Commands require schema 16. Upgrade older catalogs explicitly with
`db upgrade --dry-run` followed by `db upgrade`. Rules reuse the existing
[artifact queue, output definitions, validation and recovery](ARTIFACTS.md).

## Create, preview, apply and run

Select your database/profile, scan sources, and collect current probe facts using
`process enqueue probe` and `process run`. Follow enqueue pagination for large
libraries. Probes supply duration, dimensions and streams; missing/stale evidence
is reported as deferred. Then bind a dedicated empty directory and save a recipe:

```sh
catabolic artifact bind generated --root /absolute/path/generated
catabolic --json artifact recipe mobile --preset h264-720p

cat > movies.json <<'JSON'
{
  "language": "sql",
  "query": "SELECT item_id FROM catalog_items WHERE kind='movie'"
}
JSON

# Replace RECIPE_ID and RULE_ID with the IDs returned by these commands.
catabolic --json rule put mobile --recipe RECIPE_ID --location generated \
  --selection movies.json --estimate-video-kbps 2500
catabolic rule preview RULE_ID
catabolic --json rule preview RULE_ID --limit 20
catabolic --json rule apply RULE_ID --batch 100
catabolic --json rule run RULE_ID --batch 1
```

`put` saves a revision, initially disabled for maintenance. `preview` is read-only:
it reads recorded facts and checks filesystem metadata/free space, without probing,
encoding, hashing entire files, or writing the database. `apply` enqueues work;
`run` renders only tracked queued jobs still matching this rule's current selection
and input revisions. Neither drains unrelated rendering work.

Names are labels; execution uses explicit revision IDs. Identical definitions reuse
their revision. Changing a selection, recipe, destination, required flag or estimate
assumption creates a new revision and disables the old revision for maintenance.
Enable the new revision explicitly after review. Old jobs, outputs, requirements
and worklogs are preserved. Recipe changes never silently alter a saved rule.

## Selection and backlog

Use the shared [SQL/GraphQL selection contract](QUERY_FOLDERS.md). SQL returns one
column named `item_id`, `file_id` or `association_id`. Rules select active primary
associations and deduplicate file/item pairs. By default, generated locations and registered/generated renditions are excluded
as render inputs. `--allow-derived` explicitly admits validated rendition inputs
for catalog-driven chains; it does not trigger subsequent rules recursively. A file associated with multiple items can need separate outputs.

For example, place this query in a selection JSON object to target files above 1080p:

```sql
SELECT file_id FROM catalog_facts
WHERE profile=:profile AND operation='probe' AND current=1 AND height>1080
ORDER BY file_id
```

Such a filter excludes files without facts. Use a broad item selection if you want
missing evidence to appear explicitly in the deferred count. Current limits are
10,000 selected IDs by default and 100,000 expanded input pairs. Larger selections
can opt into `page_size` and `max_ids` up to 100,000; see [processors](PROCESSORS.md).
Required-rule records are written only after the entire selection succeeds.
Unknown IDs, timeouts and truncated selections fail before queuing any work.

Matches are `missing`, `stale`, `satisfied`, `queued`, `running`, `failed` or
`deferred`. Missing probes, unavailable sources and known incompatible streams
are deferred. HDR inputs are deferred for the current SDR transcode presets.
Actual rendering still performs all its compatibility checks.

Satisfied outputs have matching input/recipe/destination evidence and current
filesystem metadata. This is not a fresh checksum verification; use the checksum
tools when stronger content verification is needed. Changed inputs or missing/stale
outputs can create new work, preserving previous records/files. Failures and
cancellations require `rule apply RULE_ID --retry-failed`; there is no automatic retry.

## Understanding space estimates

`--limit` bounds match details, not counts or the full retroactive space estimate.
JSON includes exact byte values; terminal output also shows GiB.

| `space` field | Meaning |
| --- | --- |
| `expected_bytes` | Approximate additional storage; null if any output size is unknown |
| `low_bytes`, `high_bytes` | Heuristic range, not guaranteed bounds |
| `known_expected_bytes` | Estimated subtotal for inputs with sufficient facts |
| `unknown_count` | Outputs that cannot yet be estimated; never silently counted as zero |
| `already_satisfied_bytes` | Existing valid outputs excluded from additional storage |
| `available_bytes` | Live free space on the generated destination filesystem |
| `planning_bytes` | Sum of high estimates, using recipe output limits for unknown sizes |
| `configured_output_limits_bytes` | Sum of configured recipe size limits, separate from estimates |
| `reserve_bytes` | Recipe free-space reserve |
| `fits_planning_estimate` | Whether available space covers the full planning allowance and reserve |

Queued, failed and deferred matches still count as outstanding backfill. Originals
and older outputs remain intact, so these are **additional bytes**, not space
savings. Logical size can differ from physical allocation due to filesystem
compression, snapshots and other storage behavior.

Estimator version 1 uses duration × assumed bitrate for video/previews: by default
2,500 kbps video for 720p/previews or 5,000 kbps for 1080p, plus audio and container
overhead. The video range is 0.4–3 times the assumption. Preview duration is limited
to the requested clip length and remaining source duration. Thumbnails use scaled
pixel dimensions and a PNG compression estimate; remuxes use source size plus
overhead; AAC uses duration/bitrate; FLAC uses PCM size/compression; SRT uses a
coarse text-density estimate. Each match explains its method and assumptions.

**`--estimate-video-kbps` changes the estimate only, not encoder settings.** CRF
size varies with content, resolution, frame rate and quality and can fall outside
the range. No sample encoding is performed. Missing duration/dimensions can leave
an estimate unknown. `estimates_exceeding_output_limit` flags estimates whose high
value exceeds a recipe's configured cap; revise the recipe if necessary.

Apply checks the selected batch's planning allowance plus reserve against live
free space. An optional estimated enqueue budget can block the batch before writes:

```sh
catabolic rule apply RULE_ID --batch 10 --max-new-bytes 10737418240
```

This example caps the batch's planning allowance at 10 GiB. Returned `batch_space`
is separate from the full `space` total. It is not an exact limit on encoded bytes.
No space is reserved when queuing; other queued work or external activity can
consume it. Runtime per-output size, free-space reserve and timeout checks remain
independent. Publication renames the temporary output in the same generated
location, so the estimate does not add a second complete copy for publication.

## Maintenance and completion requirements

```sh
catabolic rule enable RULE_ID
catabolic maintenance --all-catalogs --rules --rule-batch 100
catabolic --json maintenance --all-catalogs --rules --render-rules 1
catabolic --json maintenance --inventory-only --rules \
  --rule-max-new-bytes 10737418240
catabolic rule disable RULE_ID
```

Ordinary maintenance never runs rules. `--rules` evaluates enabled rules after
complete scanning and optional analysis, before layout/sync. Rendering requires
the separate `--render-rules N` option. Excluded scan paths are not used to queue
or execute new work from old observations. Current probe facts and source metadata
are rechecked; rule application does not wait for a settle interval.

Enqueue and render budgets are shared across enabled rules in name order. An
earlier rule can consume the batch; use explicit per-rule commands to prioritize
another. Maintenance supports up to 1,000 enabled rules. Space is grouped by
destination device and deduplicated for identical input/item/recipe/destination
requests. Per-rule backlog counts can overlap. Reported space covers the full
outstanding selection before the cycle, including jobs beyond its batch.

Outstanding rule jobs/deferred inputs yield maintenance `complete:false` and exit
3 even if safe output sync finishes. Budget blockers and actual execution failures
stop dependent output work. Per-job timeouts still apply; the batch limit controls
job count rather than an overall wall-clock deadline.

`rule put --required` records semantic rendition requirements when apply completes
its evaluation and storage preflight, including deferred matches and matches
beyond the queue batch. Requirements track current job evidence across explicit
retries, retaining prior attempts in the worklog. Saving or previewing a rule alone
creates no requirements. Requirements survive disablement and revisions; waive
obsolete ones explicitly with a reason. Legacy job requirements remain intact.
See [rendition workflows](RENDITION_WORKFLOWS.md) for completion semantics,
catalog-specific publication and `rule stats` actual usage reports. Rules never
replace source files or mark an entry complete.

## Discovery and automation

```sh
catabolic rule --help
catabolic rule list --enabled
catabolic rule show RULE_ID
catabolic query 'SELECT id,name,revision,enabled,recipe_id FROM catalog_rules WHERE profile=:profile'
catabolic query 'SELECT rule_id,job_id,state FROM catalog_rule_jobs'
catabolic docs rules
```

List pagination uses `next_after` / `--after`. A preview's `complete:true` means
selection evaluation finished, even when it reports deferrals or insufficient
space. Apply completion means the eligible backfill is enqueued; run completion
means all current matches are satisfied. Errors use the normal CLI exit contract.

## Measured estimate calibration

After three distinct successful source renders for the same recipe/profile,
previews and `rule stats` report automatic calibration from recorded actual bytes.
Expected sizes use the median measured ratio; planning budgets never fall below
the original high estimate. Explicit estimate overrides take precedence. See
[calibration details](PROCESSORS.md#calibration-and-larger-rules).
