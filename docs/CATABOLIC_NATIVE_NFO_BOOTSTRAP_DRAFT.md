# NFO attribution/presence owner: unregistered schema41 proposal

Status: design only. No schema41 resource, migration, route gate or bootstrap
mutation is registered by this document. Native40 contracts remain unchanged.
The private evidence constructors are read-only descriptive proof, not authority
to publish a sidecar or resurrect an expired command result.

## Complete baseline inventory

Under the retained native writer, independently exhaust the union of subjects
in `catalog_metadata_sources` and `catalog_metadata_history` for namespace
`motion.nfo`, record `document`. Do not use caller counts, visible-library DTOs,
only live items, or only currently nonempty JSON. Retired subjects and unsupported
histories remain inventory obligations. Verify every current source and every
contiguous historical source/schema transition using immutable committed request
facts. A tombstone proves only the exact independently reconstructible effect.

The baseline families are:

1. Imported contract1/contract2: exact root and activated metadata part, immutable
   source contribution and complete retained Motion identity/origin audit. An
   origin additionally needs the separately activated physical part, exact saved
   Motion-file mapping and native imported file revision. No-origin is a positive
   audited result, not a missing mapping.
2. Native40 first expected-revision-zero contribution replacement: exact committed
   request, initial/extended schema, reconstructed source revision one and optional
   original-title reconciliation. A nonserialized complete absence audit proves
   both prior automatic identity absence and prior sidecar attribution absence.
3. Native39 preinitialized first Patch: a private exact reconstruction and real
   39-to40 migration fixtures now cover the currently HTTP-admitted cohort. Later
   Motion-shaped v1 Patch history is reconstructed before any v2 replacements.
   Historical/direct-owner InitializeAndPatch now has a separate private exact
   synthetic-child reconstruction and nine source-only owner fixtures; it is not
   interpreted as native40 replacement or claimed as current39 HTTP admission.

All baseline families carry the independent attribution audit digest into the
terminal source-history proof. Full physical origin proof remains separate.
Unknown history operations, missing source lineage or unresolved archived audits
refuse bootstrap, identifying the exact missing proof. They are migration parity
limits, not a claim that empty present metadata established absence.

## Minimal indexed state

Proposed owner tables (names not yet registered):

- `catalog_nfo_states`: item subject, current owner revision; FK to immutable
  state history. This head is retained when public presence is withdrawn.
- `catalog_nfo_state_revisions`: owner revision, native source revision, public
  incarnation/public revision or explicit absence, retained document identity,
  separately active automatic identity (retirement does not erase the former), optional
  profile/file origin, exact attribution/source proof digest, command key.
- `catalog_nfo_identity_claims`: indexed exact opaque identity to owning subject,
  tied to that subject's state revision. No normalization or inferred provider.
- `catalog_nfo_file_attributions`: indexed profile/file to subject and state
  revision; exact origin mapping proof is retained in immutable state history.
  The profile/file index is deliberately nonunique: legacy attribution and the
  refresh planner permit multiple old subjects for one file, all of which must
  be found before withdrawal. Subject remains the attribution head identity.
- One bootstrap completion record binding catalog/incarnation, exhaustive source
  inventory digest and original/current schema identities to the sole receipt.

Every owner head/index change needs generation and entity-revision coverage;
immutable history and receipt foreign keys follow existing owner conventions.
Manual `catalog_identity_pins` remain separate. Refresh reads exact current pin
heads, honors accepted manual decisions and checks global automatic/manual
collision ownership. A same-subject retained document identity X and manual pin Y
may legitimately differ; bootstrap preserves both rather than calling that a
cross-owner collision. Neither the stored document nor active automatic claim
permits refresh to replace protected work.external_ids against pin Y. The pure
manual-pin guards govern publication and resolved structural identity. Reverse
cross-owner collision guards on future Pins writes do not rewrite any prior pin,
receipt or history byte and do not normalize/reinterpret opaque v1 identities.
It must not manufacture a Pins decision from automatic NFO.

## Admission and atomicity

The first bounded implementation should exhaust, verify and publish the complete
inventory in one offline owner transaction. Each source proof keeps its existing
2 MiB/8192-node/work admission, while all sources share the embedding operation's
cumulative byte/node/work/deadline budget. There is no independent budget reset
per source and no promise that an arbitrary catalog fits. Actual captured-library
fixtures must measure accepted size and exact overflow rollback before claiming
that cohort supported. No new global cap increase is proposed.

If measured supported imports cannot fit one transaction, a later explicit
owner-enforced bootstrap write barrier and resumable exact cursor protocol is
required. Staging rows alone are neither a lock nor completeness authority. No
segmented protocol is registered speculatively by this proposal.

Finalization compares the complete native inventory again under the same writer,
requires exact proof coverage and uniqueness, commits an effect digest covering
public presence, identities, full physical origins and both source/audit proofs,
and writes all heads/indexes plus
one receipt atomically. Any missing, extra, duplicate or stale subject rolls back.
Before completion refresh remains unavailable; legacy publication never resumes.

The schema upgrade itself preserves all native40 source/history/receipt bytes.
For an already serving native40 catalog, seed public source revision from the
currently served native source revision, not the archived original Motion value.
Fresh schema41 imports need an explicit versioned import operation to retain the
original Motion public revision. Withdrawal/reappearance follows the qualified
presence model: native history remains monotonic, public presence disappears,
then returns at public revision one with a new internal incarnation.

## Required next integration evidence

Actual bootstrap must cover imported and native-created sources, preserved
native39 histories, complete no-origin and imported-origin audits, retired
subjects, exact manual/automatic identity collisions, duplicate/extra/missing
inventory, overflow rollback and restart/replay. The explicit offline40-to41
transition must preserve original frozen manifests and use positive migration
proof; it must not relax existing39-to40 identity admission.

After bootstrap, compose current sidecar descriptor/root/occurrence custody,
partial-observation outcomes, pure refresh/presence planning, metadata/schema/tags,
automatic claims and attribution in one receipt transaction. Revalidate current
file/work membership and all-library curation before effect or replay disclosure.
Motion91 source lifecycle must be independently active under the actual Motion
writer at observation admission and terminal publication, even when native
generation is unchanged. Historical receipt-only recovery does not require a
currently active source. Only actual pipeline/outer-route tests can qualify
refresh, not these proofs.

## Source-write capability boundary before registration

The private bootstrap and current-state reader are not write capabilities. Before
registering schema41, every NFO source entry point must require a native-produced,
nonserialized permit tied to the exact outer command, subject, source revision and
current typed state. Operation names, request fields, session rows and a caller's
claimed bootstrap completion do not grant that permit.

The audited entry points include generic v1 Metadata/Patch and compound
InitializeAndPatch, v2 contribution replacement and system replacement, typed
v1/v2 import, and private namespace-scoped source tag writers. The public
TagMutation API exposes suppression and vocabulary merge, not arbitrary source
assertion replacement; independent manual suppression must remain available.
Neither a direct source tag writer nor a vocabulary merge may reintroduce public
NFO assertions after document withdrawal. Logical retirement must release
the active automatic claim in its own atomic owner composition while preserving
document identity and attribution history. Reverse manual-Pins collision checks
must use the same exact opaque identity ownership and retain old receipt behavior.

Native40 wrappers and payload checks remain unchanged. The future internal
replacement entry point must consume the private permit rather than manufacture
an independently replayable inner command. Initial schema41 import may obtain a
separate pre-bootstrap permit only from the exact validated immutable import
batch; this permits retained source effects, not current serving authority.
Post-bootstrap imports require inline typed attribution composition or refusal.
These entry points are not wired yet, so this proposal remains unregistered.

Registration acceptance cases must include bootstrap followed by the first real
NFO write with exact receipt/ACK/restart; public withdrawal followed by an actual
source assertion writer or merge effect that would violate absence (refused with
no tag/source/state changes); and logical
retirement followed by another subject claiming the released automatic identity.
The last case must prove the retired subject retains its original document and
history, that an independent manual pin still prevents cross-owner reuse, and
that replay of the retirement does not release another subject's later claim.

Imported tag provenance is an additional bootstrap obligation. Native40 source
replacement retains catalog_metadata_tag_history, but typed imports establish
assertions and suppression receipts without that replacement history. Missing
replacement history is therefore not proof of missing tags, and suppression
reason JSON is not an ownership grant. The activated immutable import batch and
complete archived audit must supply the exact imported assertion/exclusion facts
before the bootstrap can authorize their later replacement. The private current
reader validates typed current heads descriptively; it does not invent this audit.
NFO replacement itself forbids nonempty excluded tags. Independent local
suppressions remain effective without removing the underlying NFO assertion from
its source document, and must survive NFO refresh unchanged.

The post36 private replay candidate consumes authenticated original tag IDs and
reconstructs recorded replacements using the same pure assertion-write plan as
the native40 source owner. Original assertion timestamps come from the exact
activation metadata-history admission time, not receipt commit time. Replay
compares each immutable before snapshot and retains inactive rows, notes,
timestamps and permanent entity counters through empty, repeated and reappearing
source writes. Tag-preserving v1 Patch advances source history without inventing
tag history. Only terminal rows are observed as a comparison, never as a seed.
Current canonical vocabulary is presentation, not proof of historical name
resolution. Original authenticated assertion IDs can survive canonical rename
and even reuse of the old spelling when no later request needs that spelling.
Recorded replacement replay additionally requires every requested alias to have
an unchanged permanent revision-one mapping. Canonical rename that retains
those aliases is permitted; rebound requested aliases are not inferred from the
current canonical row. Unexplained merges and unjournaled assertion changes
remain explicit obligations. This source-only extension is not covered
by the prior66 proof tests or captured42 untouched-closure qualification; it needs
its own owner/effect-equivalence and ordinary tag/metadata/import/migration runs.

The first private PublicPut composition targets an existing typed NFO source,
including withdrawn-state reappearance. A never-existing source is a separate
mandatory acceptance case: descriptive no-row results do not prove prior
automatic-identity and origin absence. Before registration, fresh-created and
imported subjects must obtain that proof from complete immutable creation/import
ancestry and root audit linkage, then initialize the first typed state atomically.
This is required to preserve initial writes already supported by native40.

A post37 private first-source candidate now makes that distinction explicit.
Its nonserialized absence capability binds current catalog/incarnation/generation,
subject, host, exact outer request digest, original creation digest and an
independently complete host audit. Native source/history/tag/index absence is
only a consistency check. The initial cohort proves an exact same-incarnation
Motion create-only command with empty original external identities. Unmanaged
metadata additionally requires the actual retained full creation body and the
unchanged original title baseline; a commit tombstone cannot replace that body.
Managed metadata retains its existing schema and does not invent scan provenance.
Imported-root absence and restored historical-incarnation ancestry remain
separate production-builder obligations, not inferred supported cohorts.

The first-source permit installs the deferred metadata barrier before nested
initialization. A private capability-checked initializer reuses and retains that
barrier, and the replacement owner does not reinsert or clear it. Publication
checks the exact initialized root, original legacy history, normalized source,
scan-title reconciliation when applicable, independent Pins/suppressions and the
pure first-presence plan before inserting typed revision one and releasing the
barrier last. Ordinary40 entrypoints retain their original contracts. Real
candidate tests cover post-initialization swallowed errors and actual COMMIT,
token drops, origin semantics, managed normalization, replay/reopen and refused
audit/grant/expired-result evidence. These post37 tests remain unqualified until
run against their own immutable capture; no production schema41 is registered.

For existing sources, the permit binds the original typed state and exact outer
PUT request before metadata effects. Existing metadata policy normalization and
schema extension run once under the sole transaction. The presence planner then
uses the actual normalized resulting source and tags against the original state,
retaining the document identity and origin; any mismatch or planning failure rolls
back the whole transaction. Raw request values are not assumed to be normalized,
and no independently replayable synthetic inner command is minted.

## Unregistered candidate transaction qualification

A cfg(test)-only admission helper derives the exact candidate DDL/ledger from
shipped native40 plus this draft migration. It installs under actual lifecycle,
writer and SQLite-exclusive custody, verifies every original data/ledger row,
and reopens with normal path/settings/incarnation fences. Production Store::open
and the supported-schema registry still reject41. Candidate receipt tests use
the normal receipt-first transaction engine and finalizers, with a test-only
owner callback, not manually inserted receipts.

The private existing-source PUT corridor now composes metadata normalization,
source/tag history, presence/attribution and indexes under one outer command.
It retains the existing deferred metadata commit barrier until exact typed-state
publication succeeds. Candidate fixtures exercise real COMMIT after token drop
or swallowed post-effect failure, in addition to normal dispatcher rejection.
The controlled fixture host supplies independently constructed archive/audit
grants; these tests do not qualify Motion's future archived-root audit builder.
No candidate source or test success alone registers41 or enables refresh routes.

## Imported-first and retained Motion execution boundary

The post42 private imported-first candidate uses a distinct imported-root baseline,
never an Activate envelope as create-only/title-reconciliation evidence. It checks
complete ordered root parts, permanent Activate commitments, exact original
managed metadata/source/history/projection bytes and heads, and an independently
prepared positive absence audit. The audit names a subject subset but covers the
entire root for original NFO documents, automatic identities, tags and sidecar
origins. Native current absence is only a consistency check. Original metadata
contracts1 and2 retain their original normalization and wire semantics. Manual
Pins and local suppression remain separate owners. Evolved structural histories,
restored incarnations and roots outside the existing cumulative proof bounds are
explicit remaining obligations, not silently accepted or omitted subjects.

Eight source-only fixtures captured in native49 exercise imported first PUT,
compacted activation proof, exact replay/reopen, rollback/retry, original tag and
structural-history refusal, independent Pins/suppression, and cumulative root
admission. These fixtures are not yet qualification evidence and do not establish
imported-first refresh or the complete Motion-to-native execution bridge.

The intended bridge follows the existing owned guarded execution corridor:

1. Move the actual owned Motion transaction and immutable saved intent into the
   retained host execution task. Cancellation cannot release the writer while
   the task is executing. Resolve the exact native receipt before requiring any
   archive, sidecar or first-source evidence. Expired results remain expired.
2. Only after an absent receipt, prepare the complete archive, authorization and
   filesystem observations under that owned writer. Create borrowed root and
   sidecar views lexically inside the task, never move borrowed leases across a
   thread boundary or manufacture a self-referential owned capability bundle.
3. Native execution repeats receipt-first resolution and generation checks under
   its own transaction. Earlier receipt absence is not execution authority. The
   native owner reconstructs private first-source capabilities from exact root
   references and current heads; a descriptive DTO is never a capability.
4. Terminal native callbacks only revalidate retained temporal/descriptor facts.
   They perform no SQLx calls, nested host calls or new filesystem selection.
   Drop lexical views after native completion, then ACK and commit using the
   still-owned Motion transaction. Preparation and ACK may use the established
   outer retained-owner async bridge, never reenter it from a native callback.

One borrowed complete root is shared across requested subjects. Any eventual
shared-root admission optimization must preserve cumulative work/byte/node
charging; it cannot reset a per-subject budget to make an oversized root fit.
Production schema41 admission, command dispatch and refresh gates remain closed.

### Connected writer and retirement prerequisites

The live candidate now includes a distinct deferred source-effect barrier for
NFO source/history/assertion writes. Generic metadata or tag grants cannot mint
that capability. Validated fresh import obtains a separate exact batch-bound
capability before bootstrap; post-bootstrap NFO imports still require composition
and are refused. Independent local suppression and ordinary non-NFO external
metadata/schema evolution remain separate. Generic protected `motion_catalog`
changes and specialized system replacement remain explicit outstanding projection
composition obligations, not completed parity.

Logical retirement uses the reviewed owner's existing deferred barrier to release
only active automatic claims and file attribution. It preserves the source,
public document presence, retained identity/history, manual Pins and suppressions.
Five new source-only retirement fixtures and eight source-effect/import/item-put
fixtures exercise these paths. They are unqualified until executed. Controlled
candidate import tests do not establish production schema41 worker-inventory or
observation-cleanup admission.

The offline transition must distinguish closed40 evidence, exact41 migration,
bootstrap pending, and bootstrap complete. Normal40 descriptive inventory is not
an executable41 plan. Actual effect and inventory digests must be prepared again
under post-migration custody and the new generation, with ordinary startup and
source writers barred until bootstrap completes. Recovery resolves the exact
saved receipt before reconstructing effect evidence. No read API upgrades a
catalog or substitutes a stale pre-migration generation.

### Terminal filesystem validation boundary

The candidate refresh path validates retained occurrence data with an explicit
expected ctime through the same descriptor validator used by ordinary source
reads. It retains both root/path walks and before/held/named revision checks,
without cloning the complete occurrence or returning an otherwise unused stat
object. Admission covers the inspected fields, requested path-component storage,
component opens and internal stat representations before filesystem use.

Linux refresh requiring a non-null volume UUID is temporarily refused before
I/O. The existing general-purpose UUID probe collects subprocess output before
checking its size, so it cannot establish bounded candidate refresh custody.
This is an explicit incomplete cohort, not permission to skip UUID checks.
Reopening it requires an owned, cancellable, output-bounded probe and actual
positive UUID, oversized-output, cancellation and replacement tests. Ordinary
source APIs retain their existing behavior. The bounded macOS UUID lookup is
separately charged for both fixed-size UUID strings.
