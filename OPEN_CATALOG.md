# Open Catalog contract

Catabolic has a code-defined interchange contract: `catabolic.catalog-manifest`.
Current exports use format version **3**, adding explicit output modes and
hardlink/retention ownership to the tagging introduced in v2. Versions 1 and 2
remain readable, with their models and artifacts frozen unchanged. It is a project-defined catalog projection
profile, not an independently standardized format or a complete database backup.

The separate [Catabolic native layout v1](NATIVE_LAYOUT.md) specifies symlink
destinations for the `catabolic` preset. Its naming profile and version are
included in the manifest's saved layout definition. Plex and custom outputs can
produce valid manifests without following the native naming contract.

The source of truth for structural fields is
[`src/catabolic/interchange/v1.py`](src/catabolic/interchange/v1.py) and
[`src/catabolic/interchange/v2.py`](src/catabolic/interchange/v2.py), and
[`src/catabolic/interchange/v3.py`](src/catabolic/interchange/v3.py).
Pydantic types and `label(...)` annotations supply required/nullable types,
descriptions, examples, version introduction, and deprecation information.
The generator emits JSON Schema Draft 2020-12 and a Markdown field reference:

- [Frozen v3 JSON Schema](src/catabolic/interchange/releases/manifest-v3.schema.json)
- [Generated v3 field reference](src/catabolic/interchange/releases/manifest-v3.md)
- [Frozen v2 JSON Schema](src/catabolic/interchange/releases/manifest-v2.schema.json)
- [Generated v2 field reference](src/catabolic/interchange/releases/manifest-v2.md)
- [Frozen v1 JSON Schema](src/catabolic/interchange/releases/manifest-v1.schema.json)
- [Generated v1 field reference](src/catabolic/interchange/releases/manifest-v1.md)
- [Release checksums](src/catabolic/interchange/releases/index.json)

Schema IDs are `urn:catabolic:open-catalog:manifest:1` and
`urn:catabolic:open-catalog:manifest:2`, plus `urn:catabolic:open-catalog:manifest:3`. No network fetch or
schema registry is required. Pydantic is pinned so dependency upgrades cannot
silently alter generated artifacts. The independently implemented `jsonschema`
validator checks real exports and compatibility fixtures in the test suite.

## CLI and agent use

These commands do not require `--db` or access source files:

```sh
catabolic spec schema
catabolic spec schema --format-version 1
catabolic spec docs
catabolic spec check
catabolic spec validate --file catalog.json
catabolic spec roundtrip --file catalog.json > reviewed-copy.json
catabolic spec diff --against candidate.schema.json
```

`--file -` and `--against -` read stdin. All commands emit JSON except `spec docs`,
which emits Markdown; global `--json` wraps that Markdown in a JSON object.
Validation failures exit 2 with a JSON error on stderr. `spec diff` exits 0 for an
identical schema and 3 for differences, returning `review_required` and changed
JSON Pointer paths. It deliberately does not claim that an arbitrary JSON Schema
change is backward compatible. `spec check` exits 2 on generated-artifact drift.

`manifest` exports validate against the same typed contract before returning or
publishing a document. Python consumers can use `decode_document`,
`validate_document`, `document_value`, and `encode_document` from
`catabolic.interchange.validation`. The codec preserves JSON values and unknown
fields, not original whitespace, escape spelling, or object-key order.

The codec validates incoming documents; it does not merge them into a database,
copy media, restore ownership, run embedded SQL/GraphQL, or create symlinks.
Database import/merge semantics need a separate reviewed design. A manifest
contains one active projection and outgoing related items; it omits unmapped
media, disabled mappings/relationships, full scan history, journals, and other catalogs.
Version 2 does retain withdrawn tag assertions on included subjects. Its tagging
collections include the ancestor vocabulary closure and aliases; see
[TAGGING.md](TAGGING.md). It does not implicitly downgrade to v1 or discard tags.

## Semantic requirements beyond JSON Schema

These requirements are enforced by the codec and tests, rather than inferred
from field annotations alone:

1. IDs are opaque and scoped by `content.database_id`. Do not deduplicate by title
   or reinterpret an ID as a path. Provider `(namespace, value)` pairs must be
   unique within the document. External identities are hints for an eventual
   merge policy, not automatic permission to overwrite existing decisions.
2. Item, file, association, relationship, mapping, destination, and recorded-link
   identifiers must be unique within their collections. Associations and edges
   must be active and reference included records; edges cannot point to themselves.
   Each entry must list exactly the included associations for its item/file pair.
   Layout-managed entries must reference the included layout.
3. Counts must equal array lengths. Layout checksums and current/last-applied
   comparisons must agree. Recorded/expected target comparison flags must agree
   with their values. `filesystem_verified` must be false: recorded availability
   and ownership never prove live filesystem state.
4. `content_sha256` must match the complete content object, including unknown
   fields. This is an integrity check, not an authentication signature.
5. JSON object keys cannot repeat. Readers reject unsupported format versions,
   nonfinite numbers, malformed values, and type coercions such as string counts
   or numeric booleans. Rejection must not rewrite or partially import data.
6. Unknown fields at every record level, nested metadata, array order, explicit
   nulls, Unicode strings, and integer values must survive a read/re-export.
   Namespaced extension keys such as `example.org:curation` are recommended but
   not mandatory for existing metadata. Consumers unable to preserve a value
   must reject it instead of silently dropping or coercing it.

Relative file/destination paths, machine-specific roots, recorded targets, and
layout definitions remain descriptive data. Structural validation does not make
these safe to use as filesystem instructions. Catabolic's normal bind, layout,
sync, ownership, and path validation rules remain authoritative for mutations.

The local validator has a 128 MiB input/output budget and a 100,000-record limit
per collection. These resource limits do not turn a partial result into a valid
complete document. Decimal strings for byte sizes and nanosecond timestamps avoid
JavaScript integer precision loss; arbitrary metadata numbers also require a
lossless reader.

### V1 checksum encoding

V1 retains the existing Catabolic encoding, not RFC 8785. It uses UTF-8 bytes of
Python `json.dumps(content, sort_keys=True, ensure_ascii=False, allow_nan=False)`
with default separators (comma plus space, colon plus space), followed by SHA-256.
Keys sort by Unicode code point. Python's finite-number rendering is part of this
legacy profile; implementations in other languages must reproduce it, including
float exponent spelling and negative zero. Changing canonicalization requires a
new format version. A byte-order mark or trailing newline is not part of the
checksummed content. Document envelope fields are outside the content checksum.

The retained [compatibility fixture](tests/fixtures/interchange/manifest-v1.json)
and round-trip tests provide executable examples. Whitespace around input JSON
does not affect verification. Reordering arrays or adding content extensions does
change the checksum and requires an intentional recomputation.

## Versioning and generation workflow

Released schema/reference artifacts are immutable. Models, frozen artifacts,
and the checksum index are checked locally by `spec check` and in CI. CI also
compares release files against the pull-request base or previous pushed commit:
existing artifacts cannot change or disappear, and existing checksum-index
entries cannot be rewritten. Adding a new version and new index entries is
permitted. Python 3.11 and 3.14 on macOS and Linux are configured in CI.

For the next format version:

1. Define a new versioned model module and semantic validator while keeping the
   v1 decoder, models, artifacts, and compatibility fixtures available.
2. Add field descriptions/examples and introduction/deprecation annotations in
   that module. Never maintain a second hand-written field schema.
3. Generate candidate schema/reference files and review their structural diff.
   Required-field changes, renamed/removed fields, narrowed constraints, changed
   semantics, and checksum rules all need explicit compatibility review.
4. Add old/new reader fixtures and preservation tests, including unknown fields.
   Database migration versioning remains independent of document versioning.
5. Freeze the new artifacts under new filenames, append their checksums, register
   explicit version dispatch, and extend the generation/check commands.

No generator overwrites released files automatically. `spec schema` and `spec docs`
write stdout so a candidate can be reviewed before it becomes a release artifact.
The frozen v1, v2 and v3 profiles are the current contracts; broader interchange and database
merge guarantees should be added only with their own semantics and tests.

Implementation references: [Pydantic models](https://pydantic.dev/docs/validation/latest/concepts/models/)
and [JSON Schema generation](https://github.com/pydantic/pydantic/blob/main/docs/concepts/json_schema.md).

Manifest v3 encodes the output mechanism and separates hardlink ownership from
symlink targets; [HARDLINKS.md](HARDLINKS.md) defines those semantics.
