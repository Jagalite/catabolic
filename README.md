# Catabolic

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

**One media inventory. Many organized libraries. A CLI for people and agents.**

Catabolic catalogs files across local drives and mounted storage in SQLite, then
builds organized symlink or hardlink folders for the applications that use them.
Keep your source filenames, attach identities and metadata, query the collection
with SQL or GraphQL, and generate a different view for each app or purpose.

An agent or person supplies the media judgment. Catabolic handles inventory,
explicit catalog decisions, naming rules, and recoverable filesystem operations.

[Getting started](GETTING_STARTED.md) · [Wiki](https://github.com/Jagalite/catabolic/wiki) ·
[Application outputs](COMPATIBILITY.md) · [SQL](QUERYING.md) · [Agent guide](AUTOMATION.md)

## What you can do

- **Combine sources:** catalog movies, TV, music, audiobooks, podcasts, books,
  comics, photos, documents, and custom media kinds in one database.
- **Describe your collection:** record provider identities, file roles, ordered
  parts, relationships, namespaced tags, and evidence for curation decisions.
- **Ask useful questions:** join and group catalog data with read-only SQL, use
  nested GraphQL queries, or filter files and items directly from the CLI.
- **Build application libraries:** use presets for Plex, Jellyfin, music servers,
  readers, and photo apps, or define your own path templates.
- **Turn queries into folders:** save a SQL or GraphQL selection and explicitly
  refresh its membership, naming, and links.
- **Inspect media when needed:** run lightweight signature checks, optional
  `ffprobe` analysis, content hashing, decode verification, and text extraction
  with bounded, resumable jobs.
- **Export metadata:** generate versioned JSON manifests, XSPF playlists, OPDS
  catalogs, or NFO metadata alongside selected media.

```text
Source locations                 One SQLite database        Generated catalogs
/Volumes/Films ──────┐                                    ┌─ catabolic/plex/
/mnt/music ─────────┼── scan → identify/tag → select ─────┼─ catabolic/jellyfin/
/mnt/books ─────────┘                  ↓                 ├─ catabolic/favorites/
                               preview → apply → sync ──└─ catabolic/native/
```

Several catalogs can reference the same source file. Creating links does not
copy the media bytes. Hardlinks share the source inode and require the same
filesystem; symlinks are the default.

## Install

Requires **Python 3.11+** on **macOS or Linux**. This is an alpha application;
review the [release verification guide](RELEASE_TESTING.md) and
[current CI](https://github.com/Jagalite/catabolic/actions) before deploying it.
Windows is not currently supported.

Install from this repository into a virtual environment with pip:

```sh
python3 -m venv ~/.venvs/catabolic
. ~/.venvs/catabolic/bin/activate
python -m pip install 'git+https://github.com/Jagalite/catabolic.git'
catabolic --version
catabolic --help
```

Or use an existing pipx installation:

```sh
pipx install 'git+https://github.com/Jagalite/catabolic.git'
```

These commands use GitHub as the package source. For repeatable installations,
pin a reviewed commit; see [installation and upgrades](INSTALLATION.md).
FFmpeg and ffprobe are optional external tools. Basic cataloging, queries, tags,
and link generation work without them. They are not bundled with Catabolic.

## Your first library

This example organizes one movie into a Plex catalog. Replace `/absolute/path/to/movies`
with an existing source directory. Use a workspace outside your source trees.
For a fully runnable example using a disposable text fixture instead, follow
[the walkthrough](GETTING_STARTED.md).

```sh
mkdir -p ~/catabolic-workspace/state
cd ~/catabolic-workspace
export CATABOLIC_DB="$PWD/state/catalog.sqlite3"
export CATABOLIC_PROFILE=default

catabolic init
catabolic location bind movies --root /absolute/path/to/movies
catabolic scan
catabolic --json files --unidentified --limit 20
```

Scanning inventories files. It does not guess which movie each file contains.
Choose a returned file ID, then supply the correct title and year. The metadata
below is an example; replace it with the identity of your selected file.

```sh
catabolic --json item put --kind movie \
  --identity 'local.movie=example-movie' \
  --metadata '{"title":"Example Movie","year":2026}'

catabolic association put --file FILE_ID --item ITEM_ID --role primary
catabolic catalog bind plex
catabolic layout put plex --preset plex-v1
catabolic layout preview plex --catalog plex
catabolic layout apply plex --catalog plex
```

`FILE_ID` and `ITEM_ID` are placeholders for the IDs returned by the preceding
commands. Replace the example local identity with a stable key of your own or a
verified provider identity such as `tmdb.movie=...`. `layout apply` records desired
paths in the database. Review the live
filesystem plan before creating the links:

```sh
catabolic sync --catalog plex --dry-run
catabolic sync --catalog plex
catabolic verify --catalog plex
catabolic manifest --catalog plex --in-catalog
```

The movie appears beneath `catabolic/plex/Movies/`, with its source extension.
The manifest is `catabolic/plex/.catabolic-manifest.json`. Configure Plex to read
that `Movies` directory and make the source targets accessible to it.

Omitting `--root` creates `./catabolic/NAME` on the first catalog bind. Later binds
reuse the stored path. A catalog name chooses the output folder; the layout
preset chooses the naming rules. See [custom layouts](OUTPUT_LAYOUTS.md) for
other destinations and [container path requirements](TROUBLESHOOTING.md).

## Query it, tag it, project it

The following examples use the database selected by `CATABOLIC_DB`:

```sh
# Discover query views and columns.
catabolic --json query --schema

# Count logical items by media kind.
catabolic --json query \
  'SELECT kind, COUNT(*) AS items FROM catalog_items GROUP BY kind ORDER BY kind'

# Retrieve a page through GraphQL.
catabolic graphql '{ items(first: 10) { nodes { id title kind } pageInfo { hasNextPage endCursor } } }'

# Add a tag to an existing item.
catabolic tag put collection:favorite
catabolic tag add collection:favorite --item ITEM_ID
catabolic --json item list --tag collection:favorite
```

Queries use stored observations and work while sources are offline. `scan`
refreshes inventory; `verify` checks current output health. Saved query folders
refresh only when you run layout preview/apply followed by sync. See
[query folders](QUERY_FOLDERS.md) and [tagging](TAGGING.md).

## Application outputs

Catabolic includes **17 application folder profiles** and **3 explicit import
adapters**. Run `catabolic target list` or `catabolic target show jellyfin` for
rules, supported kinds, required metadata, and validation status.

| Use | Targets |
| --- | --- |
| Movies and TV | Plex, Jellyfin, Emby, Kodi, Infuse |
| Music | Navidrome, MPD, gonic, Airsonic |
| Audiobooks and podcasts | Audiobookshelf |
| Books and comics | Komga, Kavita, Ubooquity, Stump |
| Photos and personal video | PhotoPrism, Photoview, Piwigo |
| Explicit imports | calibre, Calibre-Web, Immich |

Naming support does not certify every application's scanner. Imports deliberately
copy or upload through the target's CLI. See [compatibility](COMPATIBILITY.md)
for requirements and the distinction between folder presets and import adapters.
The [native Catabolic format](NATIVE_LAYOUT.md) covers all media kinds, and
custom templates let you support another application without changing sources.

## Safety and recovery

Catabolic uses explicit source bindings, complete-scan publication, output
ownership markers, and a journal for link operations. It refuses foreign output
collisions and revalidates sources before changing links. Database upgrades are
explicit and include a verified backup and rehearsal.

Keep the database on local storage, keep source and output trees separate, and
give Catabolic exclusive control of generated folders. Hardlinks share writes
with their source and are not backups. Retired hardlinks are retained; final
references are protected. Preview large changes with removal budgets:

```sh
catabolic sync --catalog plex --dry-run --max-removals 10 --max-removal-percent 5
```

Pass the same budgets to the actual sync. After an interruption, inspect `status`
and run `recover` for the affected scope before continuing. See
[operations](OPERATIONS.md), [hardlinks](HARDLINKS.md), and
[database upgrades](MIGRATIONS.md) for the guarantees and their limits.

## Documentation and automation

Detailed guides are available in the [wiki](https://github.com/Jagalite/catabolic/wiki)
and in every installed CLI, without a database or network connection:

```sh
catabolic docs
catabolic docs getting-started
catabolic docs query
catabolic docs automation
catabolic docs --search 'manifest version'
catabolic --json docs graphql
```

Use `catabolic --db PATH --profile NAME --json COMMAND` for automation. Commands
do not prompt for curation decisions. Agents should inspect exit codes, follow
pagination, and distinguish previews from writes. The [agent guide](AUTOMATION.md)
covers invocation, errors, completeness, retries, and the full decision workflow.

For contributors: [development](DEVELOPMENT.md), [architecture](DESIGN.md),
[release tests](RELEASE_TESTING.md), [Open Catalog specification](OPEN_CATALOG.md),
and [performance measurements](SCALE_BENCHMARKS.md).

## License

[MIT](LICENSE). Copyright 2026 Jaga Tranvo and The Catabolic Contributors.
Files carry SPDX notices; JSON and checksum-sensitive artifacts use adjacent
`.license` files. Dependencies and provider data retain their respective terms.
