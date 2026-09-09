# Frequently asked questions

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

## Can one database include several drives?

Yes. Bind each source tree as a separate location and scan them into the same
database. Catalog selection can combine their files into one output. Keep the
database itself on a local filesystem. A disconnected source remains queryable
through its last recorded observations; availability is not checked by queries.

## Is the database independent of paths?

Logical items, identities, and relationships have database-scoped IDs independent
of output naming. File occurrences retain source-relative paths, and profiles
bind logical locations/catalogs to absolute roots with filesystem identity checks.
It is portable in parts, not path-free. Moving or replacing a root requires a
controlled binding update and scan; arbitrary file moves are not inferred solely
from the filename or media title. See [operations](OPERATIONS.md).

## Does scanning identify or rename my media?

Scanning inventories files. It does not automatically determine their logical
identity or rename source files. You or an agent provides curation decisions;
optional probes and proposals provide evidence. Output layouts choose generated
link names. Existing source basenames can stay as they are.

## Does Catabolic work beyond video?

Yes. Items cover music, books, audiobooks, podcasts, comics, photos, documents,
and custom kinds. File roles include covers, subtitles, transcripts, lyrics,
extras, and source material. Native naming covers all kinds and roles; application
presets cover narrower subsets. Structured metadata extraction varies by format;
Catabolic does not promise universal OCR, EXIF extraction, or transcription.

## What is Catabolic's own output format?

The `catabolic` preset uses
`{item.kind}/{item.id}/{association.role}/{file.id}/{file.name}` with portable
sanitization. Its naming contract is `catabolic.native` version 1. The separate
JSON metadata format is `catabolic.catalog-manifest`, currently version 3.
Plex/custom layouts can export that same manifest without using native filenames.

## Does a folder name choose its application format?

No. `catalog bind jellyfin` chooses the default `catabolic/jellyfin` directory.
`layout put jellyfin --preset jellyfin` chooses rules, and `layout apply` records
the resulting mappings. Layout and catalog names need not match, but matching
names make small configurations easier to follow.

## Are hardlinks better than symlinks?

It depends on the consumer and storage. Symlinks can reference multiple mounted
filesystems but need their targets visible to the consumer. Hardlinks require the
same filesystem and share bytes, permissions, and in-place writes with the source.
Neither is an independent backup. Catabolic protects final hardlink references
and retains retired ones; see [hardlinks](HARDLINKS.md).

## Can SQL or GraphQL change the database?

The query interfaces are read-only. Use explicit CLI mutations for changes so
validation, locking, and recovery apply. SQL views expose all profiles where
appropriate; filter `profile=:profile`. GraphQL runs through the CLI, not a hosted
HTTP endpoint. Catabolic does not start a GraphQL server.

## Can a query become a folder?

Yes. Save SQL returning `item_id`, `file_id`, or `association_id`, or a supported
paginated GraphQL selection in a layout. Preview/apply its mappings, then sync.
Selections refresh explicitly. Incomplete queries abort the refresh, and retiring
all generated members requires explicit empty-result handling. See
[query folders](QUERY_FOLDERS.md).

## Is a manifest enough to back up the catalog?

No. It describes a catalog projection and referenced metadata. Current manifest
versions do not include processing jobs, attempt history, proposals, or every
database record. Keep a consistent SQLite backup for full state preservation.
The manifest checksum detects content edits; it is not a media-content hash or
signature. See [manifests](MANIFESTS.md) and [migrations](MIGRATIONS.md).

## Will upgrading silently change my database or folder names?

Ordinary commands do not auto-migrate. Review `db upgrade --dry-run`, then use the
explicit backup/rehearsal upgrade. Stored naming definitions are not silently
replaced by newer presets. Edit/apply a layout deliberately and preview its sync.
Database schema versions, manifest versions, and naming versions are independent.

## Can I use this from an agent or scheduler?

Yes. The CLI provides JSON, schema discovery, bounded queries, explicit inputs,
and no interactive curation prompts. Follow pagination and exit statuses. Use
proposals when decisions need review, removal budgets for scheduled sync, and
recovery after interruptions. [Automation](AUTOMATION.md) explains the contract.

## Can Catabolic sign in to Plex and update my library?

Yes. Run `catabolic consumer plex-login`, open the returned authorization link,
and use `consumer plex-login-complete LOGIN_ID` after approving it. Completion
returns a private credential-file path; Catabolic does not ask for your password
or print the token. You then explicitly configure your server address, discover
its libraries, and bind an output catalog to a library ID. Login alone does not
create or scan a library.

A binding with `--automatic` delivers scans after verified publication. Use
`consumer run` on a schedule or supervise `consumer watch` for delayed retries.
Unchanged publication does not request another scan. Catabolic can also explicitly
create a Plex library using scanner/agent choices reported by that server. See
[the Plex setup walkthrough](CONSUMERS.md).

## Can I import my existing Plex catalog?

Yes. `consumer import` reads a selected Plex library using a saved connection and
maps exact remote file paths to scanned Catabolic sources. Preview a page, review
its proposed metadata and associations, then apply using the returned plan ID.
Existing fields are preserved and unchanged repeats do not duplicate items.
Conflicting associations and unavailable files remain explicit review work.

The import covers file-backed movies, episodes, tracks and photos. It does not
copy media or import watched state, playlists, artwork, or parent relationships.
See [Plex import](CONSUMERS.md#import-an-existing-plex-library-into-catabolic) for
path mappings, pagination, conflict handling and restart behavior.

## Plex accepted the scan, so why are my files missing?

Scan acceptance and indexing are separate outcomes. Check `consumer bindings`,
`consumer attempts`, and `consumer events`, then run
`consumer verify-indexing BINDING_ID --limit 100`. The bounded path check can be
inconclusive while Plex is indexing or when the library exceeds the checked page.
Confirm that the binding's remote root matches Plex's namespace and that Plex can
read both the published symlinks and their resolved targets. Matching titles or
file counts do not establish that the expected files were indexed or can play.

## Is it a media player, downloader, or backup system?

No. Catabolic inventories, describes, queries, and projects existing files.
Consumers play or display them. Import adapters copy/upload selected media to
external applications explicitly. Source backups, media acquisition, and source
editing remain separate responsibilities.

## Is every target certified and is this production-ready?

The package is marked alpha. Naming fixtures, local tests, real consumer acceptance,
and remote CI are different kinds of evidence. Consult the exact revision's
[Actions results](https://github.com/Jagalite/catabolic/actions), target discovery,
and [release verification guide](RELEASE_TESTING.md). A preset existing does not
establish every server version or network filesystem works.
