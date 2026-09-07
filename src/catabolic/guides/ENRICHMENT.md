# Media enrichment, curation and processing

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Schema 6 adds optional processing and curation workflows. Inventory and queries
remain usable without FFmpeg, credentials, a daemon, or source metadata writes.
Current commands require schema 10. Older databases require `db upgrade --dry-run` followed by `db upgrade`.
The existing backup, rehearsal, preservation and recovery requirements apply.

Saved media outputs use the separate [artifact workflow](ARTIFACTS.md), with owned
generated locations and recoverable publication. The processing commands below
analyze source files without changing them.

## Processing from the CLI

Processing has two steps: enqueue bounded work, then run it. Examples assume
`CATABOLIC_DB` selects an initialized catalog with registered, scanned sources.

```sh
catabolic --json process enqueue sniff --location media --limit 1000
catabolic --json process enqueue probe --file-id FILE_ID
catabolic --json process run --workers 2 --per-device 1 --limit 1000
catabolic --json process list --state failed
catabolic --json process show JOB_ID
catabolic --json process facts FILE_ID
```

Enqueue/list responses return `next_after` when more records exist. Pass it as
`--after` with the same operation and source scope. Explicit `--file-id` can be
repeated up to 1000 times. All commands support the global `--json` flag. Lists
are bounded summaries; `process show` retrieves a full result when `result_omitted`
is true. A run's `complete` reports outcomes processed by that invocation, not
whole-library health. `remaining` reports queued jobs. Exit 3 indicates an
incomplete operation; exit 2 indicates an input/operation error.

| Operation | Work and dependencies |
| --- | --- |
| `sniff` | Read at most 4096 bytes for common signatures and retain an extension hint; standard library only. An unknown MIME is explicit. |
| `probe` | Optional `ffprobe`: bounded container/stream analysis, audio tags, stream languages, chapters, dimensions and color-transfer evidence. Supports common self-contained video, audio and image formats. |
| `hash` | Full SHA-256, streamed in 1 MiB chunks; creates a baseline only if none exists. |
| `verify` | Recompute SHA-256 and compare against a preserved baseline. Requires a prior hash. |
| `decode` | Optional `ffmpeg`: full audio/video decoding to a null output; explicitly more expensive than probing. |
| `text` | Bounded UTF-8 TXT/Markdown lines or SRT/WebVTT cues with line/time locators. |

A probe is complete for its configured analysis scope; it is not proof that every
possible field exists or that the media decodes fully. HDR is unknown when the
necessary stream evidence is absent. Raw structured probe data is retained beside
normalized summary fields. This first adapter does not promise complete EXIF,
book metadata, OCR, or transcription. Automatic playlist processing is outside
the probe's supported formats.

Configure work budgets explicitly:

```sh
catabolic process enqueue probe --file-id FILE_ID \
  --options '{"timeout":20,"analysis_bytes":1048576,"analysis_us":1000000,"max_bytes":2097152}'
catabolic process enqueue hash --file-id FILE_ID --options '{"timeout":3600}'
catabolic process run --workers 4 --per-device 1 \
  --storage-groups '{"seed1":"nas","seed2":"nas"}'
```

Defaults: two workers globally, one per recorded storage device, a 20-second
extractor timeout, 120 seconds for hashing/verification/decoding, and 2 MiB captured
output. The runner supports at most 16 workers. NAS groups combine registered
source names that share storage. These limits constrain concurrent work, output
and analysis; FFmpeg analysis-byte settings are not a universal total-I/O limit.
Hash deadlines are checked between reads. Blocking kernel filesystem calls can
outlast a cooperative deadline. Subprocesses are isolated in process groups and
terminated on timeout, excessive output, or cancellation; process memory remains
a measured platform property, not a promised hard RSS ceiling.

Eligibility includes profile, source binding, occurrence revision, extractor
identity/version and options. Unchanged eligible jobs are reused, including
recorded failures; use `--refresh` for a new attempt or `process retry JOB_ID` for a
failed/timed-out/cancelled job. `verify` intentionally rereads content on each new
enqueue. A changed file requires a scan and a new job. Cache reuse is not an
integrity guarantee. `current` on facts compares recorded inventory, without
contacting the source or checking a newer tool version.

`process cancel JOB_ID` cancels unstarted or abandoned work. A running CLI holds
the existing exclusive writer lock; interrupt that runner to stop active work,
then cancel its abandoned jobs before resuming. Jobs claimed by a crashed runner
are retried on the next `process run`. Successful prior results remain available
when later attempts fail. Inspect jobs as well as facts to see those failures.
Small successful results can be reused across same-inode references within a run;
file occurrences and checksum baselines stay separate.

## Query selections and stored results

`process enqueue --selection selection.json` accepts the existing saved-selection
contract, with a SQL or paginated GraphQL query returning file IDs. Selections are
bounded to 10000 IDs and must complete successfully before any jobs are enqueued.
The selection profile should match the processing profile.

```json
{
  "language": "sql",
  "profile": "default",
  "query": "SELECT file_id FROM catalog_files WHERE profile=:profile AND status='present' AND source_relative_path LIKE '%.mkv'"
}
```

```sh
catabolic process enqueue probe --selection selection.json
catabolic query "SELECT file_id,height,width,hdr FROM catalog_facts WHERE profile=:profile AND operation='probe' AND current=1"
catabolic graphql '{ files(first:10) { nodes { id facts } } jobs(first:10,state:"failed") { nodes pageInfo { endCursor hasNextPage } } }'
catabolic graphql 'query($id:ID!){job(id:$id)}' --variables '{"id":"JOB_ID"}'
```

New SQL views: `catalog_jobs`, `catalog_facts`, `catalog_checksums`,
`catalog_proposals`, `catalog_decisions`, `catalog_expected`, `catalog_text`, and
`catalog_refresh`. Use `query --schema` for current columns. SQL views contain all
profiles: filter `profile=:profile` where applicable. GraphQL `jobs` and
`proposals` use context-bound cursors and summary JSON nodes; `job(id:)` and
`proposal(id:)` return individual full records. Queries never launch extractors.

## Checksums and content search

```sh
catabolic process enqueue hash --location media --limit 1000
catabolic process run --limit 1000
catabolic content duplicates
catabolic process enqueue verify --file-id FILE_ID
catabolic process run
catabolic process enqueue text --file-id SUBTITLE_FILE_ID
catabolic process run
catabolic content search 'blue planet'
```

Duplicate groups are recorded SHA-256 baselines, with separate occurrence counts.
Use `catalog_checksums` joined to `catalog_files` to list their members. A mismatch
records the newly observed digest and expected digest in the job result and leaves
the baseline intact. A known mismatch also invalidates prior facts for that
occurrence, including when size and modification time were preserved.
There is no automatic duplicate deletion, baseline reset,
or occurrence merging. Source relocation can be curated by associating the new
occurrence with the same item and replanning output; existing history remains.

Text search uses indexed normalized words with AND semantics, independent of an
optional SQLite FTS extension. Results include their original locator, job ID,
and recorded `current` status. Refreshing extracted text replaces derived segments
and terms; job history retains the previous extraction. Stale hits remain visibly
marked until refreshed. GraphQL also provides `contentSearch(text:,first:,after:)`.

## Identification proposals and history

Create a payload file, then submit it for review:

```json
{
  "item": {
    "kind": "movie",
    "identities": {"tmdb.movie": "438631"},
    "metadata": {"title": "Dune", "year": 2021}
  },
  "role": "primary"
}
```

```sh
catabolic proposal put --file-id FILE_ID --file proposal.json \
  --source agent:curator --evidence '{"reason":"Reviewed title and release year"}'
catabolic proposal list --state pending
catabolic proposal show PROPOSAL_ID
catabolic proposal accept PROPOSAL_ID --actor agent:curator
catabolic proposal reject PROPOSAL_ID --actor manual
catabolic proposal history
```

To associate an existing item, use `{"item_id":"ITEM_ID","role":"subtitle"}`
with optional association `metadata` and `part`. Acceptance creates/updates the
item and association in one transaction. It does not create output mappings or
run synchronization. Repeating an identical proposal or decision is idempotent.
Conflicting identities, changed snapshots, and unavailable live sources block
acceptance. A rejected proposal stays rejected; submit a new proposal when the
input or decision changes.

Existing curated fields win over candidate values. Explicit `overwrite_fields`
in the payload names fields the acceptance should replace. The result reports
preserved conflicts. The decision event records the actor and before/after state;
association `origin` remains compatible with the existing model. This is proposal
decision history, not a complete audit of every legacy CLI edit, and there is no
unconditional undo of filesystem changes.

Optional TMDB movie search uses `TMDB_TOKEN` (API read-access bearer token):

```sh
catabolic identify --file-id FILE_ID --query 'Dune' --year 2021
catabolic identify --file-id FILE_ID --query 'Dune' --year 2021 --apply
```

Default output is candidates only; `--apply` saves proposals without accepting
them. Search retrieves the first page of up to 20 candidates and reports whether
more pages exist. Title-word overlap and year evidence explain ranking; they are
not probabilities or automatic authorization. Credentials remain in the named
environment variable. Provider failures preserve existing curation. This product
uses the TMDB API but is not endorsed or certified by TMDB.

## Sidecars, preferred copies and completeness

```sh
catabolic sidecar --location media --limit 1000
catabolic sidecar --location media --limit 1000 --apply
```

Discovery matches same-directory primary-file stems and conventional artwork
names. It saves optional association proposals, not guesses applied directly.
Subtitle language and forced/SDH flags are proposed from names. Ambiguous item
matches remain separate candidates. Explicit associations remain available for
multipart and unconventional naming. Pagination is over inventory files examined,
not the number of candidate sidecars returned.

A copy-policy file can contain:

```json
{
  "require": {"language":"eng"},
  "prefer": [{"field":"height","order":"desc"}],
  "tie_break": "file_id",
  "available_only": true
}
```

```sh
catabolic copies put --catalog plex --file copies.json
catabolic copies plan --catalog plex
catabolic layout preview my-layout --catalog plex
catabolic layout apply my-layout --catalog plex
catabolic sync --catalog plex --dry-run
```

The policy is evaluated after any saved SQL/GraphQL selection and before naming.
Candidates are grouped by item and part; distinct editions remain distinct items.
Discovered sidecars retain `primary_file_ids`; copy selection excludes those
attached only to an unselected primary. Explicit unscoped sidecars are retained.
Required unknown fields fail eligibility. Preferences are evaluated in order;
use `values` instead of `order` for an ordered list of text preferences. Ties block
unless an explicit file-ID tie-break is configured. `available_only` is an explicit
fallback choice based on recorded availability; live reconciliation safeguards
still apply. Layouts can reference normalized values such as `{probe.height}`.
Missing required probe fields block path generation. Copy policies apply to layout
generation; they do not rewrite explicit manual mappings.

An expected-members file names its source, edition, ordering, completeness, and
member identities:

```json
{
  "source":"manual:collection-check",
  "edition":"original",
  "ordering":"season-episode",
  "complete":true,
  "members":[
    {"key":"S01E01","item_id":"EPISODE_ITEM_ID"},
    {"key":"S01E02","identity":{"tvdb.episode":"12345"}}
  ]
}
```

```sh
catabolic expected put --collection COLLECTION_ITEM_ID --file expected.json
catabolic expected report EXPECTED_SET_ID
```

Each definition creates or reuses an immutable expected-set ID. Reports compare
that exact set, never an implicit latest provider response. Members can be present,
unavailable, unidentified, missing, or unknown. `expectations_complete=false`
means the supplied list cannot establish whole-collection completeness. Different
ordering schemes or editions should have separate sets. Several items associated
with one multi-episode file are counted independently through those associations.

## Watching and media-server refresh

```sh
catabolic watch --kind probe --location media --settle 30 --interval 30 --cycles 1
catabolic watch --kind sniff --cycles 0
```

Watching is periodic full reconciliation of inventory, not an OS event listener.
It preserves stability timestamps across restarts, enqueues stable eligible files,
and processes at most `--batch` jobs per cycle. It releases the writer lock between
cycles. Progress is JSON on stderr; a finite run returns a summary on stdout.
Watching never accepts proposals or synchronizes outputs. Tune the polling interval
for NAS traversal costs; filesystem events can become an optimization later.

The first media-server refresh adapter is Jellyfin:

```sh
catabolic refresh configure --catalog jellyfin --endpoint http://localhost:8096 \
  --credential-env JELLYFIN_TOKEN
catabolic sync --catalog jellyfin
catabolic refresh list
catabolic refresh run
catabolic refresh retry EVENT_ID
```

Configuration is scoped to the machine profile and catalog and stores only an
environment-variable reference for credentials. Link ownership
publication records a durable dirty marker; successful verified sync queues a
refresh event. Recovery preserves the marker. Unchanged subsequent syncs do not
queue duplicate events. `refresh run` sends the HTTP request explicitly, independently
of link operations, with up to three attempts before explicit retry. Delivery is
at least once: a crash after the remote server accepts a request may cause a repeat.
Requests have a 15-second overall deadline, including worker startup, DNS, TLS,
headers and response consumption. A supervised private process makes stalled
network calls cancellable; credentials travel over stdin, not command arguments.
The response byte cap remains in force. Redirects are refused and errors omit
credential values. The adapter is tested
against the protocol contract; live application/version certification is separate.

## Storage and scale boundaries

The scanner streams directory entries into disposable SQLite staging with 500-row
buffers and a small page cache. It still publishes a complete source generation
atomically. Traversal errors are capped at 1000 detailed messages plus an omitted
count; an incomplete scan preserves prior inventory. Deep directory traversal
still has the documented nesting/descriptor limits.

The job runner holds the existing single-writer lock for the invocation; other
readers can query between short result transactions. It does not introduce multiple
SQLite writers or a distributed scheduler. Probe costs depend on actual file formats,
storage and cache state; synthetic metadata benchmarks do not measure media decoding.

Schema 6 records are available through the CLI and query APIs. Frozen manifest
versions 1–3 remain unchanged and do not export job histories, proposals or these
new fact tables. The verified SQLite backup preserves all database records. A
future portable enrichment contract can be versioned independently after use has
established which result fields should be standardized.

## Transient retry policy (schema 7)

```sh
catabolic process run --retry-transient 2 --retry-delay 30
catabolic process attempts JOB_ID --limit 100
catabolic query "SELECT * FROM catalog_job_attempts WHERE profile=:profile"
catabolic watch --cycles 0 --retry-transient 2 --retry-delay 30
```

Automatic retry is opt-in: `--retry-transient 0` is the default. A positive value
allows up to that many additional attempts after the first attempt, based on the
job's durable attempt counter. The maximum is 10. Only extractor timeouts and
classified temporary OS failures (such as EIO, ESTALE, or connection interruption)
are eligible. Unsupported input, invalid media/parser output, permission denial,
missing paths, changed revisions, integrity mismatches, cancellation and excessive
output are not automatically retried. Subprocess nonzero exit alone is not proof
of a transient error.

A failed attempt records its reason and retry time. Delay doubles from
`--retry-delay` (default 30 seconds) up to one hour. A subsequent invocation of
`process run` or watch claims eligible retries once, after their deadline, subject
to its retry cap and run limit. The runner never sleeps while holding the writer
lock to wait for a retry. An exhausted job stays failed and queryable. Manual
`process retry` does not reset its attempt counter; use explicit refresh to start
a new job after investigation. Old schema-6 failures have no inferred eligibility.
Source and tool revisions are revalidated before any retry result is accepted.
Attempt history preserves states/errors, while jobs retain their latest result.

## Bulk output-removal limits

```sh
catabolic sync --all-catalogs --dry-run --max-removals 10 --max-removal-percent 5
catabolic sync --all-catalogs --max-removals 10 --max-removal-percent 5
```

Both limits are optional and apply to the whole selected scope, before journal
intent or any output mutation. Exceeding either blocks the entire plan. Exact
limits are allowed; zero blocks every removal. JSON previews include
`removal_budget` with the count, denominator and percentage. The denominator is
currently owned output entries across the selected catalogs; newly created links
do not dilute it. Hardlink retirement counts as a removal even though its data
is retained. Replacements and forgetting already absent paths are separate actions
and do not count. These limits do not authorize source deletion or override final
hardlink-reference protection. Recovery completes previously recorded intent;
limits apply to fresh synchronization plans, not to replaying that intent.

For scheduled jobs, pass explicit limits appropriate to the catalog on every
invocation. See `catabolic docs testing` for real media and storage acceptance.
