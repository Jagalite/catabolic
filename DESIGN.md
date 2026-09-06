# Catabolic design

Status: the explicit-identification core and CLI are implemented in Python.
Schema 6 adds persisted proposal review and optional enrichment jobs; see
[ENRICHMENT.md](ENRICHMENT.md) for current processing and integration boundaries.

Catabolic inventories media and projects curated catalogs as symbolic links. It
owns its implementation and storage format. The 4dlink skill is a requirements
reference, not a dependency or a specification to copy unquestioningly.

## Ownership

The application core owns use cases and validation. The CLI parses input and
renders results. A SQLite repository owns transactions and migrations. A small
filesystem adapter owns traversal and catalog mutations. Media identification
produces proposals; it has no filesystem or database mutation authority.

Source media is always read-only. Generated catalogs contain links, not copies.
The database contains desired catalog state; filesystem observations determine
whether that state can safely be applied.

## Domain

- Location: a stable identity for a source tree.
- Profile: machine-specific source and catalog root bindings.
- File occurrence: a location and relative path, observations, and scan status.
- Media item: a logical identity, optional authoritative identity keys, and
  validated descriptive metadata. Multiple occurrences may represent one item.
- Catalog: a named projection; the default catalog is `global`.
- Mapping: a selected occurrence and a unique relative destination in a catalog.
- Proposal: evidence and a suggested identity/destination awaiting acceptance.
- Scan: a generation with explicit completeness and traversal errors.
- Owned link: the last link target created by this database for a profile.
- Operation journal: recoverable intent for filesystem changes, since SQLite
  and the filesystem do not share an atomic transaction.

Absolute paths belong to profile bindings, never logical item identity. Paths
must be relative, traversal-free, and representable without lossy conversion.
Root registration rejects overlap between sources, outputs, and database state.

## Inventory

Scanning never follows symlinks. Observations are staged by scan generation.
A complete scan publishes its observations and marks unseen occurrences missing
in one transaction. An incomplete scan reports errors and cannot infer absence.
An inaccessible or changed root is a location failure, not an empty inventory.
Root checks include filesystem identity; a readable mount-point directory alone
does not establish that the expected source is mounted.

File listings use stable cursor pagination. Filtering by catalog is explicit.
Content hashing is optional and separate from initial metadata discovery.

## Decisions

An identification result contains candidate identities, evidence, and a reason
for acceptance or review. Confidence alone is not an authorization mechanism.
Acceptance records the decision and desired mapping transactionally. Repeating
an accepted decision is idempotent. Conflicting authoritative identities and
destination collisions are errors, never silent overwrites.

The first implementation accepts explicit identities and destinations. Provider
research, naming policies, and a review queue build on the same acceptance use
case. Automated identification requires separate provider selection and matching
acceptance criteria.

## Queries

`query.py` owns read-only file, item, mapping, and item-detail queries. Application
use cases expose them to the CLI. Query parameters are bound SQL values; sort
expressions come from fixed allowlists. Unicode substring search and typed
top-level metadata matching use connection-local Python SQLite functions, so they
do not depend on an optional SQLite JSON or full-text extension.

Queries use the selected profile's recorded observations and bindings without
touching media roots. Item detail joins identities, file associations, catalog
destinations, and recorded link ownership; these records do not establish live
filesystem health. Schema 3 associates files with items independently through
`item_files`; an unidentified-file query means no active identification. Disabling
a catalog placement retains identification.

Listings use keyset pagination with an ID tie-breaker. Cursor context includes
database identity, profile, query type, filters, and ordering. Each command reads
one consistent snapshot; cursors do not freeze data across separate commands.
Items, mappings, associations, and relationships share the bounded page contract
with files. Item detail returns independent cursors for each collection.

`sql_query.py` exposes a separate bounded SQL execution path for CLI users and
agents. It opens a dedicated read-only store, installs documented temporary
views, enables SQLite query-only mode, and uses an authorizer to allow reads and
known computational functions. It refuses transaction/schema control and external
database or extension access. Named scalar bindings carry agent-supplied values;
`:profile` is reserved for the selected profile, with no implicit SQL filtering.
Execution deadlines and output caps prevent unbounded ordinary query results.
JSON column arrays preserve duplicate aliases, and truncation has a distinct exit
code. `query --schema` provides discoverability without a separate tool adapter.

## Reconciliation

Preview is read-only: it does not scan, migrate, record decisions, or create
directories. Scanning is a separate operation that updates inventory.

Apply acquires exclusive ownership of a run, reads current desired state, and
revalidates source roots and output ownership. It calculates a fresh delta;
previous previews are explanations, not executable filesystem instructions.

Bindings currently require existing directories. Only an empty output can be claimed. Its ownership marker binds the
database, profile, and catalog. Unknown entries and externally changed owned
paths block conflicting mutations. Parent directories must not be symlinks.
Filesystem operations must use directory-relative, no-follow primitives where
available to avoid a check-then-use path substitution vulnerability.

Correct links are untouched. Creation never overwrites an existing entry.
Same-path replacement requires matching ownership and uses atomic replacement.
Creates precede removals. Removal requires that the current entry is still the
owned symlink with its expected target. Relative link targets are calculated
from the destination parent to the selected source binding.

A missing source without a complete confirming scan blocks application. A
confirmed missing, empty, or incomplete source is excluded from desired links;
only its owned link can be removed, while the mapping remains for restoration.
An unavailable source root blocks application even if older scans succeeded.

Every mutation has durable intent before filesystem work. Recovery checks actual
filesystem state against intent; it never assumes a failed command changed
nothing. A run can make partial progress, which is reported and recoverable.
Verification independently inspects output ownership, link targets, and source
availability instead of treating a successful apply as proof of health.

## Schema evolution

Schema changes are immutable numbered SQL resources, bundled with the application.
`migration.py` owns their ordering, checksums, backup verification, rehearsal, and
the single transaction covering all pending steps. `database_io.py` shares SQLite
connections and writer locking with normal application operations. `store.py`
rejects incompatible versions without silently upgrading them.

The initial migration freezes schema 1. Schema 2 adds an applied-migration ledger;
the legacy baseline is adopted only after checking its known structure. Schema 3
adds file roles, optional part ordering, and typed item relationships. Historical
file/item pairs are backfilled from all mappings, including disabled decisions. Upgrades
preserve the database UUID, existing columns and values, and filesystem ownership.
All profiles must have an empty link-operation journal before an upgrade starts.
Read-only status and preview do not take writer locks or create backups.

For now, migrations may add tables or columns but cannot discard or transform
existing data. A future transformation requires explicit preservation rules and
fixture-based validation before extending this constraint. There is no automatic
downgrade or restore. See [MIGRATIONS.md](MIGRATIONS.md) for authoring and recovery.

## Delivery sequence

1. Core types, SQLite schema, explicit initialization, profiles, and bindings.
2. Safe scans and paginated inventory.
3. Explicit media identities and idempotent catalog decisions.
4. Read-only reconciliation, owned link application, recovery, and verification.
5. CLI exposing these application operations, with JSON and readable output.
6. Declarative naming layouts and typed read-only GraphQL queries.
7. Identification providers and persisted proposal review.
8. Batch automation with explicit acceptance policy and resumable progress.

The CLI can be developed alongside each use case; business rules remain in the
application core. The first complete milestone is a manually identified file
projected to a verified catalog, with safe repeat execution and crash recovery.

The first milestone is implemented. Runtime modules are `domain.py` (validation),
`store.py` (repository and transactions), `migration.py` (schema upgrades),
`database_io.py` (connections and writer locks), `app.py` (catalog and inventory use cases),
`filesystem.py` (directory-relative operations), `reconcile.py` (preview, apply,
recovery, independent verification), `layouts.py` (declarative projection plans),
`graphql_query.py` (bounded read-only queries using graphql-core), and `cli.py`
(terminal adapter). Other runtime features use the standard library. See README for supported concurrency assumptions
and performance limits.

## Acceptance evidence

Use temporary fixture trees and databases. Test source immutability, complete
versus incomplete scans, changed/unavailable roots, identity conflicts,
pagination, repeated decisions, traversal attempts, output ownership, symlink
parents, external changes, interrupted application, missing-source restoration,
and repeat synchronization preserving unchanged links. Add a subprocess test
for the public CLI workflow. Real media libraries are not test fixtures.

## Multiple media forms

`media.py` defines a shared vocabulary for media kinds, file roles, and directed
relationships, including namespaced custom extensions. `media_catalog.py` owns
transactional identification and relationship mutations. File identification is
independent of output projection. Legacy mapping creation still identifies a file
when needed, in the same transaction. Removing a catalog mapping does not remove
identity; removing the final identification while a mapping is active is blocked.

Ordered parts belong to an item and file role. Ordered child relationships belong
to a parent. Active positions are unique in those scopes. Structural relations
are checked for cycles across part_of, edition_of, derived_from, and recording_of.
Self-relations are refused, and built-in relationships validate endpoint kinds.
Custom relationships retain caller-defined semantics; they are not automatically
classified as structural. Metadata accepts additional JSON fields unchanged.

Source scanning, safe relative links, recovery, and verification are shared by all
media kinds. Catalog destinations can be explicit or generated by declarative
layouts; see OUTPUT_LAYOUTS.md. Automatic provider research and media probing
remain future features. Mixed-media fixtures exercise film/subtitles, albums/artwork,
alternate ebook formats, audiobook chapters, podcasts, comics, and photos.

## Layout and GraphQL boundaries

Layout definitions and mapping ownership use versioned, namespaced meta records.
No schema migration is needed. Preview validates complete generated/explicit scopes;
apply replans inside the writer transaction and updates only generated mappings.
Filesystem work stays in the existing reconciler and journal. Definitions are
shared across profiles, and source availability never silently selects a winner.

GraphQL uses one read-only Store snapshot and the existing CatalogQuery filters
and cursors. Its public schema is independent of the persistence schema. Runtime
budgets cover nested lookups and introspection. No HTTP server, mutation resolvers,
or caller-provided SQL is part of the GraphQL adapter. See GRAPHQL.md.

## Saved query selections

`selection.py` selects stable catalog IDs using the existing SQL/GraphQL engines
on the planner's Store snapshot. Internal engine calls temporarily enforce
read-only query execution and restore connection settings before the mapping
transaction continues. GraphQL selection uses a constrained pageable ID contract
and consumes all pages. Query errors, truncation, and unknown IDs abort before
mapping writes. Selection definitions carry a fixed query profile; the machine
profile used for synchronization does not silently alter shared desired mappings.
An explicit empty-result override is required to retire all generated mappings.
See QUERY_FOLDERS.md for authoring, limits, and refresh semantics.

## Manifest exports

`manifest.py` builds a complete metadata snapshot of active catalog mappings,
matching file identifications, and outgoing related items. It distinguishes
desired state from recorded filesystem evidence and keeps manifest format
versioning independent of database schema versioning. Publication uses a writer
lock, snapshot transaction, no-follow directory descriptors, content validation,
and atomic file publication. Catalog-local exports require an existing matching
root marker and use a reserved filename. Manifests are explicitly refreshed
metadata artifacts; they do not participate in the link-operation journal.
