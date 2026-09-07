# Generated media and processing artifacts

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Schema 8 adds saved outputs to Catabolic's existing processing jobs. Originals
remain read-only. Each completed output is a separate file associated with the
same media item; creating an encode does not create another movie or track.
FFmpeg and ffprobe are optional external executables, never bundled dependencies.

## Initial scope

| Preset | Output | Association role |
| --- | --- | --- |
| `thumbnail` | PNG, at most 640 pixels wide | `thumbnail` |
| `preview` | Short SDR H.264/AAC MP4, at most 720 pixels high | `extra` |
| `remux-mkv` | Matroska with all input streams copied; incompatible streams fail | `primary` |
| `audio-flac` | One selected audio stream encoded as FLAC | `custom:audio` |
| `audio-aac` | One selected audio stream encoded as AAC in M4A | `custom:audio` |
| `subtitle-srt` | One selected subtitle stream converted to SRT | `subtitle` |
| `h264-720p` | SDR H.264/AAC MP4, at most 720 pixels high | `primary` |
| `h264-1080p` | SDR H.264/AAC MP4, at most 1080 pixels high | `primary` |

Video transcodes and previews take the first video and optional first audio track.
They do not preserve every language, subtitle, attachment or HDR property. The
SDR video presets refuse detected PQ/HLG input; tone mapping is not implemented.
SRT extraction is conversion of an existing subtitle stream, not OCR of bitmap
subtitles or speech transcription. Unsupported conversions fail explicitly.
Analysis operations (`probe`, `decode`, `hash`, `verify`, `text`, `sniff`) keep
using `process enqueue` and `process run`; see [Enrichment](ENRICHMENT.md).
Live streaming, HLS/DASH packages, multi-input edits, automatic cleanup and
additional analysis filters are outside this first version.

## Create an output

Upgrade an older catalog explicitly with `db upgrade --dry-run`, then `db upgrade`.
The normal backup and migration rehearsal apply. Examples assume `CATABOLIC_DB`
selects a current initialized catalog and the source has been scanned and identified.

```sh
# Discovery works without opening a catalog.
catabolic --json artifact capabilities

# Choose a new, empty directory outside all registered media/output roots.
mkdir /path/to/generated-media
catabolic artifact bind generated --root /path/to/generated-media

# Returns an immutable recipe ID. Repeating this definition reuses that ID.
catabolic --json artifact recipe mobile --preset h264-720p
catabolic --json artifact recipes

# Use the returned recipe ID and an item actively associated with the input file.
catabolic --json artifact enqueue --file-id INPUT_FILE_ID --item-id ITEM_ID \
  --recipe RECIPE_ID --location generated
catabolic --json artifact run --limit 10
catabolic --json artifact list --file-id INPUT_FILE_ID
catabolic --json artifact show ARTIFACT_ID
catabolic --json process attempts JOB_ID
```

`artifact run` executes one job at a time, using the existing exclusive writer
lock. It does not start a daemon. `process run` handles analysis jobs only and
never claims output jobs. Output jobs remain visible in `process list/show`.
Use Ctrl-C to interrupt the runner; `process cancel JOB_ID` cancels queued jobs.
Resolve pending publication with `artifact recover` before cancel/retry.
A failed terminal job can be retried with `process retry JOB_ID`; the next output
attempt uses fresh filenames and keeps the previous attempt's evidence.

The generated directory is owned by this database/profile/location. Existing
source locations cannot be adopted for writing. Rebinding or relocating generated
locations is not implemented. Root identity and ownership are rechecked before
writing and publication. Normal scans of a generated location inventory only
registered ready outputs; partial files and unrelated files remain outside the
catalog. Files are initially private to the invoking user (mode 0600); grant
access explicitly if a separate service account needs to read them.

## Recipes and budgets

Recipes are immutable revisions. Changing a named recipe creates a new ID and
revision. Jobs record the exact definition, input revision, destination binding
and FFmpeg/ffprobe identity at enqueue time. Changed tools require a new job.

```sh
catabolic --json artifact recipe cover --preset thumbnail \
  --options '{"start_seconds":10,"timeout":60}'
catabolic --json artifact recipe sample --preset preview \
  --options '{"start_seconds":30,"duration_seconds":15}'
catabolic --json artifact recipe english --preset subtitle-srt \
  --options '{"stream":1}'
```

`stream` is the zero-based index among streams of that type, not the absolute
container stream index. It is available for audio/subtitle extraction.
All recipes support integer `timeout` (1..86400 seconds), `max_output_bytes`
(1024 bytes..1 TiB), and `reserve_bytes` (0..1 TiB). Defaults are 3600 seconds,
20 GiB and 64 MiB. Preview duration is 1..60 seconds; start time is 0..86400.
Unknown options and arbitrary FFmpeg argument fragments are rejected.

The timeout applies to the conversion subprocess; input/output probes have their
own 20-second limits. Hashing and blocking filesystem calls are not covered by
that conversion deadline. Free space and output size are monitored; FFmpeg's file
limit is also set. These checks are not disk reservations or strict filesystem
quotas. Reaching a limit fails the attempt, even if FFmpeg exits successfully.

## Database model and publication

- `processing_recipes`: definition, digest, name and immutable revision.
- `generated_locations`: locations explicitly authorized for generated files.
- `processing_artifacts`: producing job/attempt, output location and relative
  paths, item/role, publication state, file ID, checksum and validation evidence.
- `processing_jobs`: existing queue, with a recipe reference for output jobs.
  Its input file ID and snapshot record lineage and the input revision.
- `processing_attempts`: attempt state, errors, start/end timestamps and result.
  Analysis attempts also retain their results; `file_facts` is the latest summary.
- `files`, `observations`, `item_files`: completed outputs join the existing
  catalog model. Output probe facts and initial SHA-256 baselines are registered
  in the same transaction.

Bytes remain on disk. The database stores their identity, location and evidence.
The publication lifecycle is `planned → writing → validating → publishing → ready`.
A new private temporary file is created with exclusive descriptor-based access.
FFmpeg receives that descriptor, never an existing media pathname to overwrite.
Input revision checks, bounded stream probes, operation-specific checks and a
SHA-256 digest precede publication. Validation is not a full output decode; enqueue
`decode` for the generated file when that stronger check is required.

SQLite commits and filesystem publication cannot form one atomic transaction.
Publication intent is committed before an exclusive rename; recovery verifies
identity and checksum before finishing file registration. Destination collisions,
symlinks, root replacement and changed partial files block recovery. No existing
file is overwritten as a fallback.

```sh
catabolic --json artifact recover --limit 100
```

Interrupted work before validated publication is marked interrupted, with partial
bytes retained for inspection. A crash after validation or rename can finish
publication without re-encoding. No automated deletion is provided. Repeating a
completed request reuses its job only after the saved output's identity and full
checksum are verified; missing or modified outputs cause a new job instead.

## Selecting outputs and querying

Generated associations are initially **inactive**, so default library planning
continues to use originals. To select a generated file, explicitly activate its
association, then use an existing copy policy or a saved file selection to avoid
collisions between original and generated primary copies:

```sh
catabolic association put --file OUTPUT_FILE_ID --item ITEM_ID --role primary
```

Use the corresponding role for thumbnails, previews and extracted tracks. Source
metadata is never rewritten and media item identities are not duplicated.

```sh
catabolic query "SELECT id,input_file_id,file_id,recipe_id,state,size,sha256 FROM catalog_artifacts WHERE profile=:profile"
catabolic query "SELECT id,name,revision,preset FROM catalog_recipes"
catabolic graphql '{ artifacts(first:20,state:"ready") { nodes pageInfo { endCursor hasNextPage } } recipes(first:20) { nodes } }'
catabolic graphql 'query($id:ID!){artifact(id:$id)}' --variables '{"id":"ARTIFACT_ID"}'
```

SQL also exposes `catalog_generated_locations` and extended `catalog_job_attempts`.
Recipe definitions are catalog-wide; artifacts and GraphQL results are scoped to
profiles. CLI list/recipes use `next_after` and `--after`; GraphQL uses its normal
cursors. Attempts omit results larger than 8192 characters from list output and
set `result_omitted`; artifact details retain full validation evidence.
Current manifests do not export processing lineage or complete processing history.
