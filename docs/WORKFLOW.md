# Recommended workflow

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Use this workflow for routine cataloging, whether you run the commands yourself
or through an agent. Read it offline with `catabolic docs workflow`.

For your first run, follow the [disposable walkthrough](GETTING_STARTED.md) to
initialize a database, bind source locations and an output catalog, and save a
layout. The cycle below assumes those are configured. Select your database and
profile explicitly, or set `CATABOLIC_DB` and `CATABOLIC_PROFILE`. Replace
`ITEM_ID`, `LAYOUT`, and `CATALOG` with your actual IDs and names.

## 1. Check state and scan sources

```sh
catabolic db status
catabolic status
catabolic scan
```

Handle any reported migration or interrupted operation before continuing; see
[database migrations](MIGRATIONS.md) and [recovery](OPERATIONS.md).
`scan` inventories the configured sources into the same database. Require a
complete scan before interpreting missing files, and repeat any exclusions from
your intended scan scope on every run. Queries read recorded observations;
they do not discover new files by themselves.

## 2. Review new and unfinished work

```sh
catabolic files --unidentified
catabolic item list --curation-status pending
catabolic item list --curation-status in_progress
catabolic item list --curation-status needs_attention
```

Follow `next_cursor` with `--cursor` to read further pages using the same filters.
Unidentified files include older unresolved files as well as newly discovered
ones. Revisit deferred entries when their missing evidence becomes available.

Identify items, review metadata and editions, and create their file associations,
relationships and tags. People or agents make these decisions. Proposals allow
explicit review before acceptance; direct item and association commands are also
appropriate when that review has already happened.

## 3. Record decisions and finish required work

```sh
catabolic item status ITEM_ID --set in_progress --note 'Reviewing metadata and files'
catabolic item note ITEM_ID --kind decision --text 'Retain both editions'
catabolic item requirements ITEM_ID
```

Enqueue bounded sniffing, probing or hashing when it answers a concrete question.
For custom renditions, register an existing output or use a saved recipe; inspect
the resulting artifact and choose its association deliberately. Processing is
optional for cataloging. See [enrichment](ENRICHMENT.md) and
[artifacts](ARTIFACTS.md) for the commands.

Use `item require` to make a particular file, job, artifact, accepted proposal or
manual review a completion requirement. Only explicitly required processing
blocks completion. Record unresolved questions in the journal and set the entry
to `deferred` when appropriate. See [entry worklogs](WORKLOG.md).

## 4. Preview, publish and verify output folders

Skip this step if you only need the database catalog. For each output catalog:

```sh
catabolic layout preview LAYOUT --catalog CATALOG
catabolic layout apply LAYOUT --catalog CATALOG
catabolic sync --catalog CATALOG --dry-run
catabolic sync --catalog CATALOG
catabolic verify --catalog CATALOG
```

Review each result before continuing. Layout application saves desired mappings;
sync changes the output links. Resolve blockers and unexpected removals before
running sync. If you set removal budgets, pass the same budgets to the dry run
and the actual sync. Verify checks the resulting links.

Default layouts do not filter by entry completion status. Use an explicit
[query selection](QUERY_FOLDERS.md) if you want publication limited to particular
statuses. A configured media-server refresh is a separate optional operation.

## 5. Mark reviewed entries complete and export metadata

```sh
catabolic item status ITEM_ID
catabolic item status ITEM_ID --set complete --note 'Review and required work finished'
```

Completion checks the title, recorded availability of active primary files in
the selected profile, and explicit requirements. It does not run a new scan,
probe or output verification. To make publication verification a completion
requirement, add a manual review requirement and resolve it after checking the
verification result. Later invalid evidence can make a completed entry appear as
`needs_attention` without erasing its history.

After updating statuses, optionally refresh the output manifest so it includes
the latest entry summaries:

```sh
catabolic manifest --catalog CATALOG --in-catalog
```

A manifest is an interchange snapshot, not a full database backup. Keep database
backups for the complete journal and operational history.

## Repeat or monitor

For repeatable thumbnail, preview-clip or transcode generation, configure a
[processing rule](RULES.md). `rule preview` estimates the full retroactive storage
cost before queuing; maintenance evaluates enabled rules only with `--rules`,
and rendering needs the additional `--render-rules N` option.

Repeat the cycle when source files or catalog decisions change. For continuous
discovery and lightweight sniffing, run:

```sh
catabolic watch --kind sniff --interval 30 --settle 30 --cycles 0
```

This foreground process repeatedly scans and processes stable files, sleeping
30 seconds between completed cycles. It does not identify items, accept proposals,
publish links or mark entries complete. Stop it with Ctrl-C. For scoped scans and
watch behavior, read [operations](OPERATIONS.md) and [enrichment](ENRICHMENT.md).

For an on-demand cycle of scanning, optional analysis, configured layout updates,
link synchronization and backlog reporting, run `catabolic maintenance --all-catalogs`.
It defaults to zero output removals. Read `catabolic docs maintenance` or the
[maintenance guide](MAINTENANCE.md) for budgets, scope and failure behavior.
Identification decisions and completing entries remain explicit curation steps.

For scheduled jobs and agents, use [the automation guide](AUTOMATION.md): explicit
database/profile selection, global `--json`, complete pagination, exit-code
checks, and deliberate handling of interrupted writes.
