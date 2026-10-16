# Native identity v1 implementation contract

This extends the native catalog design. The privileged pre-schema R1 contract
is locally qualified by the checkpoint below. This is not a schema migration or
a claim that the remaining R2–R7 and external integration gates have passed.

## Composition and authority

Each version owns a dedicated ordinary association item. Its complete primary
association set is the version's unit of selection. Existing uniqueness by item,
role and part remains intact: alternative two-part versions have different
association items, rather than two competing part-1 rows on one item.

The core identity contract validates integer microsecond coordinates, complete
ready-version coverage, exact revision evidence, distinct primary files and
explicit segmentation. The store resolves all requested versions' complete part
sets in one existing query/component/fallback operation, preserving its aggregate
work and probe budgets. It then chooses the first wholly available version in
preference order. An unavailable part cannot be filled from another version.
Preference does not change the timeline identity.

Before persistent typed ownership exists, supplied version manifests are a
privileged host proof boundary. They do not create durable timeline authority or
authorize an application user. The bridge must compare every declared binding
against the full active database association set and current revision evidence.
R2 replaces supplied authoritative facts with transactional typed records and
retained revisions; there will not be two independently writable owners.

The pre-schema manifest is one explicit editorial segmentation context. Within
it, one exact file revision has at most one segmentation set, including all its
declared episode slices. Splitting overlapping uses into differently named sets
is refused. This bounded contract does not establish nonoverlap across separate
manifests or a global prohibition on deliberate reuse. The persistent owner must
retain explicit context identity and load its complete active membership before
admitting a change; an untrusted caller cannot evade that check by choosing a
fresh set ID.

## Component and effect boundary

`component::compatible_binding` is the additive native compatibility path. It
first validates the whole version, association item, part and exact video
revision. Internal tracks inherit the selected binding's source and timeline
intervals, edition and stable timeline identity. Legacy `compatible` keeps its
container-derived identity behavior.

An external component needs the existing full, declared-or-verified exact-video
claim plus native `coordinate_mapping` version 1. Its checked source and timeline
intervals have equal duration; the timeline interval must exactly cover the
selected part. Partial coverage, another edition/timeline, stale video revision,
unknown mapping versions and speed transforms are refused. Native compatibility
results carry integer `coordinate_offset_us` and omit the independent legacy
floating-point offset. Legacy assertion normalization is not widened implicitly;
persistent native mapping admission remains work for the typed owner.

Metadata compatibility is not media execution readiness. Legacy packaging refuses
native binding relations with `native_binding_effect_unsupported`. A selected
slice must not expose the entire backing file as a ready output. No publication,
mux or playback effect gains permission from this metadata contract; interval
execution requires its own qualified adapter and existing safety gates.

## Schema-32 backfill and adapter decision

The initial upgrade will preserve every existing item, association, relationship,
file identity and history row in place. It will not rehome associations, add
fabricated version items, infer timelines from duration/digest/title, or relabel
unknown provenance as a manual or verified decision.

New additive mapping records identify legacy associations as unclassified until
an explicit reviewed classification supplies enough evidence for typed ownership.
Unknown metadata provenance becomes retained `legacy_import` evidence. Legacy
rows remain readable in their documented shape. A later classification must
record original association IDs and owners, exact revision evidence and explicit
identity mappings in its atomic receipt. Any transformation of old rows requires
a separately tested preservation rule; the migration owner's existing whole-row
preservation check must not be weakened globally.

Once a version claims associations, all mutation entrypoints must enforce its
typed owner. A legacy request that cannot express the whole version or slice is
refused with an explicit unsupported/upgrade contract outcome. A raw privileged
SQL connection is not an application mutation API.

## Compatibility and outstanding evidence

Historical schema 1–32 SQL/checksums and their parity fixtures remain frozen.
The new identity and component semantics are versioned changes, not Python parity.
The catalog contract ledger records their mapping and pending evidence.

Required R1 evidence includes real query and component owner composition, complete
alternative multipart selection, unknown encodes on separate provisional
timelines, two episode slices from one revision, segmentation overlap refusal,
stale revision refusal and a blocked partial external claim. Pure interval tests
alone cannot satisfy this gate. External Motion integration remains deferred by
the user's sequencing amendment and is not tested by this phase.

## 2026-10-14 Linux checkpoint

The frozen core/store/runtime aggregate passed **623 executions, zero failures,
seven ignored entries**. The ignored real-helper scan/cancel/drain/reopen test
was then covered by its separate passing run; six historical ignored obligations
remain. Strict all-target Clippy and formatting passed. Independent core and
store review tests passed after correcting segmentation, aggregate-budget and
legacy media-effect admission gaps.

The [checkpoint receipt](../tests/parity/native-catalog-r1-checkpoint.json) records
commands, retained failed development runs, helper identity and limitations. All
705 source files in its manifest remained unchanged across the final run; the
canonical manifest SHA-256 is
`c3de8ac8f635321af262527c6d64f0219d0cc1f42e1bbf654d4d008fa150b392`.
This is standard Rust 1.99 Linux evidence under the authorized runner exception,
not Kyoto, another platform, a full-workspace release or an M0–M11 gate closure.
