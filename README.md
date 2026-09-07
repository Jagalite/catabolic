# Catabolic

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

**Catalog your media. Make its metadata useful everywhere.**

Your movies might live on one drive, your music on another, and your books on a
NAS. Catabolic brings them into one searchable catalog with titles, tags,
identities, and relationships. Keep track of what you have, enrich its metadata,
and build collections across your storage locations.

When you're ready to use that collection elsewhere, generate **symlink libraries
in popular media-app folder formats**. Give Plex, Jellyfin, music servers, or
readers their own organized view without copying, moving, or renaming your
original files.

Use it yourself or let an AI agent help curate the collection. You decide what
each file is and how it should be described and organized; Catabolic maintains
the catalog and generates the outputs you choose.

[Get started](https://github.com/Jagalite/catabolic/blob/main/docs/GETTING_STARTED.md) · [Documentation](https://github.com/Jagalite/catabolic/wiki) · [FAQ](https://github.com/Jagalite/catabolic/blob/main/docs/FAQ.md)

## What can you do with it?

- **Bring your collection together.** Catalog media across several drives and
  mounted storage locations, and search the recorded catalog while they're offline.
- **Organize it your way.** Add titles, identities, tags, and relationships to
  movies, TV, music, books, audiobooks, comics, photos, documents, and more.
- **Track your cataloging work.** Keep an entry worklog, defer unresolved items,
  and check required work before marking an entry complete.
- **Make collections from searches.** Save a selection, such as favorite films or
  books by an author, and refresh it into a symlink folder when you choose.
- **Build symlink libraries for your apps.** Generate popular folder formats or
  custom layouts from the same catalog, with links back to your source files.
- **Keep useful metadata alongside your media.** Export catalog details,
  playlists, and metadata files for other tools.
- **Keep track of custom versions.** Catalog your own remuxes and edits, or use
  optional FFmpeg recipes to generate separate files with recorded source relationships.
- **Generate previews and smaller versions with rules.** Apply a recipe to a
  saved selection, backfill existing media, and include new matches during
  maintenance. Preview the estimated additional storage before queuing work.
- **Give each library the versions it needs.** Publish a separate transcode
  library, keep originals in another, and register results from external processors.
- **Coordinate network processing.** Submit work through a versioned HTTP receipt
  adapter, with durable jobs and worker leases. Estimates learn from successful
  local renders, and paged rules can select up to 100,000 IDs. See
  [processor workflows](docs/PROCESSORS.md).

For example, the same film can appear in your Plex library and a favorites folder
without storing another copy of the movie. Each symlink points to the existing
file while giving it a name and location suited to that library. You can preview
the changes before applying them.

## Works with your media apps

Folder presets cover **Plex, Jellyfin, Emby, Kodi, Infuse, Navidrome,
Audiobookshelf, Komga, Kavita**, and other applications. Custom naming rules let
you build a different structure. There are also explicit import options for
calibre, Calibre-Web, and Immich.

See [supported applications](https://github.com/Jagalite/catabolic/blob/main/docs/COMPATIBILITY.md) for the full list and each
integration's requirements. Folder presets and import options behave differently;
imports copy or upload selected media.

## Install

Requires **Python 3.11+** on **macOS or Linux**.

```sh
python3 -m venv ~/.venvs/catabolic
. ~/.venvs/catabolic/bin/activate
python -m pip install 'git+https://github.com/Jagalite/catabolic.git'
catabolic --help
```

Prefer pipx or want to install a specific version? See the
[installation guide](https://github.com/Jagalite/catabolic/blob/main/docs/INSTALLATION.md).

## Try it without touching your library

The [getting started walkthrough](https://github.com/Jagalite/catabolic/blob/main/docs/GETTING_STARTED.md) creates a small sample
collection and walks through cataloging, tagging, and generating your first
folders. No media server, API key, or FFmpeg installation is needed.

Then add your own source locations, identify the files you want to organize,
choose an output format, and preview the result.

Catabolic is a command-line application and is currently **alpha**. Keep backups
and start with a small collection. Generated folders need access to their source
files; they are not independent backups.

## Operator guide

The recommended cycle is **scan → review and catalog → preview outputs → sync
and verify → repeat maintenance**. Start with the
[setup walkthrough](https://github.com/Jagalite/catabolic/wiki/Getting-Started)
to register sources and configure output catalogs and layouts. Then select your
database and profile for the commands below:

```sh
export CATABOLIC_DB=/absolute/path/catalog.sqlite3
export CATABOLIC_PROFILE=default
catabolic db status
```

If an upgrade is required, follow the
[migration guide](https://github.com/Jagalite/catabolic/wiki/Database-Migrations)
before continuing.

### Discover and review

```sh
catabolic scan
catabolic files --unidentified
catabolic item list --curation-status pending
catabolic item list --curation-status deferred
catabolic item list --curation-status needs_attention
```

Review identities, associations and tags, and record decisions in each entry's
worklog. Follow pagination to see the full backlog. The
[recommended workflow](https://github.com/Jagalite/catabolic/wiki/Recommended-Workflow)
covers curation, completion checks, and the first preview/apply/sync/verify cycle
for your symlink folders.

### Run routine maintenance

Once your output layouts are configured:

```sh
# Scan sources, refresh output links, verify them, and update metadata manifests.
catabolic maintenance --all-catalogs --manifest

# Or scan and report the backlog without updating output folders.
catabolic --json maintenance --inventory-only
```

The report includes new files, uncataloged files, entry statuses and unfinished
jobs. Maintenance defaults to zero output removals; it does not identify media
or mark entries complete. See the
[maintenance guide](https://github.com/Jagalite/catabolic/wiki/Maintenance)
for scan exclusions, budgets, optional analysis and handling incomplete cycles.

### Backfill previews or transcodes

Follow the [processing rules guide](https://github.com/Jagalite/catabolic/wiki/Processing-Rules)
to collect probe metadata and save a recipe, selection and generated destination.
Replace `RULE_ID` with the saved rule's ID, then review its full storage estimate:

```sh
catabolic rule preview RULE_ID
# Queue up to 10 outputs with a 10 GiB estimated planning budget.
catabolic rule apply RULE_ID --batch 10 --max-new-bytes 10737418240
catabolic rule run RULE_ID --batch 1
```

Estimates show additional space, a likely range and any unknown sizes; they are
not guaranteed output sizes. Rendering requires an external FFmpeg installation
and creates separate files, preserving originals. After reviewing a rule, opt it
into future maintenance cycles:

```sh
catabolic rule enable RULE_ID
catabolic maintenance --all-catalogs --rules --rule-batch 10 --render-rules 1
```

`--rules` queues enabled rules; `--render-rules 1` also renders at most one job.
Ordinary maintenance does neither. For agents and scheduled runs, use global
`--json` and check exit codes; see [automation](https://github.com/Jagalite/catabolic/wiki/Automation).

## Learn more

- [Wiki](https://github.com/Jagalite/catabolic/wiki) — walkthroughs and detailed guides.
- [Recommended workflow](https://github.com/Jagalite/catabolic/blob/main/docs/WORKFLOW.md) — the everyday cataloging cycle; also `catabolic docs workflow`.
- [Processing rules](https://github.com/Jagalite/catabolic/wiki/Processing-Rules) — recipes, retroactive storage estimates and maintenance; also `catabolic docs rules`.
- [Rendition workflows](https://github.com/Jagalite/catabolic/wiki/Rendition-Workflows) — transcode libraries, external result receipts and completion requirements.
- [Using Catabolic with agents](https://github.com/Jagalite/catabolic/blob/main/docs/AUTOMATION.md) — automation and structured output.
- [SQL queries](https://github.com/Jagalite/catabolic/blob/main/docs/QUERYING.md) and [GraphQL](https://github.com/Jagalite/catabolic/blob/main/docs/GRAPHQL.md) — explore the catalog.
- [Troubleshooting](https://github.com/Jagalite/catabolic/blob/main/docs/TROUBLESHOOTING.md) — common questions and recovery steps.
- [Development](https://github.com/Jagalite/catabolic/blob/main/docs/DEVELOPMENT.md) — contribute, run tests, and maintain the docs.

Documentation is also available offline: run `catabolic docs` after installation.

## License

[MIT](https://github.com/Jagalite/catabolic/blob/main/LICENSE). Copyright 2026 Jaga Tranvo and The Catabolic Contributors.
