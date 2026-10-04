# Catabolic

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

![Catabolic — a glowing blue cube dissolving into colorful film frames](https://raw.githubusercontent.com/Jagalite/catabolic/main/docs/assets/catabolic-banner.png)

**One media collection. Many libraries. Keep your originals where they are.**

Catabolic is a command-line media catalog for files spread across drives and
mounted storage. Identify and tag your media, search it with SQL or GraphQL, and
publish selected files into folders your apps understand. Generated symlink
libraries point back to your existing files, so the same collection can have
several layouts without duplicating the media.

For example, a film on your NAS can appear in both a Plex library and a favorites
folder, each with its own naming scheme. An optional processing rule can create
a smaller version in separate storage and publish it to another library.
Catabolic records which original each generated version came from.

[Get started](docs/GETTING_STARTED.md) · [Documentation](https://github.com/Jagalite/catabolic/wiki) · [FAQ](docs/FAQ.md)

## What you can do

- **Bring scattered media into one catalog.** Inventory movies, TV, music, books,
  photos, documents, and more. Search recorded metadata even when a drive is offline.
- **Curate once, publish in several places.** Add identities, tags, and relationships;
  save selections and generate app-specific or custom folder layouts.
- **See what needs attention.** The [work inbox](docs/INBOX.md) brings together
  unidentified files, pending reviews, failed processing, and publication work.
- **Make and track additional versions.** Use optional FFmpeg recipes for previews,
  remuxes, and transcodes, or register results from external processors. Review
  storage estimates before queuing work.
- **Keep libraries up to date.** Opt into [watchers](docs/WATCHERS.md) for scheduled
  scans and query-driven work, and connect published outputs to Plex or Jellyfin
  for library refreshes.

## Install

Requires **Python 3.11+** on **macOS or Linux**. Catabolic is currently **alpha**.

```sh
python3 -m venv ~/.venvs/catabolic
. ~/.venvs/catabolic/bin/activate
python -m pip install catabolic
catabolic --help
```

See [installation](docs/INSTALLATION.md) for pipx, checkout installs, and upgrades.
Repository documentation may describe changes beyond the installed release;
`catabolic docs` opens the guides bundled with your version.

## Try it

The [first-catalog walkthrough](docs/GETTING_STARTED.md) takes a disposable sample
file from inventory to identification to a verified symlink library. It needs no
media server, API key, or FFmpeg installation.

To start inventorying your own media, create a workspace on a local disk, outside
your source folders. Replace `/path/to/your/media` before running:

```sh
mkdir -p ~/catabolic-workspace/state
cd ~/catabolic-workspace
export CATABOLIC_DB="$PWD/state/catalog.sqlite3"
export CATABOLIC_PROFILE=default

catabolic init
catabolic location bind media --root /path/to/your/media
catabolic scan
catabolic --json files --unidentified --limit 20
```

This records files in the catalog. Identifying them and publishing a library are
separate steps; continue with the [recommended workflow](docs/WORKFLOW.md).
Keep `CATABOLIC_DB` and `CATABOLIC_PROFILE` set when returning in another terminal.

Scans default to common media extensions. [Scan policies](docs/OBSERVATIONS.md#file-types-and-regex-filters)
let you add file types, scan all files, or use include/exclude patterns. Filtering
retains recorded history and does not establish that a file is missing. Scans
also report unfinished work; see [coverage and continuation](docs/OBSERVATIONS.md#guarded-discovery-and-continuation-schema-27).

## From a search to a library

Catabolic stores media identities, file locations, relationships, and processing
results in SQLite. Three reusable definitions turn that catalog into workflows:

| Definition | What it does | Example |
| --- | --- | --- |
| **Query** | Selects catalog records using SQL or GraphQL | Movies without a recorded transcode |
| **Rule** | Applies an explicit processing or analysis operation to a selection | Generate a smaller version of each selected movie |
| **Projection** | Maps query results to an output layout or destination metadata | Build a library or curate genres and collection membership |

Processing results become catalog evidence. On the next evaluation, a query for
missing versions stops selecting items whose results have been recorded.
Queries can also feed projections directly when no processing is needed.
[Destination mappings](docs/CONSUMERS.md#query-driven-destination-mappings) apply
query results to Plex/Jellyfin metadata and collections, all folder presets,
calibre/Immich imports, and NFO/OPDS/XSPF exports. Each operation declares its
verification and recovery capabilities and requires a reviewed plan.

Preview the selection and planned changes, execute a bounded batch, then verify
the result. Jobs and filesystem journals support recovery after interruptions.
Start with the [query, rule, and projection guide](docs/PROGRAMMABLE_CATALOG.md).

## Use it with your apps

Folder presets include **Plex, Jellyfin, Emby, Kodi, Infuse, Navidrome,
Audiobookshelf, Komga, and Kavita**. A preset controls folder naming; API integration
is a separate capability. See [application compatibility](docs/COMPATIBILITY.md)
for requirements and the full list.

For **Plex and Jellyfin**, [consumer bindings](docs/CONSUMERS.md) connect published
outputs to an existing server library and can request scans after publication.
Plex also supports browser sign-in, explicit library creation, and
[metadata import from an existing library](docs/CONSUMERS.md#import-an-existing-plex-library-into-catabolic)
to help identify files you have already scanned. You can also
[match existing published files](docs/CONSUMERS.md#match-existing-published-files-to-plex) and
[publish curated metadata to Plex](docs/CONSUMERS.md#publish-catalog-metadata-to-plex),
with a field-by-field preview and verification.

A media server must be able to read both the symlinks and their source targets.
An accepted scan request does not prove indexing completed; see the
[consumer validation record](docs/CONSUMER_VALIDATION.md) for tested behavior and
live-service qualification limits.

## Automate or build on it

Use the CLI's global `--json` option and exit codes in scripts and agent workflows.
Saved queries, explicit previews, and the work inbox provide a shared workflow
for people and automation. See [automation](docs/AUTOMATION.md),
[SQL queries](docs/QUERYING.md), and [GraphQL](docs/GRAPHQL.md).

The optional [HTTP backend](docs/HTTP.md) exposes authorized queries, metadata,
media content, and durable rendition requests. It includes scoped tokens,
OpenAPI and GraphQL contracts, and a TypeScript client contract. Serving content
and enabling processing require explicit configuration.

## Working with your collection

Catabolic preserves source media. Symlink publication creates managed output
folders; processing creates separate files. Explicit application imports, such
as calibre or Immich imports, copy or upload selected media.

Generated libraries depend on their sources and are not backups. Start with a
small collection, keep backups, and preview changes before applying them.

| Next step | Guide |
| --- | --- |
| Identify, curate, and publish your first collection | [Recommended workflow](docs/WORKFLOW.md) |
| Review outstanding work and completion requirements | [Work inbox](docs/INBOX.md) · [Worklogs](docs/WORKLOG.md) |
| Schedule scans and maintain outputs | [Watchers](docs/WATCHERS.md) · [Maintenance](docs/MAINTENANCE.md) |
| Generate previews, remuxes, or transcodes | [Processing rules](docs/RULES.md) · [External processors](docs/PROCESSORS.md) |
| Handle offline sources and choose fallback versions | [Fallback policies](docs/FALLBACK.md) · [Source trust](docs/TRUST_POLICIES.md) |
| Upgrade or diagnose a problem | [Database migrations](docs/MIGRATIONS.md) · [Troubleshooting](docs/TROUBLESHOOTING.md) |
| Contribute or publish a release | [Development](docs/DEVELOPMENT.md) · [Release testing](docs/RELEASE_TESTING.md) |

All guides are also available offline with `catabolic docs`.

[MIT License](LICENSE). Copyright 2026 Jaga Tranvo and The Catabolic Contributors.
