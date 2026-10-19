# Private scoped cursor, search, and writer boundary

This increment builds on schema35 derived storage. It does not close R4 or any
cross-application/platform gate, qualify external pinned relevance, or replace
independent schema/definition/filesystem safety reviews.

## Admission and continuations

`ScopedReadSession` owns a newly opened, unescaped Store. Trusted host selection
references resolve complete membership in a fresh snapshot. Request JSON cannot
mint a `ScopeHandle`, metadata mask, relation privilege, or write grant. The host
lease is held through delivery, including unwinding; revocation closes new
admission and waits for admitted delivery/commit.

Item pages use authenticated binary-ID keyset cursors rather than offsets. Search
uses descending local score and binary-ID ties. Per-admission random secrets and
capability identities prevent cross-session/capability reuse. Bindings include
schema/contract, catalog/incarnation/generation, host revision, profile, approved
root/dependency revisions, field/search masks and exact query/limit. A separately
computed membership-and-mask digest catches membership changes not covered by
catalog generation. Connection data-version changes conservatively invalidate
old handles, including receipt-only changes. A final fresh-state check occurs
after closing the read snapshot. Result delivery retains its admission lease.

## Real scoped FTS

`TrustedSelection::with_search_fields` supplies explicit accepted-identity
namespaces, effective-tag matching and logical-alias matching. Metadata visibility
does not imply any of these. These privileges authorize matching, not public
history, provenance, file paths, relationship traversal, or mutation.

Only a ready, identity-matching schema35-derived index is searched (including
the unchanged index carried through the additive schema36 migration). The private query owner
reads bounded canonical values by admitted field keys. Every alias path subject,
including historical source, intermediate nodes and destination, must be visible
and alias-authorized. Split sources are never redirected to an arbitrary child.
The owner constructs request-local FTS5 only after these checks. It never uses the
global FTS index as a prefilter, including for negative queries.

Matching uses SQLite FTS5 `unicode61 remove_diacritics 2`, including phrase,
prefix, boolean and column-filter semantics. Invalid syntax is an explicit error;
there is no substring fallback. Authorized multivalues are sorted/deduplicated and
newline-joined, so v1 phrases may span adjacent authorized values.

Fixed document-local boolean phrase-hit weights are identity 120, title 100,
alias 80, effective tag 40 and description 10. `highlight(...) != original`
inside SQLite supplies real per-column hits even for cross-field conjunctions.
There is no BM25, global corpus statistic, term-frequency or length input. Totals
come from the complete bounded scoped match set before pagination. Weights remain
provisional pending external pinned-case relevance qualification.

The search owner installs an actual per-opcode VM callback only on its private
connection and only after saved-selection evaluation has returned its owned hook.
It clears its callback before rollback. SQLite can auto-abort TEMP FTS population
on interruption; cleanup recognizes that state and preserves the typed original
budget/cancellation error. Source bytes, values, IDs, results and work share the
same operation budget. No partial/truncated match set becomes a successful page.

## Private writes

`PrivateCatalogWriter` opens its own unescaped Store. Local metadata write admission,
index-maintenance admission and receipt reconciliation are distinct trusted types.
Read/search handles cannot become mutation authority. Fixed dispatcher admission
rechecks membership/fences inside its transaction before new mutation; replay
uses independently authorized receipt visibility.

The writer owns its real SQLite VM callback and lock-wait policy through the
central transaction boundary. It does not install/replace a callback on an
arbitrary public Store. Source work, derived-index flush, result serialization,
receipt insertion and prepared delivery share one cumulative budget. Any failure
before commit rolls back source/projection/index/generation/receipt together.
Rollback cleanup ignores exhausted cancellation while remaining bounded on locks.

A durable commit must never be relabeled as cancellation by fallible post-commit
SQL or output preparation. Output is prepared before COMMIT; the final fixed
commit phase uses a conservatively prepaid 64-opcode/parser-callback allowance.
Ordinary work uses a per-opcode callback. The final pre-COMMIT check is the
cancellation-admission point; after admission, newly raised cancellation cannot
replace confirmed durable success. Lock waits still use the same deadline.
FTS5 pending term/totals flushing runs explicitly before that phase under the
ordinary budget. An unexpected fixed-program bound overrun retires the Store
without erasing a successful commit. This is qualified only for the explicitly
supported schema34/35/36/37 finalizer versions and pinned bundled SQLite, not future
schema versions. Every private-writer Store access rechecks the exact admitted
user_version; schema38 and later are refused until separately qualified. It does
not claim hard interruption of filesystem synchronization or a hard wall-clock
bound on B-tree C loops or post-commit passive WAL checkpoints.

Schema36 qualification uses actual populated35→36 native upgrades, not a changed
PRAGMA version or freshly fabricated schema. The frozen migration only adds the
guarded `native_execution_lifetimes` table and its statement-time validation,
entity-revision and generation triggers. It adds no virtual table, FTS pending
writer, deferred foreign key, commit hook or other commit-time finalizer. The
existing explicit FTS flush and deferred-FK counter check therefore remain the
qualified finalizer contract. Both the exact schema signature and the immutable
35-row history prefix are verified after every upgrade fixture, along with old
row preservation and the durable backup. The lifetime table starts empty; no
worker cleanup evidence is fabricated by the migration.

The schema36 writer matrix repeats the 24 executable private-writer cases,
including the 128-document/8192-term FTS rebuild, real per-opcode budget exhaustion,
receipt-stage rollback, shared-lock deadline before durability, unclosed deferred
FK refusal, cancellation after fixed-COMMIT admission, and the exact gate lease
through COMMIT and delivery. The original schema35 matrix is retained. Additional
schema36 read/search fixtures migrate populated items, saved definitions,
relationships, pins, tags, logical aliases and a ready FTS index before checking
masked detail/count/graph output, hidden paths, local rank, opaque cursors,
cancellation cleanup and invalidation. These are implementation regressions, not
independent schema/filesystem/definition review or worker-lifetime gate clearance.
The immutable slice03/04/05 receipts preserve those actual35→36 runs. Once
latest advances to37, the current schema36 writer matrix is explicitly a fresh
fixed-version control; it does not claim to repeat the historical upgrade. The
current upgrade helper and test names pin actual36→37 and deliberately fail on
a later target. No37 execution is relabeled as36 evidence.

Pinned libsqlite3-sys 0.38.2 SQLite source shows why this distinction matters:
progress counters are maintained per Vdbe statement (sqlite3.c 97357–97360),
OP_AutoCommit calls sqlite3VdbeHalt before jumping to vdbe_return
(100517–100529), and vdbe_return can call progress again after durability
(105815–105821). COMMIT compilation emits its fixed AutoCommit program
(131683–131698). FTS5 xSync may otherwise flush pending terms during COMMIT
(262259/265525); the fixed owner invokes its pinned flush command first.
Public legacy Store search maintenance retains its existing hook and only its
existing step checks; it does not gain private VM ownership by a thread-local
budget alone.

## Explicit directed relationship reads

`TrustedRelationshipRead` is a separate trusted-host mask, keyed by the existing
relationship vocabulary. Each kind has its own relationship metadata field mask.
A selection may attach it with `with_relationships`; neither item metadata nor
search privileges imply relationship authority. Membership/cursor fingerprints
include this entire per-kind mask.

The private materializer reuses current `item_relationships` and live-item state
inside the same admitted snapshot. Only active edges with both endpoints visible
and independently authorized for that exact kind enter the sanitized TEMP graph.
Relationship metadata is restricted to the intersection of endpoint masks for
that kind. Endpoint titles are computed from each item's already masked metadata.
Retired identifiers are never redirected to survivors. Disabled edges/history
are not included in this active-graph contract.

`RelationshipsPage` narrows to an admitted anchor and uses authenticated binary
relationship-ID keysets, exact anchor/limit/capability binding and scoped totals.
Hidden, missing and unprivileged anchors share not-found-or-not-visible. Approved
saved SQL may compute counts/facets or recursive paths using only this sanitized
graph; every traversed edge therefore has both endpoints authorized. Main-table,
persistent FTS/shadow and schema bypasses remain denied. No client-supplied query
or second query language is introduced.

Items and relationships now share a private per-opcode materialization budget;
fanout, source bytes, masks, public query work and output consume the same
operation budget. Exhaustion/cancellation rolls the TEMP graph back without a
partial successful page. The cumulative ID budget also bounds relationship
fanout, so dense graphs can require a larger explicit read budget.

## Remaining scoped surfaces

Masked item detail, explicitly admitted memberships/current field provenance,
active directed relationships, approved SQL item/relationship
counts and metadata facets, scoped search and continuations are covered by these
narrow APIs. Broad public GraphQL, content/filesystem access, public history/opaque evidence, and dedicated
accepted-pin/logical-alias-resolution result adapters are still denied. Search
matching grants do not authorize those outputs. External pinned-case relevance,
restore/cross-process/platform qualification and independent reviews remain
separate; these slices do not close R4 or a release gate.

Deferred foreign-key work is not waived: this SQLite version maintains counters
in source mutation VMs and checks their sum at commit (sqlite3.c 91044–91047).
The owned busy callback is used for lock acquisition before commit phases;
the default post-commit passive WAL checkpoint does not invoke it. The 64-work
bound covers fixed VM/parser callbacks, not B-tree C loops, fsync, storage failure
latency or a passive checkpoint. After COMMIT admission, those non-preemptible
steps can exceed the wall-clock deadline. No hard real-time I/O or crash gate is
claimed by these tests.


## Admitted memberships and current metadata provenance

`with_membership_read` explicitly permits positive membership labels for that
saved selection. `GetMemberships` returns only matching selection IDs/names
actually present in the exact capability; dependency roots, other saved queries,
query definitions and output catalogs are never inferred as libraries. The
per-item membership-label set is part of the membership fingerprint. No metadata,
search or relationship grant implies this purpose.

`TrustedProvenanceRead` separately permits current value/conflict/source-revision
provenance for explicit metadata fields. `GetMetadataProvenance` uses the existing
metadata source decoder and pure resolver on the same owned read snapshot. The
resolver still considers the complete source set; no authorized source subset is
silently resolved into a different winner. Output includes only permitted fields
and current contributions containing one of those fields, including shadowed
contributions. Hidden-only contributions do not affect the scoped total. Whole
schema fingerprints, whole-source input lists, raw mutation history and opaque
source evidence are not returned because they are not field-scoped. Uninitialized
visible subjects return the explicit `MetadataNotInitialized` outcome; hidden or
missing subjects remain indistinguishable.

The new crate-private snapshot seam does not loosen the existing public metadata
reader's ambient-transaction refusal. It requires an existing owned transaction,
checks catalog/incarnation/generation before and after resolution, bounds raw
policy/source bytes before decode, and charges the operation-wide budget. Both
adapters use the private actual per-opcode VM guard, the normal fresh-snapshot
membership revalidation, and the same exact HostAuthorization delivery lease.
Neither adapter exposes new SQL tables or reads media paths from the filesystem.

Output accounting is cumulative with raw-source accounting. Focused fixtures
cancel only after serialized output is charged, exhaust the output-byte allowance
after source decode succeeds, and commit a WAL mutation after either adapter has
prepared its value but before the outer freshness/delivery check. None exposes a
partial or stale success. Adding a hidden-only contribution leaves every visible
provenance field, contribution and scoped count unchanged; the response has no
content/schema/input fingerprint. Its `scope` token is the existing fresh opaque
authorization capability, not a content-derived hash.


## Explicit whole-physical-file facts

`TrustedFileFactsRead::whole_file(roles, fields)` is deliberate permission for
whole-file stored facts. Its per-role `FileFactField` enum is independent of item
metadata, memberships, provenance, search and relationships. `GetFileFacts` only
follows exact active item/file associations for the requested visible live item.
The complete bounded response has an exact authorized-association count; it does
not silently truncate. It includes no inferred output-catalog membership.

Each record is labeled `physical_scope: whole_file_recorded`. A visible episode
may cover only one interval of a file containing hidden content. Item-only scope
therefore confers no physical facts: trusted host code must deliberately grant
the whole-file fields. Size/probe duration describe that physical file, never the
episode interval. A real typed fixture binds visible/hidden items to separate
intervals, verifies this explicit permission boundary, and proves the existing
legacy/interval-effect refusal remains intact after reading the facts.

The closed field set covers recorded size/mtime/status/revision, hash/verify
digests, and probe format name/duration/size. Only requested scalar data leaves
the existing stored-fact decoder. Status/currentness accompany a requested
processing fact; recorded-revision fingerprints require the explicit
RecordedRevision field. No paths, whole probe objects/streams/tags, raw fact
snapshots, job IDs, text bodies or association metadata are returned. Exact
recorded currentness uses the existing native component-revision owner including
source policy and volume identity, rather than the legacy relaxed currentness
flag. It reports recorded evidence only and does not inspect live media paths or
prove content availability.

Raw inventory/fact strings are bounded and charged before owner decoding. The
same private VM, cumulative source/output budget, cancellation, fresh snapshot
and exact host delivery lease used by the other adapters remain in force. Hidden
associations and unrequested processing operations cannot change visible facts
or counts. Component occurrence projection uses the independent purpose below.


## Schema37 private writer and read qualification

The frozen37 migration appends `native_observation_companions`, its partial claim
index and statement-time guards/entity-revision/generation triggers. It adds no
foreign key, virtual table, FTS writer or COMMIT hook. The explicit supported
finalizer set extends through37 only after actual populated36→37 owner upgrades,
full schema validation, immutable36-row history-prefix checks, data preservation
and backup checks. The new table is empty immediately after migration; no cleanup
witness is backfilled. Existing FTS preflush and deferred-FK refusal remain.

The writer matrix retains35 and fresh36 controls, adds a34 metadata/replay
control without FTS, and repeats all24 executable cases after populated36→37.
An additional actual private-VM fixture inserts128 guarded companion reservations
and verifies their generation/entity-revision work completes before prepaid
COMMIT; the COMMIT callbacks remain within the same64 allowance. These are unit
fixture reservations only, never claimed child-exit or cleanup witnesses.

Current37 read regressions migrate populated36 catalogs containing saved
selections, metadata provenance, relations, physical facts, logical aliases,
pins, tags and ready FTS. They recheck masked counts/paths, fixed local ranking,
cursors, hidden-source isolation, cancellation, stale delivery and exact gate
ownership. Source fixtures and test names say36→37 explicitly; the frozen old36
receipts stay historical evidence. Production grant and lease signatures do not
change. No ordinary cancellation result replaces confirmed durable success.
This qualification does not prove arbitrary helper cleanup, shared-scan recovery,
import activation, hard I/O deadlines or any blocked independent review.


## Explicit current whole-component occurrences

`TrustedComponentsRead::whole_occurrences(roles, kinds, fields)` is a separate
trusted Rust purpose. Roles use the existing association-role validator; the
closed kind vocabulary is video/audio/subtitle/attachment. `GetComponents(item)`
uses the owned detail snapshot, actual per-opcode VM guard, cumulative raw/work/
output budget, fresh delivery check and exact host authorization lease. Metadata,
search and file-fact grants do not imply this purpose. Role/kind/field masks are
included in membership and capability fingerprints and never cross-unioned.

Only exact active item/file associations whose occurrence item/file IDs match
that association and whose item is live and admitted enter the adapter. Its
complete bounded result excludes historical/noncurrent occurrences. Root and
response labels say `physical_scope: whole_occurrence_recorded`: these are stored
whole-physical-occurrence facts even when the admitted item is only an episode
slice. This is no statement of interval playability or permission for effects.
Typed slice fixtures retain the existing legacy/interval-effect refusal.

Normalization and recorded currentness remain owned by the frozen
`catalog_component_occurrences` SQL, `component_mutation::get`/`decode_occurrence`
and `components::occurrence`/`revision`. This adapter exposes neither another
resolver nor SQL tables. Output contains explicit selected scalar fields plus
admitted occurrence/association identity. Logical component identity and recorded
revision fingerprints each need their own selected field. Paths, locators,
snapshots, raw technical objects, dependencies, compatibility/playback plans,
provenance and raw history are not returned; no media filesystem is opened.

Before the frozen query's inherited `LIMIT 1` projection runs, every matching
`component_output_lineage → processing_artifacts → component_job_inputs` path is
checked, including nonwinning paths. Each retained occurrence must match its
canonical referenced occurrence's IDs, profile, item, file, association, kind and
revision; each source and intermediate must independently pass live association,
role, kind and currentness admission. Hidden/missing/malformed paths, unsupported
lineage relationships, cycles and depth above 32 exclude the root and its count.
Selected scalar fields intersect every endpoint's mask across every path. The
source query continues to decide values, including conflicts; this gate never
resolves a restricted subset into a different winner.

Relevant snapshots, association evidence, locators, job results, file-fact data,
assertion payloads and retained lineage evidence are bounded and charged before
JSON/normalizer decoding. Row/depth and operation limits refuse incomplete results.
Fixtures cover populated schema36→37 data (the original slice07 preserves its
actual35→36 receipt), useful inherited output, hidden-only
unrelated giant/malformed inputs, hidden endpoints/nonwinning paths, currentness,
actual VM exhaustion, byte/output cancellation, WAL stale-output rejection and
host-gate ownership through delivery. These are targeted implementation checks;
independent schema/definition/filesystem reviews and release gates remain blocked.

`recorded_current` describes recorded revision/current-owner state, not live
filesystem availability, conflict-free compatibility or extraction readiness.
Selected conflicted scalar fields retain the existing resolver's null/unknown
behavior. `technically_verified`, when granted, likewise keeps its existing
stored-evidence meaning and confers no playback or effect permission.
