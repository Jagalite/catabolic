# Catabolic as Motion's catalog authority

Prepared 2026-10-13 for upstream discussion. This is a source review and a proposed
integration contract, not a claim that the integration is implemented or qualified.

## Request

We want Catabolic to own Motion's catalog: discovery, file observations, logical
media identity, associations, descriptive metadata and provenance, catalog
relationships, and catalog queries/search. Motion should consume those capabilities
through supported native Rust APIs and retain its application-specific state.

Please first identify existing APIs and compositions that satisfy the acceptance
cases below. Where they do not, we want reusable Catabolic extensions rather than
a Motion-specific catalog implementation. A documented call sequence and a passing
fixture can close a requirement without new production code.

The review used Catabolic `codex/rust-migration-m4-plus` at
`e24cda655ba62654c1754d7607ad7e922325b96d` and Motion published `main` at
`1e5a24675b059337b34e98f86b225178798cc080`. References below pin those revisions.
The upstream response supplied in this discussion is incorporated below. Its
confirmations refine the source review; they are not evidence of newly implemented
code or a passing combined host fixture.

## Status after upstream response

| Area | Assessment from the response | Next deliverable |
| --- | --- | --- |
| Native embedding | Existing foundation; dependency conflict confirmed | Motion-owned dependency alignment and a Kyoto-built host fixture; stable host error contract |
| Logical identity | Existing entities need additional enforced semantics | Supported mutation owner and a composition that also satisfies component/fallback identity checks |
| Metadata | Reusable extension required | Revisioned contributions, deterministic resolution, tag suppression and pinned identity protection |
| Merge/split and command recovery | New operations over existing transaction machinery | Reviewed affected-set validation and an atomically committed durable result receipt |
| Scoped reads | Existing authorized views are tied to stored tokens/grants | Demonstrate host scope composition or add narrow trusted-host admission |
| Ranked catalog search | No comparable native owner identified by either review | Reusable search implementation, unless a behavior change is separately accepted |
| Invalidation | Existing epoch is a candidate, not yet a complete contract | Mutation-coverage audit, cursor semantics and restore invalidation |
| Import and execution | Joint integration work using existing owners | Resumable migration and resource accounting for actual worker lifetimes |

The first gate is the combined native dependency graph and host fixture. Before
semantic implementation, settle interval units/endpoints/overlap scope, logical
identity composition, host scope admission, stable error categories and receipt
expiry behavior. General change feeds and external-consumer leases remain deferred.

## Ownership

| Catabolic owns | Motion owns |
| --- | --- |
| Source registration, scanning, file revisions, probe facts, availability evidence | Application configuration, users, permissions and library access policy |
| Logical works, editions, equivalent representations and catalog associations | Playback planning, delivery, sessions and player controls |
| Metadata contributions, accepted identity decisions, tags and provenance | Viewing history, progress, devices, downloads and offline state |
| Catalog relationships, selection membership, search and catalog mutation history | Application queues/playlists, HTTP API and UI |
| Its processing jobs, artifact evidence and publication rules when used | Admission of background work against the application's resource budget |

Motion can retain catalog IDs, migration mappings and disposable caches. It should
not retain an independently authoritative item/metadata database, scanner or
metadata resolver. Catabolic's machine **profile** is not a Motion user profile;
its output **catalog** is not automatically a Motion library. Saved selections
may implement library membership without introducing another entity type.

## Existing capabilities to reuse

| Motion need | Existing Catabolic capability | Remaining question |
| --- | --- | --- |
| Rust library integration | Native `Store`, persistent `embedding::Session`, transactions/savepoints, bounded query evaluation and reconnect checks | Supported host-facing surface, dependency compatibility and lifecycle example |
| Logical items and editions | Explicit item IDs, authoritative identities, custom kinds, `edition_of`/`part_of`, association roles and metadata | Which composition enforces Motion's timeline/version rules? |
| Multipart and alternative files | Parts, variants and explicit `representation_group`; fallback selections preserve logical slots | Not yet evidence of persistent timeline identity or equivalent-version mutation semantics |
| Audio/subtitle identity and compatibility | Components, exact-revision occurrences, accepted edition/timeline/offset claims, package resolution | Reuse; partial coverage is explicitly unsupported and must remain rejected unless separately implemented |
| Metadata evidence and curation | Immutable proposals, reviewed acceptance, stale-source checks, decision records and overwrite controls | General per-source field revisions, conflict resolution and local overrides |
| Tags | Source-specific assertions, withdrawal, aliases, hierarchy and atomic batches | Local suppression of another source's still-retained assertion |
| Browsing and collections | Saved SQL/GraphQL queries, immutable definitions, composition and bounded pagination | Scoped host integration, ranked search behavior and cache invalidation contract |
| Catalog jobs | Native scanning/processing, shared demand identity, cancellation, artifacts and recovery owners | Host admission and supported completion/status calls |

These are existing facilities, not requests for replacements. Expected sets model
completeness; they do not establish playback equivalence. Manifests/program bundles
are useful interchange, but are not a complete migration of identity, provenance
and application references. [C1], [C2], [C3], [C4], [C5], [C6], [C7]

## 1. Supported native embedding contract

**Mostly existing; integration and qualification required.** Provide a small
supported example that opens a catalog, performs bounded reads, executes a catalog
mutation, invokes scanning, observes completion, cancels work and closes cleanly.
Document which calls block, session/thread ownership, transaction boundaries,
recovery requirements, cancellation and machine-readable error categories.
Existing JSON-shaped native calls are acceptable; a new universal SDK is unnecessary.
The upstream response identifies frequently string-based errors: define a stable
host error contract instead of making Motion parse incidental message text.

Use `catabolic-store` for sessions, transactions and queries, and
`catabolic-runtime` for scanning/processing. Some isolated operations still require
the native helper executable. Document serialized session access and which runtime
calls detach/reconnect the store; those calls cannot be enclosed in incompatible
caller transactions. Native embedding does not imply that every effect runs in
the host process.

The current build graphs need reconciliation: Catabolic uses Rust 1.99 and
`rusqlite 0.40.2` / `libsqlite3-sys 0.38.2`; Motion uses Rust 1.98 and
`sqlx 0.8.6` / `libsqlite3-sys 0.30.1`. The unchanged graphs cannot coexist in one
Cargo dependency graph because both SQLite bindings declare the same native
library link. Motion can change its dependencies; this is not a request for
Catabolic to downgrade. Supply the minimal supported crate/features combination
and prove it in a Motion host build. [C1], [C8], [M1]

**Acceptance:** a Kyoto-built native host fixture, with required native helpers,
exercises reads, mutation, scanning, cancellation, interruption and reopen without
Python or a Catabolic HTTP server. This combined fixture is not yet demonstrated.
If dependency alignment is impractical, agree on one process transport as an
explicit alternative rather than maintaining two independent implementations.

## 2. Logical media identity beyond individual occurrences

**Existing primitives; upstream confirms additional enforced semantics are needed.**
Motion currently distinguishes a work, an edition, a timeline, a version of that
timeline, and ordered file bindings. A timeline is the stable coordinate system
for progress and playback; another encode can share it only with explicit declared
or verified equivalence. Unknown equivalence needs a separate timeline. Bindings
pin a file revision and may have start/end intervals. [M2]

Catabolic already has edition relationships, parts, representation groups and
explicit `edition_id`/`timeline_id` compatibility metadata. First show how these
represent the model and which mutation owner enforces its invariants. Storing IDs
in arbitrary metadata alone does not enforce equivalence, ordering or revision
checks. Do not introduce parallel component or fallback models. [C2], [C3], [C4]

One concrete constraint needs a design answer: active numbered associations are
unique by `(item_id, role, part)`, independently of `representation_group`. Two
multipart encodes therefore cannot both use `primary` part 1 under the same item
through the current association API. Representing versions as separate related
items may solve this, but upstream should demonstrate that composition together
with work-level queries and component/fallback resolution. A metadata-only version
label does not remove the uniqueness constraint. [C2], [C17]

The upstream response also confirms that component compatibility requires matching
item and part identities. Related version items or matching timeline metadata do
not by themselves satisfy that check. The proposed composition must preserve the
meaning of those checks across queries, components and fallback resolution.
Before implementing interval validation, define units, endpoint conventions and
the scope in which overlap is prohibited; do not assume a universal prohibition
across every use of a file.

**Acceptance fixture:**

- A theatrical cut and a director's cut of one work have separate timelines.
- Two encodes declared equivalent share one timeline; an unclassified encode
  cannot silently join it. Timeline identity survives a change of preferred file.
- Two alternative two-file representations coexist under one logical timeline.
  Each has complete, unique consecutive parts; selection cannot mix their parts.
- One file containing two episodes can bind to two timelines through known,
  non-overlapping intervals on the same revision. Invalid or overlapping bindings
  are rejected atomically.
- Existing external audio/subtitle compatibility remains tied to the exact video
  revision, edition, timeline and part. Unsupported partial coverage blocks;
  shared titles, languages or digests do not establish synchronization.

The requirement is the behavior and stable IDs, not five prescribed new tables.

## 3. Metadata contributions and explicit local decisions

**Upstream confirms a reusable extension is needed.** Motion's
resolver retains independent provider contributions. A local value wins; agreeing
providers share attribution; disagreeing providers produce an explicit unresolved
conflict; scan-derived values are fallback only. Removing a local override reveals
the retained contributions. Provider refreshes use an expected source revision.
These catalog decisions should move to Catabolic. [M3]

Catabolic's item update replaces its metadata object; proposal acceptance supplies
history, stale checks and overwrite controls, but those operations alone do not
establish the above contribution/resolution contract. Existing tag assertions
already preserve source provenance: do not rebuild them. The missing tag behavior
is a local exclusion that hides an imported tag while preserving its assertion,
and survives later provider refreshes. Withdrawing the provider's assertion is a
different operation. [C2], [C5]
Suppression must use Catabolic's tag normalization and alias semantics so that a
rename or alternate spelling cannot bypass the retained local decision.

Reuse existing identification/enrichment and proposal workflows. Define a supported
ingestion path for additional provider contributions and preserve manually pinned
identity choices against automated rematching. This does not require a new provider
marketplace or fetching every provider in the first release.

**Acceptance:** two providers disagree on a title; the conflict stays visible. A
local override survives refresh; removing it exposes the conflict again. A stale
provider update fails without losing newer evidence. A locally excluded tag stays
hidden across refresh, and removing the exclusion restores it. An automated
identification candidate cannot replace a pinned accepted identity.

## 4. Atomic reviewed identity changes and recoverable commands

**Upstream confirms new operations over existing transaction machinery.** We need
supported merge/split operations, or a composition providing equivalent guarantees:
preview the affected catalog objects; apply only against the reviewed revisions;
atomically update owned relationships, associations, identities and provenance;
preserve resolvable aliases where appropriate; return the exact identity/remapping
result. A split must not invent an ambiguous redirect from one old ID to several
new IDs. Existing arbitrary SQL access is not this contract. [C1], [C2], [M2], [M4]

Commands crossing Motion and Catabolic need a durable operation identity and
recoverable result. Replaying the same identity and payload after a lost reply
must return the original result; reusing it with another payload must fail.
Commit the catalog mutation and its result receipt in the same transaction.
Document receipt retention and what happens after expiry; expiry must not silently
turn a previously committed operation into a new execution. Catabolic already has
idempotent rendition demands and proposal decisions: reuse their approach where
applicable rather than introducing a second job system. [C2], [C9]

Motion remains responsible for authorization, persisting its durable intent before
invoking Catabolic, and applying application-side reference changes. Catabolic must
atomically validate the catalog revisions/affected set used to authorize the
command. Sharing a process does not make the two databases one transaction.

**Acceptance:** a stale preview fails with no partial catalog change. A crash after
Catabolic commit but before Motion receives the reply is recoverable by the same
operation identity. Motion can reconcile viewing/playlist/download references from
the returned mapping without repeating the catalog mutation. Historical timeline
references and exact-revision offline copies are retained, not silently rebound.
Also exercise conflicting-key reuse and retry after receipt expiry.

## 5. Scoped catalog reads, search and invalidation

**Reuse query/access engines; qualify the host contract and extend only gaps.**
Motion needs item detail, related items, memberships, metadata provenance, components,
file facts and ranked catalog search. Use existing saved selections, SQL/GraphQL,
bounded evaluations and native access enforcement. Do not add a second query DSL.
Catabolic's token-backed `Access` already evaluates grants and membership against
one owned snapshot; the open question is how Motion supplies its authorized scope
without duplicating application users and permissions. An existing safe API call
sequence is preferable to a new authorization subsystem. [C6], [C10]
The upstream response leaves two options: prove a suitable existing composition,
or add a narrow trusted-host scope admission API. Neither is qualified yet.

Scope must apply before results, relationships, counts, facets and pagination are
derived. Post-filtering a global result page is insufficient. Public Motion input
must not become unrestricted caller SQL. A mutation authorized against membership
must fail/revalidate if that relevant catalog state changes before commit.

Motion currently has ranked full-text search. Upstream also found no comparable
native search owner. Provide a reusable implementation with comparable matching,
deterministic ordering and scoped totals, or obtain separate agreement to change
the behavior.
Do not substitute title substring search without acknowledging the behavior change;
identical numeric ranking scores are not required. [M5]

For caching, begin with direct authoritative reads and bounded consistent snapshots.
Define a catalog generation/invalidation contract covering every exposed mutation,
including changes made outside Motion. Existing fallback epochs and watcher
invalidation may suffice after a coverage audit. Database identity plus an epoch
alone cannot distinguish a restored older backup with the same identity: define
an incarnation/reset mechanism or a host reopen procedure that invalidates all
affected caches/cursors. [C1], [C11]

A complete durable catalog change feed is **conditional**, needed only if later
incremental replication or client notification requirements demand it. The existing
API event stream covers request/job visibility, not a general catalog changelog.
Coarse invalidation and rereads are acceptable initially. [C12]

**Acceptance:** hidden items cannot affect counts or leak through related objects;
revocation or changed membership cannot reuse stale scoped results. A concurrent
catalog update produces a consistent page or an explicit stale/resync result.
An external edit and a restored catalog invalidate affected caches. Query timeout,
truncation and cancellation remain explicit failures, never an empty library.

## 6. Identity-preserving import and cutover

**Joint integration work; add Catabolic APIs only where existing import falls short.**
Import Motion's logical IDs where supported; retain explicit mappings where physical
occurrence identities differ. Catabolic already accepts caller-supplied item IDs.
Its scanned file IDs are derived from database/source/path identity, while Motion
has evidence-based move reconciliation. Do not change Catabolic's occurrence scheme
or merge duplicates merely to make IDs match. Preserve logical references through
an explicit, evidence-backed mapping/rebinding policy. [C2], [C13], [M6]

Migration must retain edition/timeline/version structure, metadata contributions,
accepted/manual identity decisions and relevant history. Audit every Motion foreign
key or stored reference to catalog entities, including playlists, preparations,
markers, progress and offline manifests. Manifests and program bundles can help,
but are not proof of complete preservation. Catabolic validates its known schema;
Motion must not add private tables to its database. Any required durable extension
needs an upstream-owned migration. [C7], [C14]

**Acceptance:** import a representative fixture, compare logical/catalog results
and application references, then interrupt/resume and rerun import without
duplication. Exercise rename,
same-path replacement, duplicate content and unavailable sources. An unavailable
source must not be interpreted as successful empty coverage and erase membership.
After cutover, mutations and scanning have one owner. Keep a backup and a tested
pre-cutover recovery path; do not imply rollback can discard later writes safely.

## 7. Background execution and output ownership

**Reuse first; conditional host hooks.** Existing native scan/processing entrypoints,
shared demands, cancellation, artifacts and recovery should remain the execution
owners. Motion can initially admit bounded work under a host permit with
an explicit worker limit. The upstream response notes that an observation request
can return pending while an isolated worker continues. Account for the worker's
actual lifetime: returning from the request must not release its resource budget
prematurely. Shutdown/recovery must retain ownership of continuing work.
A new per-child resource callback is needed only if that composition cannot
satisfy Motion's CPU/I/O/process limits or cancellation rules.
Document status, busy/retry semantics, shutdown and restart behavior. [C15]

If Catabolic owns generated outputs that Motion serves, establish how exact-revision
references remain valid while in use. First evaluate current publication/ownership
mechanisms. A durable external-consumer pin/lease is a conditional extension if
existing retention cannot protect active consumers; it is not a prerequisite for
read-only catalog adoption. Motion still owns playback delivery and its private
streaming cache.

**Acceptance:** cancellation of one demand does not cancel another consumer's
shared work; stale output is not published as current; interruption releases or
recovers resource ownership without orphaned children. No second scanner, recursive
scheduler or parallel artifact journal is introduced.

## Delivery sequence after upstream response

1. **Qualify native hosting first:** Motion aligns its toolchain/SQLite dependencies
   and builds the combined fixture through Kyoto. Catabolic supplies the supported
   lifecycle, helper and error contract. Retain exact source/build receipts.
2. **Settle and implement catalog semantics:** agree on media identity and interval
   rules, contribution resolution, reviewed mutations/receipts, host scope admission
   and ranked search. Reuse existing owners and move applicable Motion pure logic
   and fixtures upstream. Audit invalidation and define restore/cursor handling.
3. **Qualify cutover:** run resumable identity-preserving migration and application
   integration fixtures, then retire Motion's corresponding catalog writers.
4. **Defer conditional work:** general change feeds, finer resource hooks, partial
   component coverage and consumer leases require a demonstrated use case.

Track each requirement by its supported API/example, limits, migration impact and
acceptance evidence. The response establishes the work to do; it does not close
any acceptance gate by itself.

Place reusable identity, conflict, stale-result and replay decisions in Catabolic's
production core. Adapters should observe files, run tools and enforce database
transactions. Verify the same production decisions with bounded state/interleaving
tests, and separately test real rollback, restart, file revision and cancellation
behavior. Motion retains authorization and cross-owner orchestration decisions.
Report actual explored bounds and platform results, not just the presence of tests.

The current native migration records still distinguish failed aggregate runs from
later focused passes. This source review ran no new build or runtime tests and does
not certify macOS/Linux readiness. Integration builds should go through the existing
Kyoto build workflow and retain exact source/artifact identities. [C16]

## Disposition of the existing integration prototype

The prototype is a read-only inventory bridge and Motion projection. Its snapshot
fixtures and identity tests can inform qualification, but it does not offload the
logical catalog. Do not upstream its whole process wrapper or adopt its shadow
catalog as the final architecture. Its snapshot export should be compared against
existing session/query materialization before adding a new Catabolic API.

The prototype's macOS type-width fixes overlap fixes already in upstream. Its
Motion migration number `0045` also conflicts with advanced Motion main. Rebase
and reassess the prototype against the pinned revisions before reusing code.
This brief changes the intended destination from physical-inventory delegation to
full catalog ownership; it is not an assertion that the original I0–I2 work
implemented that destination.

## Source references

Links are pinned to the reviewed commits, not moving branch heads.

[C1]: https://github.com/Jagalite/catabolic/blob/e24cda655ba62654c1754d7607ad7e922325b96d/crates/catabolic-store/src/embedding.rs
[C2]: https://github.com/Jagalite/catabolic/blob/e24cda655ba62654c1754d7607ad7e922325b96d/crates/catabolic-store/src/curation.rs
[C3]: https://github.com/Jagalite/catabolic/blob/e24cda655ba62654c1754d7607ad7e922325b96d/docs/FALLBACK.md
[C4]: https://github.com/Jagalite/catabolic/blob/e24cda655ba62654c1754d7607ad7e922325b96d/docs/COMPONENTS.md
[C5]: https://github.com/Jagalite/catabolic/blob/e24cda655ba62654c1754d7607ad7e922325b96d/docs/TAGGING.md
[C6]: https://github.com/Jagalite/catabolic/blob/e24cda655ba62654c1754d7607ad7e922325b96d/docs/RUST_MIGRATION_QUERY_EMBEDDING.md
[C7]: https://github.com/Jagalite/catabolic/blob/e24cda655ba62654c1754d7607ad7e922325b96d/docs/MANIFESTS.md
[C8]: https://github.com/Jagalite/catabolic/blob/e24cda655ba62654c1754d7607ad7e922325b96d/Cargo.lock
[C9]: https://github.com/Jagalite/catabolic/blob/e24cda655ba62654c1754d7607ad7e922325b96d/crates/catabolic-store/src/rendition_mutation.rs
[C10]: https://github.com/Jagalite/catabolic/blob/e24cda655ba62654c1754d7607ad7e922325b96d/crates/catabolic-store/src/access.rs
[C11]: https://github.com/Jagalite/catabolic/blob/e24cda655ba62654c1754d7607ad7e922325b96d/crates/catabolic-store/src/fallback_resolution.rs
[C12]: https://github.com/Jagalite/catabolic/blob/e24cda655ba62654c1754d7607ad7e922325b96d/crates/catabolic-store/src/api_events.rs
[C13]: https://github.com/Jagalite/catabolic/blob/e24cda655ba62654c1754d7607ad7e922325b96d/crates/catabolic-store/src/observation.rs
[C14]: https://github.com/Jagalite/catabolic/blob/e24cda655ba62654c1754d7607ad7e922325b96d/crates/catabolic-store/src/migration.rs
[C15]: https://github.com/Jagalite/catabolic/blob/e24cda655ba62654c1754d7607ad7e922325b96d/crates/catabolic-runtime/src/public_processing.rs
[C16]: https://github.com/Jagalite/catabolic/blob/e24cda655ba62654c1754d7607ad7e922325b96d/docs/RUST_MIGRATION_MACOS_RESUMED_2026_10_13.md
[C17]: https://github.com/Jagalite/catabolic/blob/e24cda655ba62654c1754d7607ad7e922325b96d/crates/catabolic-store/resources/migrations/003_media_model.sql
[M1]: https://github.com/Jagalite/motion/blob/1e5a24675b059337b34e98f86b225178798cc080/Cargo.lock
[M2]: https://github.com/Jagalite/motion/blob/1e5a24675b059337b34e98f86b225178798cc080/crates/core/src/identity.rs
[M3]: https://github.com/Jagalite/motion/blob/1e5a24675b059337b34e98f86b225178798cc080/crates/core/src/metadata.rs
[M4]: https://github.com/Jagalite/motion/blob/1e5a24675b059337b34e98f86b225178798cc080/src/curation.rs
[M5]: https://github.com/Jagalite/motion/blob/1e5a24675b059337b34e98f86b225178798cc080/src/search.rs
[M6]: https://github.com/Jagalite/motion/blob/1e5a24675b059337b34e98f86b225178798cc080/crates/core/src/scan.rs
