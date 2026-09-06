# Catalog manifests

`catabolic manifest` exports additional catalog information as versioned JSON.
It can write to stdout, a separate metadata file, or a reserved metadata file
inside a generated symlink folder. It works with explicit mappings, layouts,
and SQL/GraphQL query folders.

```sh
# Full JSON document on stdout, without needing --json.
catabolic --db catalog.sqlite3 manifest --catalog recent

# Standalone export; the parent directory must already exist.
catabolic --db catalog.sqlite3 manifest --catalog recent \
  --output /absolute/path/to/recent-metadata.json

# Metadata beside the generated links, after synchronization.
catabolic --db catalog.sqlite3 sync --catalog recent
catabolic --db catalog.sqlite3 verify --catalog recent
catabolic --db catalog.sqlite3 manifest --catalog recent --in-catalog \
  --extra '{"description":"Recent films","curated_by":"local agent"}'
```

`--in-catalog` writes `.catabolic-manifest.json` at the bound catalog root. This
reserved name cannot collide with a supported generated mapping. The output must
already have a matching ownership marker from `sync`; export does not claim it.
The manifest is a regular file and remains compatible with sync and verification.
An explicit `--output` inside a catalog is allowed only at that same reserved path.
`--output -` writes to stdout.

## Information included

The top-level envelope contains `format: "catabolic.catalog-manifest"`,
`format_version: 3`, generator/version, UTC `generated_at`, `content_sha256`, and
`content`. The content contains:

| Field | Information |
| --- | --- |
| `database_id`, `database_schema`, `profile` | Snapshot provenance and selected machine profile |
| `catalog` | Catalog ID, link mode and profile-specific output root, or null if unbound |
| `entries` | Every active mapping: IDs, relative/absolute output paths, associated roles through association IDs, layout ownership, expected and recorded symlink targets |
| `items` | Item kinds, complete metadata objects, provider identity namespaces/values |
| `files` | Source location, relative/absolute paths, recorded scan status, size, nanosecond mtime, scan ID and observation time |
| `associations` | Active identifications for exported file/item pairs, including role, part, origin, and complete metadata |
| `relationships` | Active outgoing relationships, including positions and metadata |
| `tags`, `tag_names`, `tag_parents` | Referenced tag vocabulary, ancestor closure, canonical names and aliases |
| `taggings` | Active and withdrawn assertions for included items/files, with source, confidence, notes and timestamps |
| `recorded_links` | Link paths/targets recorded by the synchronizer, including paths awaiting retirement |
| `hardlinks`, `retained_hardlinks` | Recorded regular-file ownership and retained data, with source occurrence and decimal device/inode IDs |
| `layout` | Managing layout name, current saved definition (including query/parameters), its hash, last-applied definition hash, and whether those hashes match |
| `extra` | User-supplied JSON object, preserved independently of catalog metadata |
| `counts` | Exact lengths of all twelve record collections |

Items include the mapped items and recursively referenced outgoing parents,
editions, and contributors. For example, a track-only folder can include album
and artist metadata without including those items' other source files. Incoming
siblings are not automatically exported. Cycles in custom relationships terminate
through ID deduplication. Every entity has stable IDs for joining the arrays.

File sizes and nanosecond timestamps use decimal strings to preserve integer
precision in JavaScript consumers; unavailable values are null. Metadata is
exported without inventing missing details or restricting custom keys. Paths,
identities, metadata, and saved query parameters are included as stored, so the
manifest is a full local metadata export, not a redacted sharing format.

`--extra` requires a JSON object. For repeatable exports, pass the same extra
fields each time. Omitting it produces an empty object; export does not merge
annotations from a previous manifest. Keep annotations in the database metadata
or in your export command rather than editing the generated file directly.

## Snapshot meaning and refresh

The manifest describes **desired catalog state** from one database snapshot.
`state` is `desired_catalog` and `filesystem_verified` is false. Status and link
targets are recorded evidence, not a new source scan or filesystem verification.
`recorded_link_matches_desired` compares stored link targets with expected targets;
it does not prove that a link currently exists or its source is healthy.
Use the separate `verify` command for that check.

The export does not rerun a saved query, apply a layout, synchronize links, read
source file contents, calculate media checksums, or fetch external metadata.
After changing membership or naming, use this explicit sequence:

```sh
catabolic --db catalog.sqlite3 layout preview recent --catalog recent
catabolic --db catalog.sqlite3 layout apply recent --catalog recent
catabolic --db catalog.sqlite3 sync --catalog recent --dry-run
catabolic --db catalog.sqlite3 sync --catalog recent
catabolic --db catalog.sqlite3 verify --catalog recent
catabolic --db catalog.sqlite3 manifest --catalog recent --in-catalog --replace
```

If a layout was edited after its last apply, `definition_matches_last_apply` is
false. Entries continue to describe the existing desired mappings; the manifest
does not claim the new definition has generated them. Historical definition text
is not reconstructed from its hash.

## Safe publication

Without a destination, export uses a read-only database snapshot. File publication
holds the existing catalog writer lock and a transaction snapshot. Pending link
operations must be recovered before export. No schema migration is needed, and
export does not modify persistent database rows.

Files are written to a new temporary file in the destination directory, flushed,
and published atomically. New destinations are created without overwriting an
existing entry. A changed export requires `--replace`, and replacement accepts
only a valid manifest belonging to the same database, profile, and catalog.
Foreign JSON, unsupported manifests, manually edited content, symlinks, hard-linked
files, and non-regular destinations are refused. Parents are opened without
following symlinks; known source-tree destinations and database state paths are
refused. Bound tree identities are also checked by inode to recognize renamed
roots and case aliases.

The checksum is SHA-256 of the content serialized with Catabolic's canonical
`encode` function: sorted keys, UTF-8, unescaped Unicode, Python JSON's default
separators, and no nonfinite numbers. It detects content edits; it is not a
cryptographic signature or a source-media checksum. An unchanged checksum causes
export to leave the existing file, timestamp, and inode untouched. Generated time
is excluded from the checksum. Files are created with mode 0600.

Existing files are rechecked immediately before replacement. As with link sync,
simultaneous external modifications of output directories are outside the
supported concurrency model. A crash before publication can leave a uniquely
named temporary `.catabolic-manifest-*` file; it never exposes a partially written
final manifest. A subsequent export can still publish the complete snapshot.

Exports are complete or fail: there is no pagination or silent truncation.
The caps are 100,000 records per collection (including related items) and 128 MiB
of compact JSON. Write commands return a JSON summary with destination, checksum,
counts, `written`, and `unchanged`; stdout export returns the full document.
Input, ownership, and export errors exit 2 with a JSON error on stderr.

This is Catabolic's generic metadata format. It is not an NFO, XMP, or other
consumer-specific sidecar, and it is not a replacement for a SQLite backup.

## Generated interchange specification

New exports use manifest v3, including output link modes, hardlink ownership,
retained data, tag vocabulary, aliases, parent edges and
active/withdrawn assertions for included subjects. Version 1 and 2 documents remain
readable. All three versions have frozen typed models, generated JSON Schemas and
field references. See [TAGGING.md](TAGGING.md) for tag export semantics. See [OPEN_CATALOG.md](OPEN_CATALOG.md) for the
contract and its preservation/versioning rules. Exports validate before output;
`catabolic spec validate --file PATH` validates documents without opening a database.

For hardlink catalogs, symlink target/comparison fields are null and
`recorded_links` is empty. Hardlink records do not claim live verification. See
[HARDLINKS.md](HARDLINKS.md) for retention and recovery semantics.
