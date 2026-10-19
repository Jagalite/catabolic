# Native catalog design and implementation plan

**Decision date:** 2026-10-13. **Working branch:** `codex/rust-migration-m4-plus`.
**Status:** approved direction; the contracts and work packages below are the
implementation target, not implemented capability or passing acceptance evidence.

Continue the Rust migration with a focused catalog redesign now. Keep the existing
store, scanner, query engine, execution owners and media safety machinery. Add the
identity, provenance, mutation and embedding contracts needed for Catabolic to be
a reusable catalog authority. Provide an explicit upgrade for existing catalogs
and a separate resumable import for Motion. Finish the thin Python wrapper last.

This plan supersedes the sequencing rule “redesign only after parity” and the
schema-32 freeze in [the original migration plan](RUST_MIGRATION_PLAN.md).
Unchanged interfaces still require compatibility evidence. Deliberate changes need
versioned contracts and migration evidence; they must not be counted as Python
parity. Existing safety, recovery, platform and release gates remain in force.

## Baseline and scope

The reviewed Catabolic baseline is `e24cda655ba62654c1754d7607ad7e922325b96d`.
The Motion requirements are preserved [verbatim in this repository](requirements/MOTION_CATALOG_REQUIREMENTS_2026_10_13.md),
including their response amendments and pinned Motion baseline
`1e5a24675b059337b34e98f86b225178798cc080`. The source was
`/Volumes/seed1/Projects/motion-catabolic-backend/docs/CATABOLIC_CATALOG_REQUIREMENTS.md`;
its snapshot SHA-256 is
`90bde29d00f7938cdd2b6444d90b328f63a10eea4f8c9d552d37e2ee9b94a47d`.
Later requirement changes require a recorded plan delta, not silent replacement
of this snapshot.

Existing native code supplies most infrastructure, but these semantics need work:

| Existing owner | Extension or qualification needed |
| --- | --- |
| `Store`, `embedding::Session`, runtime entrypoints | Supported host lifecycle, stable errors and a combined Motion dependency graph |
| Items, relationships, parts and representation groups | Enforced stable timelines and complete equivalent versions; interval bindings |
| Proposals and tag assertions | Revisioned metadata contributions, local suppression and protected identity pins |
| Transactions and specialized idempotent demands | Reviewed merge/split and durable generic command results |
| Authorized SQL/GraphQL evaluations | Trusted-host scope admission and revocation contract |
| Fallback epochs and request/job events | Complete catalog generation coverage, restore fencing and ranked search |
| Native migration, scans and processing | Resumable Motion import and accounting for continuing worker lifetimes |

The [macOS qualification record](RUST_MIGRATION_MACOS_RESUMED_2026_10_13.md)
remains the evidence for its exact source snapshots. A later focused pass does not
turn a failed full aggregate into a passing one. This design closes no M0–M11 gate.

## Ownership and supported library boundary

Catabolic owns source registration, observations, file revisions, availability,
logical catalog identity, contributions, accepted identity decisions, tags,
relationships, saved selections, catalog search and catalog mutation history.
Existing Catabolic jobs, artifacts and publication remain Catabolic-owned when used.

Motion owns application users and authorization policy, playback planning and
delivery, sessions, progress, devices, offline/download state, playlists, HTTP/UI
and host resource admission. It may store Catabolic IDs, explicit migration maps
and disposable caches. After cutover it must not maintain a second authoritative
catalog, metadata resolver or scanner. Machine profiles and output catalogs are
not application users or libraries; saved selections supply library membership.

| Crate | Placement of new work |
| --- | --- |
| `catabolic-core` | Identity and interval validation, contribution resolution, merge/split planning, replay and lifecycle decisions |
| `catabolic-store` | Schema, revisions, atomic command commit, scoped snapshots, search index, generation and import checkpoints |
| `catabolic-runtime` | Host facade, admission, worker supervision, helper identity, migration orchestration and existing effects |
| CLI, HTTP and Python adapters | Translate supported contracts; no independent resolution or persistence policy |
| `catabolic-verification` | Models invoking production decisions plus independent expected results |

Start with supported modules in these crates, not a universal SDK or a second job
engine. JSON request bodies can remain where useful; version the host envelope and
provide typed IDs, revisions, errors and lifecycle handles. Keep low-level `Store`
access privileged. Public application input cannot choose arbitrary SQL or mint
host authority.

The host fixture must demonstrate open, bounded read, mutation, scan, pending
status, cancellation, drain/close and reopen. Sessions are serialized by their
owner. Blocking SQLite/tool work runs outside the application's async executor
threads. No transaction crosses an await, network call, helper launch or operation
that detaches/reconnects the store. Composed mutations use the same borrowed store
and transaction; they must not reopen the path under an existing writer lock.

Stable host errors distinguish invalid input, unsupported capability/schema,
not found or not visible, stale revision/scope/cursor, conflict, busy/retry,
cancelled, timeout, budget exhausted/truncated, recovery required, expired receipt,
missing tool and internal failure. Each includes a stable code, safe details and
explicit retry guidance. Incidental message text is diagnostic only. A timeout
must distinguish aborted work from a command whose durable outcome must be looked
up; it cannot invite a new mutation identity by default.

The unchanged dependency graphs cannot link together: Catabolic pins
`rusqlite 0.40.2` / `libsqlite3-sys 0.38.2`, whereas the reviewed Motion graph uses
`sqlx 0.8.6` / `libsqlite3-sys 0.30.1`. Both SQLite sys crates declare `links=sqlite3`.
Motion owns alignment of its dependencies and its toolchain minimum with
Catabolic's Rust 1.99 requirement. External integration must eventually prove one
combined graph; the sequencing amendment below defers that proof while Catabolic
implementation proceeds. Catabolic is not downgraded by assumption. A process transport is a
contingency requiring an explicit design amendment if native alignment fails.
Native helper executables remain allowed, with explicit path/digest registration;
embedding does not promise every effect occurs in the host process.

## Logical identity and physical bindings

Use this logical hierarchy:

```text
work item
  edition item
    stable timeline
      equivalent version with its own association item
        ordered bindings to exact file revisions
```

Works and editions retain ordinary item IDs and existing relationships. Add typed
timeline and version records with foreign keys, revisions and validated owners.
A version owns a dedicated item for its existing file associations; the typed
version record references that item. Timelines are durable coordinate identities,
not hashes of the currently preferred file. A different cut has a different
timeline. Equivalent encodes join a timeline only through recorded declared or
verified evidence. Unknown equivalence creates a separate provisional timeline.
Changing preference never changes a timeline ID or rewrites progress references.

This composition is chosen because current active parts are unique by
`(item_id, role, part)`: separate version association items allow two complete
two-part representations without relaxing that constraint. A new binding owner
references each existing association and pins its exact revision, interval and
timeline placement. It owns those typed facts; arbitrary association metadata is
not a competing authority. All writes for typed versions go through this owner,
including legacy adapter calls. Direct legacy mutations that would bypass its
invariants must be refused.

The first identity fixture must prove this composition with actual queries,
components and fallback before schema implementation is committed. If it cannot,
amend this design explicitly; do not silently substitute metadata-only links.

### Binding rules

- IDs are stable; binding changes create revisions. Historical bindings and their
  file revision references remain available for receipts and offline references.
- A ready version has unique consecutive primary parts `1..N`, complete declared
  coverage and one timeline. Alternative versions may each have part 1. Selection
  chooses an entire version; no fallback may combine parts from different versions.
- Source and timeline intervals use checked integer microseconds and half-open
  ranges `[start, end)`, with `0 <= start < end`. Both endpoints are required for
  a slice. Timeline placement must preserve duration in the initial contract;
  speed changes or inferred synchronization are unsupported.
- Tool time bases are retained as evidence. Conversion to microseconds is explicit
  and checked: round a slice start up and its end down, reject an empty/overflowed
  result, and report the normalization. Never infer exact equivalence from rounded
  duration equality. Frame-exact transforms require separate qualification.
- Whole-file bindings can retain unknown duration as evidence, but cannot claim
  a known interval, complete multipart timeline or slice readiness until measured.
- An explicit segmentation set groups uses of one exact file revision as distinct
  logical segments. Within that set, active segment ranges cannot overlap.
  Two episodes from one file must be admitted in one such set. Overlap is also
  forbidden between primary parts within one version's source/timeline coverage.
  Reuse in an alternative representation or different editorial context is not
  globally forbidden; its context must be explicit rather than inferred.
- The initial version composition allows one association per file/item/role,
  preserving the existing uniqueness rule. Repeated use of the same file within
  one version is unsupported and rejected; it is not needed for the two-episode
  fixture, whose episodes have distinct version items.
- Same-path replacement yields a new file revision. Digests, titles and languages
  alone do not authorize a rebind, move or timeline equivalence.

Component compatibility continues to validate the version association item and
part, exact video file/revision, edition, timeline, coverage and synchronization.
Related work IDs are insufficient. Resolve logical requests to a concrete version
before calling this owner. Typed timeline facts replace container-derived identity
only through the new validated path; legacy calls keep their recorded behavior.
Internal tracks inherit the selected binding's slice boundaries. External partial
coverage remains blocked; a full claim must cover the selected logical part with
explicit coordinate mapping. No new parallel component or fallback implementation.

Work-level queries aggregate versions through their typed ownership. Legacy reads
return their documented shape only where a lossless adapter exists. An ambiguous
write or a request unable to express a version/slice returns an explicit upgrade
or unsupported-contract error; it must not choose an arbitrary version.

## Contributions, tags and accepted identity

Store independent contribution documents keyed by subject, source namespace and
source record ID, with monotonically checked revisions and retained history.
Updates require the expected source revision; refreshes cannot overwrite a newer
local or provider update. A contribution includes normalized values, provenance
and observation/source evidence. Pure resolution follows these rules:

1. An explicit local field value wins.
2. Equal normalized provider values resolve with every agreeing source attributed.
3. Conflicting provider values remain an explicit unresolved conflict.
4. Scan-derived values are fallback only when there is no stronger contribution.

Removing a local value reveals retained provider/scan evidence. Local explicit
null, absent field and removal of an override must have distinct request forms;
null is a value only for fields whose schema permits it. Resolution returns state,
value when resolved, sources and the contribution revisions used. Materialized
metadata is a rebuildable projection maintained atomically with its inputs.

Reuse source-specific tag assertions and Catabolic's alias normalization. Add
local suppression by canonical tag identity; suppression hides the effective tag
without deleting provider assertions and survives provider refresh and alias
changes. Removing suppression reveals active assertions. Tag merge/alias changes
must reconcile suppression inside the same transaction.

Accepted identity pins are explicit revisioned decisions, separate from provider
suggestions. Automated rematching cannot replace them. Changing or removing a pin
requires a reviewed mutation and history. Schema-32 values with unknown provenance
become `legacy_import` evidence, preserving their effective value until explicitly
reconciled; do not mislabel them as manual decisions or verified provider evidence.

## Reviewed mutations and durable results

Introduce versioned catalog commands for merge, split, identity acceptance and
other compound edits. A preview is read-only and returns the proposed change,
complete affected set, expected entity/relationship/membership revisions and
explicit reference mappings. Include a guard against newly added related rows
between preview and apply; checking only the rows initially returned is insufficient.
Start with the coarse catalog generation as that guard if finer revisions are
not yet complete. It is acceptable to reject unrelated concurrent changes initially.

Apply obtains the writer transaction, revalidates authority and the affected-set
guard, executes the catalog mutation, updates projections/generation and commits
the immutable result receipt in that same transaction. No network or filesystem
effect is allowed inside this command transaction. Follow-on work uses the existing
durable execution owners and references the committed command.

The receipt key is `(catalog identity, host namespace, operation ID)`. It binds a
versioned canonical payload digest and the original result, including IDs, revisions
and mappings. Identical retries return that result, even if original preconditions
are now stale. A different payload under the same key fails. Receipt access still
requires current authority; replay must not leak a formerly visible affected set.
Provide a redacted committed-status result when authorized reconciliation can no
longer read the original body.

Keep full results until their declared retention deadline and any registered
migration/reconciliation hold has ended. Expiry may compact the result, but retain
the key, digest and committed/expired tombstone for the catalog lifetime. A retry
after compaction returns `receipt_expired`, never reruns the command. Bounded
storage policy is explicit: refuse new commands on exhausted storage rather than
silently recycle keys. Concurrent duplicate calls resolve under one transaction.

Each command also carries its admission incarnation. A known receipt can be read
after reopen under current authority, but an absent receipt from a prior
incarnation must return `outcome_unknown_after_restore`, never execute again.
Restoring an older backup can remove both results and tombstones; it cannot prove
that an external caller's command never committed. Motion retains its intent and
reconciles that uncertainty explicitly rather than automatically choosing a new
operation ID. Include this restore-after-commit case in R2/R6.

Merge keeps an unambiguous alias from retired identity to survivor and returns
explicit version/timeline mappings. It does not declare different timelines
equivalent. Split retains historical identity and returns a reviewed one-to-many
mapping; an ambiguous source ID never becomes a redirect to an arbitrary child.
Historical playback and offline references stay pinned unless a separately
authorized, unambiguous remapping exists.

Motion persists its intent before invoking Catabolic and applies its own reference
updates idempotently after reading the receipt. On restart it resumes that intent.
There is no distributed transaction between the two databases. Include crashes
before commit, after commit/before reply and during Motion reconciliation in the
acceptance fixtures.

## Scoped reads, ranked search and invalidation

Add a narrow trusted-host admission path reusing the existing authorized views
and bounded evaluations. The host supplies its namespace, opaque authorization
revision and approved saved-selection references; Catabolic resolves membership
and field visibility against its own snapshot. Capabilities are private handles
bound to the session, catalog incarnation, selection revisions, generation and
host authorization revision. Do not copy Motion users or accept an untrusted
client's item-ID list as authorization.

Motion holds an authorization lease while admitting an operation. For mutations,
the lease covers the catalog commit; a revocation must wait for an admitted commit
or prevent its admission. For reads, validate authorization for each page and
before delivering a completed result. Catabolic rechecks relevant catalog
membership inside its transaction. Document this ordering; separate databases
cannot supply a fictitious atomic permission snapshot. Long-lived caches/cursors
must fail on revoked host leases or changed membership.

Apply scope before detail, relationships, counts, facets and pagination. Reuse
saved SQL/GraphQL and owned bounded evaluations, not a new query DSL. Timeouts,
cancellation and truncation are explicit outcomes, never an empty successful
library. Not-found responses must not reveal whether a hidden entity exists.

Add a Catabolic-owned SQLite FTS5 index over resolved searchable metadata, accepted
identities/aliases and effective tags. Keep unresolved conflicts available in
provenance, but do not silently index a selected winner. Update index documents
in the catalog transaction initially; the index is rebuildable and versioned.
Existing catalogs require a completed rebuild before search readiness is declared.

Use Motion's pinned search cases for comparable matching and ordering, with a
stable item-ID tie-break and scoped totals; numeric score parity is unnecessary.
Do not expose global FTS statistics or compute visible ordering from hidden
documents. The initial ranker must use document-local match features and fixed
field weights, or scope-local statistics, rather than unrestricted global BM25.
Qualify both hidden-item isolation and relevance. Failure to meet either gate
requires revising the ranker, not substituting substring search.

Introduce a catalog generation contract covering every exposed mutation, including
legacy entrypoints, contributions, pins, selections, availability and new bindings.
Audit existing fallback epoch triggers; reuse their mechanism where complete, but
do not assert existing coverage without the mutation inventory and tests. A write
transaction advances generation when visible state changes. Cursors/cache keys
include schema/contract version, catalog ID, incarnation, generation, query identity
and scope revision. A changed value yields a consistent retained snapshot where
supported or an explicit stale/resync result.

Restored backups require special treatment: ID plus epoch can repeat. Controlled
restore stops all readers/workers/writers, invalidates sessions and rotates a fresh
incarnation before reopening admission. A reopened host always drops old handles
and caches, even if database bytes identify the same catalog. Incarnation metadata
is published through the exclusive restore protocol, not merely copied from the
backup. A session must validate its incarnation before writing. Uncoordinated live
file replacement is unsupported and must trigger refusal/reopen when detected;
it is not a supported restore path. Qualify cross-process quiescence and stale
open descriptors explicitly.

Direct reads and coarse invalidation are sufficient initially. Request/job events
are not a complete catalog change feed. A durable general feed is deferred until
an incremental replication or notification consumer actually requires it.

## Execution and resource ownership

Reuse native scan/processing, shared demands, cancellation, claims, artifacts and
recovery. Host admission grants a permit for the actual bounded work. A request
returning `pending` does not release that permit while its isolated worker lives.
The host supervisor retains/transfers the permit with the worker handle and
releases it only after verified exit or completed recovery. Cancellation of one
demand does not terminate work still required by another consumer.

The first integration must either wait and join under the permit or transfer
ownership to a registered supervisor before returning. After a crash, reconcile
live workers and durable claims before admitting replacement work. Shutdown stops
admission and drains or explicitly hands over owned workers; dropping a session is
not proof that its children stopped. Per-device/worker limits alone do not prove
CPU/I/O/process budgets. Add finer callbacks only if this composition cannot
enforce the host's measured limits.

If Motion serves Catabolic-generated outputs, qualify retention for exact-revision
references before enabling that use. Add external-consumer leases only if existing
publication retention cannot protect active use. Read-only catalog integration
does not depend on those leases. Motion's playback cache remains Motion-owned.

## Schema upgrade and compatibility policy

Keep SQLite and all historical migration bytes/checksums. Allocate new migration
numbers from the branch's actual head when implementation starts; schema 32 is
the input baseline, not the permanent output constraint. Catabolic owns its schema;
Motion owns its separate migrations. Do not copy the prototype's conflicting
Motion migration `0045` or create Motion-private tables in Catabolic.

The native catalog upgrade is explicit, never a side effect of ordinary open:

1. Inspect schema, identities, history, claims, pending journals, tool requirements
   and representability. Produce a versioned plan tied to the exact input and
   migration checksums, including preserved IDs, mappings and unresolved evidence.
2. Stop admission and quiesce every writer and worker. Refuse unresolved durable
   work unless a specifically qualified recovery path handles it.
3. Create and verify a consistent SQLite backup including committed WAL state.
   Rehearse the plan on an isolated clone and compare semantic results and history.
4. Recheck the planned input under exclusive ownership. Apply through the existing
   transactional migration owner, including backfill and validation. Preserve the
   database identity and logical/file IDs; record any new version/binding mapping.
5. Commit the schema/history transition atomically. Reopen with a fresh session
   incarnation and validate counts, relationships, effective values, generation,
   search and recovery status before enabling new admission.

Preserve legacy associations and history during backfill. Create typed versions
only where a coherent grouping is evidenced; keep ambiguous associations explicitly
unclassified and visible for review. No fabricated duration, source attribution,
equivalence or accepted identity. A legacy association mapping records original
IDs and owners if it is rehomed to a version item. Both old and new mutation APIs
must enter the same owner; a historical row retained for audit is not a second
writable catalog. The R1 fixture must settle the exact backfill and adapter mapping
before these SQL migrations are authored.

The old Python engine may refuse the new schema. That is a supported boundary,
not a requirement to retain a second writable implementation. Before upgrade,
schema-32 engine handover still needs its existing recovery evidence. After upgrade,
recovery is native-only unless a lossless downgrade is separately implemented and
qualified. A backup restores the pre-cutover point only; it cannot discard later
writes or reverse external effects safely. Prefer forward repair after admission
has resumed.

All supported writers must use the shared admission/locking protocol before an
upgrade is enabled on real catalogs. If the pinned legacy engine cannot be fenced
by that protocol, require verified shutdown and exclusive operational ownership;
an advisory lock used only by the new engine is insufficient.

| Surface | Contract after redesign |
| --- | --- |
| Schema-32 catalogs | Explicit inspect/rehearse/upgrade with preserved identities and evidence; no silent open-time migration |
| Upgraded catalogs | Native owner and schema rejection by unsupported engines; no automatic Python fallback |
| Unchanged CLI/HTTP behavior | Retain frozen comparison fixtures and existing authorization, output and recovery rules |
| New or unrepresentable media semantics | Versioned native operations; adapters preserve lossless cases and explicitly refuse ambiguous legacy mutations |
| HTTP 1.7 artifacts | Preserve historical bytes; any new wire semantics require a separately versioned contract/extension before exposure |
| Python public API | Thin Rust wrapper last, preserving supported legacy contracts and surfacing explicit upgrade/unsupported errors |
| Raw SQL/database internals | Privileged internal interface, not a promise that applications can write new invariants directly |
| Filesystem journals and outputs | Existing safety gates still apply; no format change grants permission to enable blocked effects |

Extend the existing compatibility ledger with `preserve`, `adapt`,
`versioned_change` or `defer`, plus replacement contract, migration rule, fixtures
and evidence revision. Do not mark an intentional change as differential parity
or remove a release requirement by relabeling it deferred.

The additive [catalog contract ledger](../tests/parity/native-catalog-contract-ledger.json)
maps these dispositions to the frozen ledger without rewriting historical
qualification. `python3 scripts/catalog_contract_audit.py` checks references and
planning status; a successful audit is not behavioral or release qualification.

## Motion import and cutover

This is separate from upgrading an existing Catabolic database. Motion exports a
versioned snapshot with logical IDs, contributions, decisions and references.
Catabolic imports through supported owners with a durable import ID, input digest,
per-batch receipts and mapping tables. Reusing the same import ID with different
input fails; completed batches replay rather than duplicate. Unavailable sources
stay unknown/unavailable, never successful empty coverage.

Retain Motion logical IDs where supported and report collisions explicitly.
Map physical occurrences using source/path/revision evidence, without replacing
Catabolic's occurrence identity scheme or merging equal-content duplicates.
Evidence-backed rename and same-path replacement are different operations.
Maintain version/timeline structure and preserve unresolved historical references.

Motion owns the audit and idempotent update of every application reference:
playlists, preparations, markers, progress, history, offline manifests and downloads.
Stage imported data outside the active catalog scope until the manifest and
reference checks pass. For the first cutover, use an explicit write freeze for
Motion's catalog/scanner and Catabolic import admission; avoid dual writes and a
new change-capture system. Large preloads can be rehearsals, but the authoritative
snapshot must be stable during final import/verification.

Persist cutover state in both owners using an idempotent handshake: prepared,
validated, activated. Admission remains closed while either side disagrees.
After activation only Catabolic admits catalog mutations/scans; Motion reads it
authoritatively. Restart reconciliation finishes or refuses an incomplete handoff.
A pre-activation failure can resume import or return to the unchanged Motion
authority. After new writes, use forward recovery; returning to the old snapshot
is not a lossless rollback.

## Implementation sequence and acceptance gates

**Sequencing amendment, 2026-10-14:** the user explicitly directed “no motion
changes, and no need to test with motion now. All changes to catabloic comes
first.” Continue Catabolic R0–R7 implementation and local qualification without
waiting for Motion dependency adoption or combined-host testing. Do not edit or
test Motion in this phase. R1 depends on the locally qualified Catabolic host
contract, not the deferred external composition gate. Its real-owner identity
fixture still precedes R2 schema implementation. R6 may implement and qualify
Catabolic import and handoff contracts using bounded fixtures, but actual Motion
reference migration, two-owner activation and external integration acceptance
remain unqualified. Record these as deferred evidence, never passing gates.
This changes sequencing only: retain external integration obligations and every
applicable M0–M11 safety, compatibility, recovery and release requirement.

Work stays on `codex/rust-migration-m4-plus`. Each package lands as a reviewable
slice with exact source/artifact evidence. Package labels below are new work
tracking, not replacements for the existing M0–M11 release gates.

| Package | Dependencies and owner | Deliverable and exit evidence |
| --- | --- | --- |
| R0 Native host proof | First; Catabolic facade; external composition deferred | Minimal supported features and lifecycle/error example; supported Rust host opens, reads, mutates, scans, cancels, drains and reopens without Python/HTTP; helper identity recorded; combined Motion graph remains deferred |
| R1 Identity contract fixture | Local R0 host contract; core/store/components/fallback | Two cuts; equivalent and unknown encodes; two alternative two-part versions without mixed selection; one revision/two episode intervals; rejected overlap; exact-revision components and partial-coverage refusal. Prove version-item composition and legacy mapping before DDL |
| R2 Schema and command foundation | R1; store/migration/core | New migrations, entity revisions, generation/incarnation, receipt transaction and expiry tombstones. Clone upgrade and interruption tests preserve IDs/history; stale/duplicate/conflicting commands cannot partially apply |
| R3 Contributions and curation | R2; core/store | Resolver, tag suppression, pins, merge/split previews and mappings; stale refresh rejection, local override removal, aliases, phantom affected-set changes and lost-reply recovery |
| R4 Scoped reads and search | R2; access/query/search, Motion authorization adapter | Host lease/admission, scoped detail/counts/facets/search/cursors; revocation and hidden-item isolation, external edits, restore/reopen, deterministic relevance and explicit timeout/budget outcomes |
| R5 Worker admission qualification | R0 and new command integration from R2; runtime/host | Pending workers retain permits; bounded shutdown/restart; one cancelled consumer preserves shared work; stale outputs blocked and no orphaned children on supported platforms |
| R6 Import and application cutover | R3, R4, R5; Catabolic import plus Motion integration | Representative stable snapshot, full reference audit, interrupted/resumed/repeated import, rename/replacement/duplicate/unavailable fixtures and crash-tested one-owner activation |
| R7 Native product completion | Catalog packages plus existing native blockers | Revised ledger, remaining behavior/safety/recovery/platform gates, full current-source aggregate, standalone artifact, then thin wrapper/default transition |

R3 and R4 can progress independently once R2 contracts are stable. R5's lifecycle
proof can begin after R0, but it must be rerun against integrated R2/R6 artifacts.
Do not treat a read-only host demonstration as R6 completion. Native catalog
adoption can precede full product completion only for the qualified operation and
platform subset, with all safety gates relevant to that subset satisfied.

### Requirement traceability

| Motion requirement | Primary package | Mandatory evidence |
| --- | --- | --- |
| 1 Native embedding | R0, R5 | Combined graph, supported lifecycle/errors and helper/cancellation/reopen receipt |
| 2 Logical identity | R1, R2 | Identity/interval matrix through real component/fallback owners and upgrade |
| 3 Metadata and decisions | R3 | Revision conflict, retained provenance, suppression and identity-pin fixtures |
| 4 Merge/split and recovery | R2, R3, R6 | Atomic result, stale preview, lost reply, key conflict, expiry and cross-database reconciliation |
| 5 Scope/search/invalidation | R4 | Scope isolation, ranked relevance, revocation, concurrent pages and restore fencing |
| 6 Import/cutover | R6 | Stable identities, every application reference, resume/repeat and single-owner recovery |
| 7 Background work | R5 | Actual worker lifetime, shared cancellation, resource recovery and stale output refusal |

The original qualification route uses `/Volumes/seed1/Projects/kyoto/kyoto.py`
with its current supported invocation. **Runner amendment, 2026-10-14:** the user
explicitly authorized continuing with standard Rust in the current cloud workspace,
without Kyoto or a Codex task thread. Rust 1.99.0 remains the required toolchain;
this changes the execution route, not the acceptance obligations or platform scope.
Record each run's actual runner, freeze source/input manifests, and retain toolchain,
platform, helper and artifact identities with results. Do not label standard Cargo
runs as Kyoto evidence. Kyoto remains an available route where explicitly selected;
inspect its current interface before using it.


Pure decision fixtures and bounded models exercise the production functions.
Report explored states/transitions and bounds; separately exercise real SQLite
rollback, process death, interrupted commit, reconnect and worker cancellation.
For each package record contract case, test entrypoint, source/artifact identity,
platform, outcome and remaining limitation. Focused tests qualify their case, not
the full workspace or another platform. Run the required full aggregate at the
integrated release checkpoint and retain all failed runs as evidence.

## Implementation evidence and current limits

The R0 host boundary and R1 whole-version composition have separate frozen
receipts in `tests/parity/native-catalog-r0-checkpoint.json` and
`tests/parity/native-catalog-r1-checkpoint.json`. Those receipts remain tied to
that source, not to subsequent schema changes.

R2 local foundation evidence is recorded in
`tests/parity/native-catalog-r2-checkpoint.json`; independent schema review remains
incomplete. The implementation extends the existing migration, Store, Session, curation,
component and fallback owners. See
[`CATABOLIC_NATIVE_FOUNDATION_V1.md`](CATABOLIC_NATIVE_FOUNDATION_V1.md) for opt-in
schema admission and durable command boundaries, and
[`CATABOLIC_NATIVE_SCHEMA_V1.md`](CATABOLIC_NATIVE_SCHEMA_V1.md) for reproducible
additive SQL and preservation tests. Legacy defaults remain schema 32; native
schemas33–36 remain explicitly supported versions. Schema37 is the current
opt-in integration target for exact closed-companion cleanup, extending the
separately preserved schema36 builtin cleanup and schema35 search storage. Receipt retention and storage limits are explicit host
policy, without silent key eviction or an implicit duration.

R3 integration is described in
[`CATABOLIC_NATIVE_CURATION_V1.md`](CATABOLIC_NATIVE_CURATION_V1.md). Additive
migration 034, contribution resolution, tag suppression/merge, accepted identity
pins and reviewed logical replacement extend these owners. Schema33 stays frozen;
its pinned admission regressions remain. Latest-target migration/crash fixtures
now explicitly target schema34; this is a versioned target replacement, not a
claim that the R2 source receipt qualifies newer code. The schema34 preservation
exception only appends exact revision-1 legacy-identity evidence baselines to the
invalidation index while preserving every prior row and catalog generation.
The final frozen R3 aggregate passed 871 tests with zero failures and nine opt-in
entries across 64 harnesses. All seven applicable opt-in cases passed separately;
two fork helpers ran through their passing wrappers. Exact source, toolchain,
initial failures and limitations are recorded in
`tests/parity/native-catalog-r3-checkpoint.json`. This qualifies the bounded
curation slice, not every populated relocation contract or a milestone closure.

R4 derived search storage is recorded separately in
`tests/parity/native-catalog-r4-search-checkpoint.json`. The frozen schema35 Store
suite passed604 test executions with zero failures across40 harness summaries;
five ignored entries remain separate obligations. Schema reproduction/audit51
Python tests, formatting, Ruff and strict core/store/runtime all-target Clippy
passed. The preceding full core/store/runtime attempt failed three obsolete
latest-target fixture assertions; those assertions and their per-migration crash
boundaries were versioned and the complete Store suite reran. That is not a new
full-workspace pass. Scoped FTS/cursor/private-writer integration and all release
gates remain outstanding for this storage checkpoint.

The independent staged schema review remains incomplete after its recorded
platform error. It has not been retried or reassigned, and implementation tests
do not clear it. Native WAL cutover and controlled restore publication remain
unavailable in this slice. Focused development results do not close R2, R7 or any
M0–M11 gate. External Motion composition and application cutover remain deferred
under the Catabolic-first amendment above.

## Remaining decisions and deferred work

R0 selects the exact supported feature set, host facade signatures and Motion's
dependency alignment. R1 settles the SQL representation/backfill from the chosen
version-item composition. R2 sets full-receipt retention duration and storage
limits while preserving permanent no-replay tombstones. R4 tunes ranking weights
against pinned relevance cases. These are bounded implementation choices; they do
not reopen the ownership, identity, explicit-upgrade or durable-result decisions.

General change feeds, partial external-component coverage, repeated-file edits
within one version, speed-changing timeline transforms, external consumer leases
and finer per-child resource hooks are deferred unless an actual integration case
requires them. No event-sourced replacement, generic workflow engine, second
query language or Motion-specific catalog fork is planned.

The hardest work is enforcing identity across existing components/fallback,
atomic curation/replay, authorization/restore fencing and the two-owner cutover.
Each has an early failure fixture and a dedicated gate. Provider option gaps,
publication/recovery safety, production HTTP protection and platform qualification
from the native migration remain separate outstanding work. Previously blocked
review lanes remain blocked; this plan does not bypass them or enable their paths.

The current implementation continues **R4–R7** from the source-bound checkpoints
above. The R4 search-storage increment adds schema35; scoped matching/cursor and
private-writer integration follow before R4 can be qualified as a whole. External
Motion work remains deferred under the Catabolic-first amendment.


R4 private scoped search/cursor and writer evidence is recorded in
`tests/parity/native-catalog-r4-boundary-checkpoint.json`:114 focused Rust test
executions and 51 Python schema/audit tests passed with unchanged enumerated source.
Strict all-target Clippy and formatting passed. Schema34/35 private writer admits
bounded COMMIT callbacks with durable-result semantics; it refuses newer schemas
pending separate qualification and does not promise hard real-time filesystem I/O.
Relationship adapters and the full integrated aggregate remain separate work.

The next integrated relationship/worker slice is recorded in
`tests/parity/native-catalog-r4-r5-checkpoint.json`. Its frozen source passed 174
focused Rust test executions and 51 Python controls, including five separately
executed real-helper cases. A broader search-index run then exposed a stale
latest-target assertion expecting 35. The failure is retained; its versioned
34-to-latest fixture preserves every existing preservation/generation/search
assertion, and all 17 search-index tests passed on that separately frozen final
fixture source. These are distinct source cohorts, not a full current-source
aggregate. The native admission/closed builtin cleanup contract and exclusions
are described in [`CATABOLIC_NATIVE_WORKER_ADMISSION_V1.md`](CATABOLIC_NATIVE_WORKER_ADMISSION_V1.md).
Scoped relationship details/counts/facets/recursive paths are now bounded native
surfaces; broad file/history/provenance adapters remain denied. Shared native host
admission, complete companion cleanup/reopen recovery, schema36 private-writer
expansion, import and final artifact/platform qualification remain separate work.

The subsequent common-gate/import checkpoint is
`tests/parity/native-catalog-r6-checkpoint.json`: 285 focused Rust executions and
71 Python schema/tooling controls passed against 1,479 unchanged Git-visible input
files. This includes eight explicitly executed current/archived helper cases,
actual two-consumer cancellation, 24 populated schema36 private-writer upgrade
cases, and bounded typed import activation/rollback/replay through existing owners.
Native hosts now require their exact shared authorization gate. The private writer
admits schemas34–36; schema37 remains refused pending separate qualification.
Import V1/V2 adds no schema. Real scanned-source activation still refuses retained
native34–36 observation pointers; positive companion cleanup, required remaining
read adapters, public cross-owner composition and final artifacts remain separate
work. This focused receipt is not a full workspace or release pass.

## Current native37 integration checkpoint

The schema37 worker/read/writer/import and scoped component slices are integrated
in the primary checkout. Its exact frozen focused receipt is
`tests/parity/native-catalog-native37-focused-checkpoint.json`: 311 Rust test
executions, 100 Python schema/audit controls, 222 parser-envelope parity cases
and four retained parser nonparity controls. Sixteen helper opt-ins executed
individually. Strict all-target Clippy, formatting and every source/helper fence
passed. The original orchestration command failed at a missing parser-helper
environment variable; its failure is retained, followed by a separate corrected
parser/lint/format tail on the same unchanged 1,517 inputs. This is focused
evidence, not the required full aggregate. Schema37 adds exact closed-companion reservation
and parent-sealed retained-child exit evidence, plus a schema-derived admission
contract and explicit borrowed-owner shared join. It does not adopt old workers
or infer cleanup from terminal state, PID absence or lease expiry. The private
writer's actual36-to37 matrix is explicitly versioned; historical35-to36 evidence
and fresh34/35/36 controls remain distinct.

The already committed membership/current-provenance and whole-file fact receipts
are `native-catalog-r4-details-checkpoint.json` and
`native-catalog-r4-physical-checkpoint.json` under `tests/parity`. Current component
reads add an explicit whole-occurrence purpose and require authority over all
relevant lineage endpoints. File/component facts are recorded physical facts,
not logical episode measurements, live availability, media access or effect
authority. Search weights still require deferred pinned external relevance
evidence. Controlled restore/incarnation publication and complete cross-process
quiescence qualification remain unavailable.

Scanned-source import can use only exact schema37 cleanup witnesses alongside
the existing pending-work, complete inventory and per-job gates. The next integrated
addendum adds mixed-owner crash fixtures and explicit persisted restart admission
through `HostSession::open_after_import_cutover`; see
[`CATABOLIC_NATIVE_IMPORT_RESTART_V1.md`](CATABOLIC_NATIVE_IMPORT_RESTART_V1.md).
Its primary-checkout receipt is
`tests/parity/native-catalog-native37-followups-checkpoint.json`: 203 Rust
executions and two complete CLI matrix methods passed on 1,531 unchanged inputs.
The matrices cover 250 parser envelopes and 242 variable-admission comparisons.
Both actual persisted-restart helper cases ran explicitly. Strict Clippy, format,
Python lint and all source/helper fences passed. Ordinary `open_authorized` does
not automatically enforce an external cutover checkpoint.
The optional read-only Python inspection/planning seam has separate historical
installed-wheel cohorts and is not yet an adopted typed host binding.

The clean schema36 seven-crate aggregate was interrupted during recovery after
1083 completed passing executions and no reported failures; it has no terminal
result and is not a passing aggregate. Its retained receipt is
`tests/parity/native-catalog-native36-interrupted-checkpoint.json`. A subsequent
source-fenced exact binary build succeeded but does not complete those tests.
The final integrated source still requires a fresh complete aggregate, all
applicable opt-ins, artifacts and supported installed/platform gates. No M0–M11
obligation or blocked independent review is waived.


The V2 import representation/contribution matrix is now integrated without new
production or schema owners. Its exact primary receipt is
`tests/parity/native-catalog-native37-final-controls-checkpoint.json`: 66 import
methods, including ten new matrix methods across 88 disposable native34–37
catalogs, and 17 Python integrity/audit controls passed on 1,541 unchanged inputs.
The portable variable regression retains mandatory recorded historical outputs
and live current-native/oracle calls; its full standard-entrypoint replay remains
part of the next native/Python aggregate. See
[`CATABOLIC_NATIVE_IMPORT_REPRESENTATIONS_V2.md`](CATABOLIC_NATIVE_IMPORT_REPRESENTATIONS_V2.md).
Unrepresentable history remains unresolved/refused, and no external reference
migration or release/default transition is implied.
