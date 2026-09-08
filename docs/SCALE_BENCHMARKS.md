# Higher-cardinality benchmarks

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

The benchmark now also measures `saved_gap_selection`: a complete reusable SQL
query for missing current renditions, drained in 1,000-ID pages with an explicit
100,000-ID cap. Larger fixtures select an explicit path-bounded subset; a cap is
never treated as implicit completeness. See [the redesign validation](PROGRAMMABLE_CATALOG_VALIDATION.md)
for the current local run.


Run the retained synthetic benchmark from an installed development checkout:

```sh
.venv/bin/python -m tests.scale_benchmark \
  --sizes 10000 100000 1000000 \
  --filesystem-sizes 10000 100000 \
  --root .local-tests/my-new-scale-run
```

The destination must be new. All fixtures and reports are retained there. An
existing fixture directory is never reused by the controller. The root-level
`report.json` is updated after each measurement. No production catalog or media
path is involved. This benchmark does not change runtime indexes, execution caps,
SQLite durability settings, or application algorithms.

## What the fixtures represent

Database-only fixtures contain the requested number of files, identifications,
mappings, and outgoing custom relationships, plus one collection per 100 media
items. There is one media item per file, mixed among movies, book editions,
tracks, photos, and documents. Metadata includes a title, year, language, tags,
and description. Each item has a synthetic external identity. For example,
`--sizes 1000000` means one million file rows and 1.01 million total item rows.
These fixtures record synthetic observations without creating physical media;
they cannot establish filesystem correctness or NAS performance.

Filesystem fixtures create actual small synthetic files, scan them through the
application, and then populate item decisions using fixture-only SQL batches.
Real application synchronization creates the links. The benchmark verifies an
unchanged repeat, retires 1% of mappings, checks that surviving links preserve
inodes/mtimes, and injects a process exit after link creation but before ownership
commit. A separate process recovers that journal and verifies the result. Source
content and inode fingerprints must remain unchanged. The recovered extra test
link is retained, so the final active count is the surviving 99% plus one.

SQL batching is a fixture-generation technique, not a claim about public import
throughput. A separate single-mapping measurement exercises `Application.put_mapping`
and removes only its own newly generated test mapping afterward. Bulk import
through the CLI is not measured here.

## Workloads and measurements

| Workload | Scope |
| --- | --- |
| Indexed association | One file's identification, including Store open/validation |
| Metadata SQL | Count every item matching a year range through the SQL query API |
| Title search | One exact title-shaped substring through the catalog query API |
| Nested GraphQL | First 100 movies and their file associations |
| Filtered GraphQL | First 100 items matching a year; stops when that page is filled |
| Select-one layout | One ID selected through SQL, planned within the entire fixture |
| Full layout | Plan every active association, subject to the existing runtime cap |
| Manifest | Build and serialize the complete global manifest, subject to existing caps |
| Manual mapping | One new mapping among existing mappings, then fixture-only removal |
| Initial sync | Real link creation, journaling, source checks, and final verification |
| Rescan | Full source rescan with no source changes |
| Repeat sync | Unchanged reconciliation and verification, with inode checks |
| 1% change | Reconcile disabled mappings, preserving the surviving 99% |
| Crash/recovery | Real process interruption at the filesystem/database boundary |

Each operation runs in a fresh Python process. Short database reads use three
trials by default; report the median along with the individual samples. These
are not enough samples for meaningful tail-latency claims. `seconds` includes
database open/validation inside the worker; `wall_seconds` also includes process
startup. Filesystem repeat/change results expose `sync_seconds` separately from
the surrounding fingerprint checks. Initial scan time is inside the seed result.

Peak RSS is measured per worker using `getrusage`, in MiB. The controller applies
a 90-second timeout to database operations and 600 seconds to fixture generation
and filesystem phases. A worker thread checks a configurable peak-RSS ceiling
(default 1536 MiB) every 200 ms; exceeding it exits that worker. This is a
best-effort test resource ceiling, separate from application limits. Database
seeding also stops if free disk space falls below 5 GiB.

Results distinguish `ok`, application `guarded` rejection, test `memory_limit`,
controller `timeout`, intentional `injected_crash`, and unexpected `failed`.
An application rejection is not counted as successful operation capacity.
Only an unexpected failure makes the controller exit nonzero; inspect the report
for limits and timeouts even when its exit code is zero.

OS filesystem caches are shared and are not flushed. These are local synthetic
measurements, not cold-cache, concurrent-client, physical power-loss, or NAS
benchmarks. Small fake files measure metadata operations, not media decoding or
large-file hashing throughput.

## SQL-first layout experiment

After generating the fixtures above, measure the production planner and compare
its complete output with the retained pre-optimization report:

```sh
.venv/bin/python -m tests.layout_strategy_benchmark \
  --production \
  --reference .local-tests/layout-strategies-20260906/report.json \
  --fixtures .local-tests/high-scale-20260906 \
  --root .local-tests/my-new-layout-comparison
```

The comparison opens retained database fixtures read-only and uses an in-memory
layout definition. It measures one item and 10,000 items selected from the million
file catalog, plus a full 100,000-item layout. Complete plan and mapping hashes
must match the reference report; a separate collision case must produce the same
blockers. Production measurements run three times per workload. The normal
application suite separately covers arbitrary selectors, ownership, and apply.

To reproduce the historical prototype experiment, omit `--production` and
`--reference` and supply `--baseline-file PATH_TO_PRE_OPTIMIZATION_LAYOUTS_PY`.
The retained local baseline is
`.local-tests/layout-production-baseline/layouts.py`. This mode compiles a copy
of that baseline with only its graph loading block replaced and selection/cap
checks moved earlier. Its candidates use batches of 1, 100, 1,000, and 10,000,
specialized to this fixture's one-hop outgoing collection relationship.

In historical mode, the baseline runs once and candidates three times in fresh
processes. The report contains individual samples, peak RSS before signature
serialization, and graph-loading SQL call counts for prototype candidates.
Neither mode measures applying/syncing the result. Selection and planned mappings
still occupy memory proportional to the selected scope. Each projection catalog
starts empty, so populated-output reconciliation is outside the measurement.
OS caches are shared, so these are exploratory estimates rather than controlled
cold-cache or tail-latency claims. Source hashing records the planner compared.

## Enrichment and staged scans (2026-09-06)

`python -m tests.enrichment_benchmark --root NEW_DIRECTORY` compares the committed
application/scanner at `7420b3566da519f0036bab8cb00fa4317d85edf6` with the staged
scanner, using disposable real files. It also probes 50 short generated WAV files
and checks that an unchanged repeat launches no jobs. The full baseline application
and filesystem modules are loaded from the current `HEAD`; record the reported
commit when reproducing this after a commit changes that baseline.

An exploratory local APFS run produced these traced Python allocation peaks:

| Source files | Committed scanner | Staged scanner |
| --- | ---: | ---: |
| 1,000 | 0.349 MiB | 0.086 MiB |
| 10,000 | 3.393 MiB | 0.087 MiB |
| 100,000 | 34.030 MiB | 0.102 MiB |

These are `tracemalloc` peaks, not total process RSS or SQLite/extractor memory.
The staged scanner still requires temporary disk space proportional to observations.

The 50-file probe run took 2.03 seconds, plus 0.28 seconds to enqueue. An unchanged
enqueue/run took 0.018 seconds, reused all 50 jobs, and launched zero probes. These
small WAV fixtures establish cache behavior, not video, NAS or decoding throughput.

Scan timings were variable: the traced 100,000-file sample was 16.03 seconds for
the baseline and 32.56 seconds staged. Additional alternating runs without
`tracemalloc` measured 24.46/69.29 seconds baseline and 17.77/9.55 seconds staged,
with substantial variation in database publication time. This evidence supports
bounded Python memory, but not a reliable scan throughput speedup or regression.
No durability settings on the catalog database were weakened for these results.

Retained local reports from this session are
`/private/tmp/catabolic-enrichment-scale-baseline-20260906/report.json` and
`/private/tmp/catabolic-enrichment-scale-baseline-20260906/timing-components.json`.
These are exploratory integration measurements, not consumer compatibility claims.
