# Catabolic

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

**A programmable media catalog.**

People and agents decide what belongs. Queries select catalog state. Rules
perform explicit processing and record results. Projections publish selected media
into maintained output structures. Consumer integrations connect those outputs
to applications; optional notifications tell people what happened.

Inventory media across drives and mounted storage, identify logical items, and
inspect their metadata, relationships, technical facts and processing evidence
with SQL or GraphQL. Save a reusable query, attach an operation rule or an output
projection, preview, execute a bounded batch, and verify the resulting state.

Completed processing becomes catalog evidence. A query for media missing a
rendition stops matching once that result is recorded. Different projections can
select different versions of the same item and maintain independent Plex,
Jellyfin, music or document layouts without changing original files. Durable jobs,
receipts and filesystem journals preserve recovery when work is interrupted.

Start with the [query → rule → projection guide](docs/PROGRAMMABLE_CATALOG.md),
including implemented operation types, compatibility and validation limits.

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
  [Automatic catalog updates](docs/CATALOG_REFRESH.md) can add links when new
  versions become ready, with durable retries for offline destinations.
- **Coordinate network processing.** Submit work through a versioned HTTP receipt
  adapter, with durable jobs and worker leases. Estimates learn from successful
  local renders, and paged rules can select up to 100,000 IDs. See
  [processor workflows](docs/PROCESSORS.md).

For example, the same film can appear in your Plex library and a favorites folder
without storing another copy of the movie. Each symlink points to the existing
file while giving it a name and location suited to that library. You can preview
the changes before applying them.

## Build an application on the catalog

The optional [HTTP backend](docs/HTTP.md) exposes authorized GraphQL, operator SQL,
saved queries, metadata, revision-pinned content and durable rendition requests.
Scoped tokens can permit browser derivatives while denying originals. A separate
foreground worker prepares approved recipes; reusable short-lived tickets support
browser range requests. CLI and HTTP share jobs, artifacts and recovery.

Install the optional HTTP dependencies and configure grants/source exposure before
starting `catabolic --db catalog.db api serve`. The server defaults to loopback
and existing-content mode. Processing requires explicit enablement and a worker.
The [HTTP guide](docs/HTTP.md) records limits and current release qualification.

## Works with your media apps

Folder presets cover **Plex, Jellyfin, Emby, Kodi, Infuse, Navidrome,
Audiobookshelf, Komga, Kavita**, and other applications. Custom naming rules let
you build a different structure. There are also explicit import options for
calibre, Calibre-Web, and Immich.

See [supported applications](https://github.com/Jagalite/catabolic/blob/main/docs/COMPATIBILITY.md) for the full list and each
integration's requirements. Folder presets and import options behave differently;
imports copy or upload selected media.

### Start from an existing Plex library

[Import Plex metadata](docs/CONSUMERS.md#import-an-existing-plex-library-into-catabolic)
to bootstrap file identification from your existing library. Connect your Plex
server, map its media paths to scanned Catabolic sources, then preview and apply
a bounded page. Imports preserve existing metadata, retain Plex identity and
provenance, and report ambiguous or unavailable files for review. They do not copy
media or change Plex. Movies, episode files, music tracks and photos are supported;
parent relationships, playlists and watched state are outside the import scope.

### Connect published outputs to Plex or Jellyfin

[Consumer bindings](docs/CONSUMERS.md) connect a projection or media subtree to an
exact server and library identity. Configure a binding once: verified publication
then records durable scan generations and can automatically request library scans.
Unchanged output creates no new scan. Offline servers leave retryable delivery
work without rerendering completed media or rewriting unchanged links.

Plex supports browser sign-in, server/library discovery, existing-library binding,
explicit library creation, normal section scans and bounded file-path indexing checks. Jellyfin supports
existing-library bindings and scans; legacy refresh commands remain available.
Folder naming presets for other apps do not imply an API integration.

Scan acceptance and verified indexing are separate outcomes. Plex must be able
to read both published symlinks and their resolved targets. Protocol and recovery
fixtures are tested; live Plex acceptance remains unverified. See the
[validation record](docs/CONSUMER_VALIDATION.md) for evidence and limitations.

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

### Set up delivery once

Start with a configured projection named `cinema` that publishes movie links
under `Movies`. Use the database/profile selected above. Sign in with Plex, open
the returned `authorization_url`, then complete the login with its `login_id`:

```sh
catabolic consumer plex-login
catabolic consumer plex-login-complete LOGIN_ID
```

Completion returns a private `credential_file` path without printing the token.
Replace the example endpoint with your Plex server address and `CREDENTIAL_FILE`
with that path. Login does not discover the server address automatically.

```sh
catabolic consumer connection-put home --application plex \
  --endpoint https://plex.example.test:32400 --credential-file CREDENTIAL_FILE --apply
catabolic consumer discover home --type movie
```

If discovery reports library `7` rooted at `/media/Movies`, preview and then save
the binding. The remote root is the path **as Plex sees it**, which may differ
from the host's output path. Plex also needs access to the symlinks' targets.

```sh
catabolic consumer bind cinema-plex --connection home --catalog cinema \
  --subtree Movies --remote-root /media/Movies --library-id 7 --type movie \
  --automatic --initial-scan
catabolic consumer bind cinema-plex --connection home --catalog cinema \
  --subtree Movies --remote-root /media/Movies --library-id 7 --type movie \
  --automatic --initial-scan --apply
catabolic projection execute cinema
catabolic consumer run --limit 10
catabolic consumer bindings
catabolic consumer verify-indexing cinema-plex --limit 100
```

`--initial-scan` schedules existing published files; `--automatic` enables delivery
after future publication. A saved binding alone does not start a worker. Scan
acceptance means Plex accepted the request; `verify-indexing` separately reports
whether expected paths were found and may be inconclusive while indexing or in a
large library. An output catalog without a saved projection can use `sync` and
`verify` instead; see the [consumer guide](docs/CONSUMERS.md).

For SSH, open the sign-in link on another device. Scheduled workers need access
to the same credential file. An existing token can instead be supplied through
protected configuration with `--credential-env PLEX_TOKEN`; see
[sign-in and credential setup](docs/CONSUMERS.md#sign-in-with-plex).

Publication performs a bounded delivery drain when automatic delivery is enabled.
Schedule `catabolic consumer run --limit 10` for delayed retries, or supervise
`catabolic consumer watch --interval 30`. The CLI does not start a background
service. Inspect `consumer attempts` and `consumer events` for separate outcomes.

The [consumer guide](docs/CONSUMERS.md) covers explicit library creation, path
mapping, repair and optional Apprise subscriptions. Apprise is installed separately
with the `notifications` extra; its per-destination retries are independent of
scan delivery. Preview and apply any required database upgrade before setup;
the current schema is 20. Upgrading does not enable automatic network actions.

## Learn more

- [Wiki](https://github.com/Jagalite/catabolic/wiki) — walkthroughs and detailed guides.
- [Recommended workflow](https://github.com/Jagalite/catabolic/blob/main/docs/WORKFLOW.md) — the everyday cataloging cycle; also `catabolic docs workflow`.
- [Processing rules](https://github.com/Jagalite/catabolic/wiki/Processing-Rules) — recipes, retroactive storage estimates and maintenance; also `catabolic docs rules`.
- [Output consumers](docs/CONSUMERS.md) — Plex/Jellyfin setup, durable scan delivery and optional notifications; also `catabolic docs consumers`.
- [Rendition workflows](https://github.com/Jagalite/catabolic/wiki/Rendition-Workflows) — transcode libraries, external result receipts and completion requirements.
- [Using Catabolic with agents](https://github.com/Jagalite/catabolic/blob/main/docs/AUTOMATION.md) — automation and structured output.
- [SQL queries](https://github.com/Jagalite/catabolic/blob/main/docs/QUERYING.md) and [GraphQL](https://github.com/Jagalite/catabolic/blob/main/docs/GRAPHQL.md) — explore the catalog.
- [Troubleshooting](https://github.com/Jagalite/catabolic/blob/main/docs/TROUBLESHOOTING.md) — common questions and recovery steps.
- [Development](https://github.com/Jagalite/catabolic/blob/main/docs/DEVELOPMENT.md) — contribute, run tests, and maintain the docs.

Documentation is also available offline: run `catabolic docs` after installation.

## License

[MIT](https://github.com/Jagalite/catabolic/blob/main/LICENSE). Copyright 2026 Jaga Tranvo and The Catabolic Contributors.

Owner-controlled source identity settings and their evidence are described in [Trust policies](docs/TRUST_POLICIES.md), also available with `catabolic docs trust`.
