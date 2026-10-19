# Native search storage v1

Schema35 adds derived search storage to the existing native Store transaction
owner. Schema32 defaults and schema33/34 admission remain supported. The new
schema starts not-ready and does not rewrite historical authoritative rows.
This document describes storage integration; scoped query adapters and final
qualification are separate increments.

## Canonical inputs

- Managed metadata contributes only resolved string title/description from its
  effective projection; a missing projection is corruption, never a fallback
- Unmanaged legacy items explicitly contribute string title/description with
  legacy provenance, until typed metadata initialization replaces that path
- Only current, non-null accepted pins become identity text
- Effective item tags include canonical names/aliases after local suppression
- Logical merge aliases retain complete original/intermediate/destination paths;
  splits never pick an arbitrary child and retired documents are removed

Canonical values retain field keys, provenance and required subject IDs. Local
metadata authority does not implicitly authorize tags, identity namespaces or
aliases. Public search must filter values and every alias-path subject before
MATCH, including negative queries. A global FTS prefilter could reveal hidden
text and is forbidden. History/conflict candidates and legacy identity evidence
do not become accepted search values.

## Atomic derived state

FTS5 uses `unicode61 remove_diacritics 2` and five fixed text columns. Derived
writes never advance authoritative catalog generation or entity revisions.
Source triggers capture dirty old/new subjects with a permanently empty deferred
FK barrier. Raw autocommit indexed-source writes fail; raw explicit transactions
cannot commit dirty state. Supported Store writes flush within their existing
transaction. The dispatcher flushes before receipt capacity/generation capture,
so source, FTS, generation and receipt roll back together on late failure.

An input revision check prevents derived writes from hiding source phantoms.
Vocabulary edits with no affected item do not create unflushable revisions.
Ordinary edits maintain touched documents but cannot declare a whole catalog
ready. Only a complete bounded rebuild validates FTS integrity and records the
catalog/incarnation/generation plus matching input/indexed revisions.

One cumulative operation budget covers extraction, serialization and finalizer
work. The private writer must own the actual SQLite VM handler through COMMIT;
a thread-local counter alone does not prove that an unknown public Store progress
handler may be replaced. Cancellation must not interrupt rollback cleanup.

## Compatibility and evidence

`scripts/native_catalog_search_schema.py` reproduces exact35 SQL/signature and
checks historical DDL preservation. Explicit35 identity/logical fixtures are
separate from pinned33 identity and pinned34 curation fixtures. Latest-target
upgrade assertions follow the versioned target; old source receipts do not
qualify this code. Independent schema review remains incomplete, Motion
composition is deferred, and no M0–M11 or platform gate is closed here.

## Versioned migration-test replacement

The generic native upgrade runner always plans the latest explicit native target.
Its populated schema33 upgrade and commit/crash fixtures now compare against
`NATIVE_SCHEMA_VERSION`, exercise every appended step, and bind before/after
COMMIT to the final target. The fixed `staged34` preservation/corruption probes
still execute exactly migration034 and retain its narrowly defined legacy-index
exception. This is a latest-target fixture replacement, not a rewrite of the R3
schema34 receipt or a claim that its old source qualifies35. Initial hard-coded34
failures remain in the source-bound checkpoint evidence.
