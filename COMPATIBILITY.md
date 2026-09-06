# Application outputs and compatibility

Catabolic exposes 20 application targets through `target list` and `target show`.
Seventeen use versioned naming profiles; calibre, Calibre-Web and Immich use
explicit import adapters. These are implementation-level capabilities with
synthetic fixture validation, not certifications of every application's scanner.
`tested_application_versions` is empty until an actual application scan is recorded.

```sh
catabolic target list
catabolic target show jellyfin
catabolic layout presets
catabolic docs compatibility
```

Target discovery always returns JSON and requires no database. It includes the
mechanism, preset name, supported kinds and roles, exact rules (including file
extensions and required template fields), upstream documentation and validation
status. The target interface and each naming profile currently have version 1.

## Folder targets

| Target | Preset | Scope |
| --- | --- | --- |
| Plex | `plex-v1` | Movies, editions, subtitles, posters, extras, numbered TV and music |
| Jellyfin | `jellyfin` | Movies, version labels, subtitles, posters, extras, numbered TV and music |
| Emby | `emby` | Movies, version labels, subtitles, posters, extras, numbered TV and music |
| Kodi | `kodi` | Movies, numbered TV and music; optional NFO export |
| Infuse | `infuse` | Movies and numbered TV; optional NFO export |
| Navidrome | `navidrome` | Album folders, tracks, covers and lyrics; embedded tags remain authoritative |
| MPD | `mpd` | Album folders, tracks, covers and lyrics; XSPF exports |
| gonic | `gonic` | One album per folder, tracks, covers and lyrics |
| Airsonic | `airsonic` | Artist/album/track folders, covers and lyrics |
| Audiobookshelf | `audiobookshelf` | Audiobooks, ordered chapters, covers and numbered podcast episodes |
| Komga | `komga` | Books/editions and numbered comic issues/volumes in series folders |
| Kavita | `kavita` | Books/editions and numbered comic issues/volumes; configure the matching library type |
| Ubooquity | `ubooquity` | Book and comic folders |
| Stump | `stump` | Book and comic folders |
| PhotoPrism | `photoprism` | Basic photo/video album folders for indexing |
| Photoview | `photoview` | Basic photo/video album folders |
| Piwigo | `piwigo` | Physical album folders with ASCII filenames; bind beneath the app's galleries location |

The existing `plex` starter preset remains unchanged. Choose `plex-v1` explicitly
for the expanded, versioned profile. Existing saved layouts are never silently
upgraded or renamed. Versioned profiles reject altered naming rules; remove the
profile marker to make a custom definition. SQL/GraphQL selections may still
limit membership without changing naming rules.

```sh
catabolic --db catalog.sqlite3 catalog bind jellyfin
catabolic --db catalog.sqlite3 layout put jellyfin --preset jellyfin
catabolic --db catalog.sqlite3 layout preview jellyfin --catalog jellyfin
catabolic --db catalog.sqlite3 layout apply jellyfin --catalog jellyfin
catabolic --db catalog.sqlite3 sync --catalog jellyfin --dry-run
catabolic --db catalog.sqlite3 sync --catalog jellyfin
catabolic --db catalog.sqlite3 verify --catalog jellyfin
catabolic --db catalog.sqlite3 manifest --catalog jellyfin --in-catalog
```

This creates `./catabolic/jellyfin` by default. Bind a custom existing root with
`--root`. Configure separate application libraries at the generated media-type
subfolders (`Movies`, `TV`, `Music`, `Books`, `Comics`, `Audiobooks`, `Podcasts`,
`Photos`) as appropriate. The target app needs read access to both generated links
and their actual source targets, including within its container namespace.

## Metadata requirements and v1 limits

- Movies require item `title` and `year`. Optional item `edition` adds a label;
  Plex uses its edition syntax. Multiple copies with identical destinations are
  conflicts; select a preferred association or customize the definition.
- Episodes require a single `part_of` season and that season's single `part_of`
  series. Relationship positions supply episode and season numbers. Direct
  episode-to-series relationships need a custom rule. Subtitles require scalar
  association metadata `language`.
- Music tracks require a `part_of` album with an ordered position, and the album
  requires one `performed_by` artist. Covers belong to the album. Multi-disc
  albums need unique positions across the release or a custom layout.
- Books and book editions require item metadata `author` and `title`. Audiobooks
  use those fields on the audiobook item; chapters inherit the parent audiobook
  through `part_of`, with the chapter position used for ordering.
- Comic issues and collected volumes require a numbered `part_of` comic series.
  Issues nested inside a volume need a custom relation chain. Embedded EPUB or
  ComicInfo metadata can override filenames in reading apps.
- Photos/videos optionally use item metadata `album`; otherwise they use an
  item-ID folder. V1 supports a conservative list of common image/video suffixes.
  RAW pairing, generated thumbnails, special Piwigo derivative directories and
  application-specific face/album metadata are outside these naming profiles.
- Suffix matching is case-insensitive. Each rule lists its accepted suffixes.
  Unsupported formats and roles are counted as skipped, not silently converted.
  Missing required metadata and naming collisions block application of the plan.

MPD explicitly documents symlink traversal controls. Other targets require
application-level scan tests; a valid naming plan is not evidence that all
versions, clients, network shares or mounts resolve links correctly. Native
Catabolic manifests do not replace embedded tags or app-specific metadata.

## XSPF, OPDS and NFO exports

```sh
# JSON envelope containing generated file paths and text; no filesystem writes.
catabolic --db catalog.sqlite3 export --catalog jellyfin --format nfo

# A raw playlist document, in deterministic catalog-path order.
catabolic --db catalog.sqlite3 export --catalog music --format xspf --raw

# OPDS 2.0 needs an HTTP(S) base URL from which the media will be served.
catabolic --db catalog.sqlite3 export --catalog books --format opds \
  --base-url https://books.example/library/ --raw

# Fresh detached bundle containing media symlinks and NFOs beside the videos.
catabolic --db catalog.sqlite3 export --catalog jellyfin --format nfo \
  --output /existing/export-parent/jellyfin-with-nfo
```

XSPF uses relative URI references unless `--base-url` is supplied. OPDS 2.0 groups
multiple formats of a book into acquisition links and includes titles, authors
when supplied, and catalog-scoped identifiers. Catabolic generates the feed; an
HTTP server must serve it and the referenced media. Neither export starts a
server. XSPF exports primary audio/video entries; OPDS exports primary books and
comics. NFO exports basic movie/episode metadata, recognized external provider
IDs, and episode/season positions. It does not synthesize unsupported fields or
modify embedded tags. `--raw` supports the single XSPF/OPDS document only.

`--output` publishes a new detached snapshot: all catalog symlinks plus the
generated metadata and `catalog-manifest.json`. The manifest records the original
catalog snapshot/provenance, not ownership of the new bundle. This is not a
registered sync root. It cannot be placed inside a registered source/output tree,
overlap the database, follow symlink parents, overwrite an existing directory or
collide with a media path. Metadata is written after the links; the manifest is
written last. Publication is not atomic across the whole tree. Interrupted
publication can leave a partial directory; publish a fresh destination to retry.
Refreshes likewise use a new destination; existing bundles are never deleted.

## Import adapters

```sh
# Preview: returns selected source files and destination, without running calibre.
catabolic --db catalog.sqlite3 target import calibre --catalog books \
  --destination /existing/parent/calibre-library

# Execute via the installed official calibredb program.
catabolic --db catalog.sqlite3 target import calibre --catalog books \
  --destination /existing/parent/calibre-library --apply

# Same import backend, using the local calibre library consumed by Calibre-Web.
catabolic --db catalog.sqlite3 target import calibre-web --catalog books \
  --destination /existing/parent/calibre-library --apply

# Requires the official Immich CLI and IMMICH_API_KEY in the environment.
catabolic --db catalog.sqlite3 target import immich --catalog photos \
  --destination https://photos.example/api --apply
```

Imports use only active primary associations represented by the selected catalog.
Use query folders to narrow the selection. The default cap is 1,000 source files,
adjustable with `--limit` up to 10,000. `--timeout` bounds each external invocation
to 1–3,600 seconds (default 300). The complete selected scope is checked against
recorded size/mtime and safe source bindings before starting.

calibre imports EPUB/PDF/etc. copies through `calibredb add`, grouping all formats
of a logical item in one staging directory. Title, optional author and a scoped
Catabolic identifier are supplied. Multiple copies of the same format must be
resolved in the query selection; they are refused. Existing-book duplicate
decisions remain calibre's default behavior; Catabolic does not enable overwrite
or automatic merging by similar title. Existing databases are operated through
calibre's CLI, never through direct SQL writes from Catabolic.

Immich imports upload selected photo/video copies through `immich upload`.
This intentionally uses the upload API via its official CLI, not an external
symlink library (which Immich's external-library guide advises against). It
copies media to the server; it is not a zero-copy external-library adapter.
All inherited `IMMICH_*` options are removed except the explicitly supplied API
key and destination, preventing inherited delete/watch options from changing the
operation. Credentials are never placed in argv or returned JSON.

External programs receive temporary private copies, never original source paths.
Staging is one logical book or photo at a time and may require disk space equal
to that unit. A failed invocation stops the batch and reports completed file IDs
and the failed group. External operations are not rolled back; timeout/failure
can occur after the external application has accepted media. Review the target
before retrying. Successful invocation means the official CLI returned zero;
it does not certify subsequent server indexing or metadata matching.

## References and verification

`target show NAME` includes the relevant official project documentation URL.
Format references: [XSPF](https://www.xspf.org/spec),
[OPDS 2.0](https://specs.opds.io/opds-2.0),
[Kodi movie NFO](https://kodi.wiki/view/NFO_files/Movies),
[calibredb](https://manual.calibre-ebook.com/generated/en/calibredb.html), and
[Immich CLI](https://docs.immich.app/features/command-line-interface/).

Tests exercise all 17 layouts with real temporary symlinks, repeat sync, source
preservation, profile validation, export parsing and path collisions. Import
tests use mocked and real executable stand-ins to check staging isolation, failures and
credential/delete-option handling. These do not replace real application scans.
