# On-demand maintenance

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Run `catabolic maintenance` when you want one maintenance cycle and a report of
what remains to be cataloged. It exits when the cycle finishes; it does not
install a background service. Read this guide offline with
`catabolic docs maintenance`.

## Recommended invocation

Select an initialized database and profile with global `--db` and `--profile`,
or `CATABOLIC_DB` and `CATABOLIC_PROFILE`. Configure source locations, output
catalogs and layouts using the [first catalog walkthrough](GETTING_STARTED.md).

```sh
# One output catalog (the default name is global).
catabolic maintenance --catalog plex

# All output catalogs, plus fresh metadata manifests.
catabolic maintenance --all-catalogs --manifest

# Inventory and backlog reporting without generating output folders.
catabolic --json maintenance --inventory-only

# Include a bounded batch of lightweight file analysis.
catabolic --json maintenance --all-catalogs --process sniff --batch 100
```

The cycle scans all registered source locations in the selected profile. Every
source must have an accessible binding. `--catalog` and `--all-catalogs` select
outputs, not sources; `--inventory-only` skips output work entirely.

## What a cycle does

1. Validate the schema, source and selected output bindings, and check for pending
   link recovery or artifact publication. Required upgrades and recovery remain
   explicit commands; maintenance never performs them automatically.
2. Scan sources. Only complete scans publish observations. If any scan is
   incomplete, stop before analysis, layout changes, or link synchronization.
3. Optionally enqueue stable files for the requested analysis type and run at most
   `--batch` matching jobs. Unrelated queued jobs are not run. Separately, `--rules`
   enables saved rule evaluation/queuing; `--render-rules N` explicitly permits
   bounded rendering. See [processing rules and space estimates](RULES.md).
4. Reapply the saved layout already managing each selected catalog. Catalogs with
   manual mappings retain those mappings. Maintenance does not choose a new
   layout; use `layout apply` once to establish a catalog's layout first.
5. Preview the aggregate link plan. Layout mappings are staged in one transaction;
   a layout blocker, source blocker, collision or removal-budget failure rolls
   them all back before any output links change.
6. Commit the desired mappings and synchronize links through the normal journaled
   reconciler. Sync revalidates filesystem operations and verifies the result.
7. With `--manifest`, create or replace each owned manifest after healthy
   verification. Existing manifest ownership protections still apply.
8. Return stage results and aggregate backlog statistics. Maintenance never
   identifies items, accepts proposals, changes entry status or edits worklogs.

The database writer lock is held throughout one cycle. Scans and analysis may
already have committed when a later stage fails. Filesystem synchronization is
journaled and recoverable, not a transaction across all output directories.
An interrupted sync can therefore leave partial progress; inspect its journal
and run the documented recovery command before retrying maintenance.

## Removal protection and scan scope

The default is **zero output removals**. Renaming a layout path can require a
removal, so it can also be blocked. The report includes the removal count,
percentage, actions and blockers. After reviewing the intended change, supply
explicit budgets:

```sh
catabolic maintenance --all-catalogs --max-removals 10 --max-removal-percent 5
```

Budgets cover the combined selected output scope. The existing hardlink
cross-filesystem validation, retained-data handling and final-reference
protections still apply. Maintenance never purges source files, inventory
records, or retained hardlink data. It does not automatically permit an empty
query-layout selection or replace a catalog's layout owner.

Repeat explicit scan exclusions on every invocation:

```sh
catabolic maintenance --catalog plex --exclude .Trashes --exclude .Spotlight-V100
```

Exclusions are exact source-relative paths or subtrees, not globs. Excluded files
keep their prior observations and are excluded from this cycle's analysis work.
See [operations](OPERATIONS.md) for scan and binding semantics.

Maintenance itself has no read-only dry-run mode: scanning records observations.
For read-only inspection, use `layout preview` and `sync --dry-run` separately.
Those commands use recorded inventory, without scanning the sources.

## Reading the report

Global `--json` returns one report on stdout; stage progress goes to stderr.
The report contains `report_version`, database, profile, selected catalogs,
`complete`, `inventory_updated`, `new_files`, `stages`, `errors`, `stopped_at`
and `summary`.

| Summary | Meaning |
| --- | --- |
| `items.by_status` | Counts of pending, in_progress, deferred, ignored, complete and needs_attention entries |
| `items.incomplete` | Pending + in_progress + deferred + needs_attention; ignored entries are excluded |
| `files.uncataloged` | Files with no active item association, including missing/unknown files |
| `files.uncataloged_present` | Uncataloged files recorded present in this profile |
| `files.by_availability` | Recorded present, missing and unknown file counts |
| `files.unmapped_present_by_catalog` | Present files with no active mapping in each selected output; these can already be identified |
| `jobs` | Processing job counts by recorded state, including historical failures |
| `proposals` | Identification proposal counts by state |
| `outputs` | Whether output verification ran, its health, verified link counts, issue counts and retained-data counts |
| `inspect` | Follow-up CLI command fragments for inspecting the backlog |

Item and file counts cover the whole database; file availability and effective
item readiness use the selected profile. Job and proposal counts use that
profile. Output counts cover selected catalogs. Unmapped files may intentionally
be outside a catalog's selection; they are not automatically errors.

`new_files` counts newly inserted inventory occurrences in this invocation, not
new logical media items. Aggregate counts are computed in SQL, independently of
pagination and `--limit` for plan details. Item and file lists still require
following `next_cursor` when inspecting individual records.

**A completed maintenance cycle does not mean curation is finished.** Exit 0 and
`complete:true` mean the requested operational steps finished successfully;
pending/deferred entries, uncataloged files, and unrelated historical job failures
remain visible in the report. Item completion continues to use the normal
[worklog requirements](WORKLOG.md).

Exit 3 with `complete:false` means a blocked/failed stage or unfinished requested
analysis. The report includes statistics even when a cycle stops early. Check
the scan-stage reports for partial inventory updates; summary counts always
describe recorded state. Output health is unknown when verification did not run.
Invalid options or an unreadable/incompatible database return exit 2; statistics
cannot be produced if the database cannot be opened. Interruption returns 130.

## Optional processing and repeats

`--process` accepts `sniff`, `probe`, `hash`, `verify`, `text` or `decode`. Choose
`sniff` for a lightweight pass; other operations may read entire files or require
external tools. Rendering requires `--rules --render-rules N` or an explicit
`rule run`/`artifact run`. Media-server refresh delivery remains separate.

Stable eligible files are enqueued in pages, and at most `--batch` jobs run per
invocation (default 100, maximum 1000). `--workers` defaults to 2, with the normal
per-device concurrency limit. Existing current successful results are reused.
Cached failed jobs are reported and require explicit review/retry through the
processing commands.

The default `--settle 30` waits until a file revision has been recorded unchanged
for 30 seconds across invocations; the cycle does not sleep to make files ready.
The first processing invocation can therefore report files waiting for stability.
For a known quiescent fixture, `--settle 0` permits immediate processing.
Files still settling and jobs beyond the batch produce an incomplete cycle and
remain counted in the processing stage. Safe output work can still finish;
actual analysis failures stop output work.

Run the same command again after resolving blockers or to finish the next batch.
Correct existing links are preserved. For continuous inventory/analysis only,
use `watch`; for the broader human or agent curation cycle, read the
[recommended workflow](WORKFLOW.md) and [automation guide](AUTOMATION.md).
