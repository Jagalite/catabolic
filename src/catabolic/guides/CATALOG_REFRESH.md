# Automatic catalog link updates

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Schema 14 connects rendition completion to saved catalog layouts through a durable
refresh queue. Once enabled for a catalog, a completed local render or validated
external receipt refreshes its links automatically at the end of the producer
batch. Probe/hash/verification completion for registered renditions also queues a
refresh, so external video outputs can become eligible after probing.

This updates catalog mappings and filesystem links only. It does not call Plex,
Jellyfin or other consumers, queue their library-scan notifications, write
manifests, scan source trees, or start another encode.

## Enable a catalog

First save and apply a layout and choose the catalog's rendition policy. For a
library containing only smaller encodes, select the exact output definition or
rule revision that produces them. The broad `purpose:transcode` policy admits all
ready transcodes, which need not all be smaller.

```sh
catabolic rendition policy --catalog mobile --definition '{"definition_id":"SMALLER_OUTPUT_DEFINITION_ID"}'
catabolic layout put mobile --preset plex
catabolic layout apply mobile --catalog mobile
catabolic catalog-refresh enable --catalog mobile
```

The catalog must already be bound to its output directory. An applied empty layout
is supported: before any smaller outputs exist, the library can contain no titles.
When an output becomes ready, the saved layout determines its readable filename
and the normal reconciler creates and verifies its link. Originals stay untouched.
Other catalogs keep their own settings. Enablement is per profile and catalog;
upgrading a database does not enable any automatic work.

Enabling queues an initial refresh and attempts it immediately. Automatic removal
budgets default to zero. If a preferred-copy policy needs to replace previous
versions, explicitly configure the allowed removals:

```sh
catabolic catalog-refresh enable --catalog mobile --max-removals 10
catabolic catalog-refresh disable --catalog mobile
```

Disabling stops future automatic refreshes and clears pending work for that
profile/catalog; it does not remove existing mappings or links. Layout collisions,
missing bindings, source changes and removal budgets remain enforced. An empty
layout that would remove all generated mappings still requires a separate,
explicit `layout apply --allow-empty`; automatic refresh never approves that.

## Completion and retries

`artifact run`, `rule run`, `artifact recover`, `rendition import-receipt` and
successful processor receipt completion attempt pending refreshes after committed
results. `process run` also drains pending refreshes after its batch. A render
remains complete even when link publication is pending: the JSON result includes
`catalog_refresh` separately and human-readable rule output reports pending
publication. The event persists until link synchronization and verification succeed.

Standalone completion attempts immediate updates. For recovery after a crash,
offline destinations, or work beyond the per-cycle limit, run the retry worker:

```sh
catabolic catalog-refresh watch --interval 5
```

Run this foreground process under your usual supervisor for unattended operation.
It opens the database only while checking/processing work and releases all locks
while idle. It never scans the media library. Multiple workers serialize through
the existing local database writer lock; a busy worker retries later. The interval
is 1–60 seconds. Retry delays increase from five seconds up to one hour; new
rendition evidence makes an affected catalog eligible for an immediate retry.
Restarting the worker preserves pending work and retry timing. Stop it with Ctrl-C.

Inspect or retry explicitly:

```sh
catabolic catalog-refresh pending --limit 100
catabolic catalog-refresh run --catalog mobile
catabolic catalog-refresh run --catalog mobile --force
```

`--force` bypasses the retry delay only; it does not override ownership, readiness,
collision or removal checks. Runs process at most 100 catalogs by default, up to
1,000 with `--limit`. Pending and run reports contain errors and remaining counts.
Run returns incomplete while matching work remains queued. Pending is read-only.

Existing `maintenance` retains its own configured catalog scope, stage ordering
and removal budgets. Its render/analysis stages enqueue events but defer this
separate automatic drain; maintenance already plans and synchronizes its selected
catalogs. A later worker cycle can verify and acknowledge the coalesced event.
Normal explicitly requested sync/maintenance retains its pre-existing consumer
notification behavior; the automatic catalog-refresh path suppresses it entirely.

## Durability and boundaries

Completion records and refresh intent commit in the same SQLite transaction.
Multiple outputs coalesce into one pending row per enabled catalog/profile. The
worker reevaluates those catalogs' saved policies and queries against current
state, so custom selections do not need a second dependency-expression system.
Only opted-in catalogs are considered; files excluded by their policies create no
links. A receipt lacking a required probe can be acknowledged with no link; the
later probe completion creates a new event.

Layout changes and filesystem preflight share one transaction. A blocked preflight
rolls mapping changes back. Filesystem updates use the existing operation journal;
a restarted worker recovers interrupted operations for its selected catalog,
replans, syncs and verifies. An event is removed only for the generation that
finished, preserving newer changes. Pending journals in other catalogs still block
unsafe desired-state changes until those operations are recovered.

This is a durable event queue with immediate producer-driven draining and an
optional persistent retry worker. It does not install an operating-system service
or modify any live catalog configuration by itself.
