# Host-coordinated bounded import partitions

This extension preserves the existing per-import bounds. A host may partition a
larger immutable source snapshot, but sequential successful imports alone do not
prove a complete migration or permit server admission.

The host must durably retain a complete ordered partition manifest, source freeze,
reference audit, exact activation envelopes, and each checkpoint. Its manifest
SHA-256 digest and exhaustive expected import-ID set are trusted host inputs to
`RecordedImportSet::from_durable_manifest`, not user request fields. The constructor
rejects missing, duplicate, unexpected, or nonuniform catalog/incarnation records.
The aggregate runtime restart contract is version 2 and currently bounds the
complete set at 4096 partitions, separately from the unchanged batch limits.

`HostSession::open_after_import_cutovers` retains one native writer lifetime while
reconciling every saved activation envelope with its durable receipt/checkpoint.
It constructs a new native-only gate only when every partition admits. A host
checkpoint needing completion or any closed outcome leaves admission closed.
The expected manifest digest must match. Native receipts cannot independently
prove that the host supplied the complete original source inventory; that remains
an explicit host responsibility. Lost replies replay saved envelopes, not freshly
minted activation intents. Normal receipt/hold storage budgets still apply.

## Empty source snapshots

Manifest version 3 explicitly permits zero batch descriptors. Versions 1 and 2
still reject an empty manifest. No fabricated item or empty nonzero batch is used.
Empty imports still require preparation, validation, the real drained gate and
reference acknowledgment, activation receipt, and restart reconciliation.

## Cross-partition endpoints

`capture_existing_item` captures exact catalog/incarnation/item revision for an ID
in an independently allowed host set. `TypedAuthority::existing_items` is not
snapshot data. The host must derive its allowed set from earlier reconciled
partition receipts, not from arbitrary native reads. Application rejects changed,
missing, retired or overlapping new-item grants. Existing endpoint authority does
not authorize an existing relationship or metadata overwrite.

## Additional typed owners

- Tag sources preserve their source namespace and source revision, canonicalize
  names through the existing tag owner, and create actual native assertions.
  Exclusions execute the native suppression owner. Each namespace/item grant is
  independent; a namespace named `local` does not itself authorize suppression.
  Original revisions are evidence, not invented native revision history.
- Relationships preserve source IDs/revisions, exact kind, position and metadata.
  Every directed edge requires an independent grant. The existing curation owner
  validates semantics and cycles. Existing edges reject instead of being silently
  upserted. Hosts must not normalize source ordering without an explicit mapping.
- Logical aliases retain the original serialized source merge receipt and execute
  the native reviewed logical merge owner. Historical source rows remain; the real
  logical retirement and reference map are created. Exact alias pairs and complete
  affected item scope require independent grants. Active source-side relationships,
  untyped associations or versions requiring replacement refuse the import rather
  than guessing replacements. These cases need a richer explicit source mapping.

All mappings rehearse in rollback and are checked again during activation. These
contracts describe implementation, not proof that an external application's full
migration, artifact ownership, or source cleanup has been qualified. Test reports
must identify the actual source snapshot tested.

## Interactive host follow-through

`item_create_batch` is a durable create-only command, distinct from the existing
upsert command. It requires 1..128 explicit unique IDs and rejects all existing-ID
collisions before applying any item. Replays return the original receipt.

`metadata_initialize_and_patch` composes the existing metadata initialization and
source-patch owners inside one command transaction and receipt. It requires both
an explicit initialization grant and an exact source grant. The complete supplied
field policies must preserve and validate every legacy field. Initialization
retains the original legacy document; the first source patch uses revision zero.
A failed patch rolls back initialization. Existing managed items use the ordinary
metadata patch with their exact source revision and schema fingerprint instead.
`read_source_head` supplies only current fenced fingerprint/revision facts for an
independently authorized subject, not source documents or request-derived grants.

## Append-only identity continuation

An import partition may append new versions to an exact previously activated
context via `existing_contexts` grants, and reference an unchanged timeline via
`existing_timelines` grants. The host derives allowed IDs from earlier committed
receipt mappings; capture helpers then bind current catalog/incarnation, context
namespace/key and revision, or timeline edition owner and revision. Replacement
of existing timeline/version/binding heads remains disallowed. The native identity
owner revalidates the full retained context and all unchanged members; the source
snapshot cannot drop them. Existing per-context64-version/1024-binding limits are
unchanged. Exact physical snapshots may repeat across distinct imports when new
version associations are created, without inventing occurrence or association
identity.

## Fresh observed media

`fresh_media_discovery` is a separate ordinary durable command, not a cutover or
import receipt. Independent host grants bind the exact newly allocated item IDs,
physical file scope, identity context scope and directed relationship edges.
Create-only item ownership, exact occurrence revalidation, associations and native
identity mutation commit atomically; association origin is the existing `explicit` owner classification; the
`fresh_media_discovery` receipt retains the observed-discovery provenance.
Unmeasured originals remain draft with no time placement. A measured-original
grant binds exact file revision and positive integral-microsecond duration. Only
one whole-file binding on its own new provisional timeline may become ready, with
source extent, timeline interval and all coverage exactly `[0,duration]`. It does
not assert equivalence to any other version. These are trusted probe grants, not
proof supplied by a request or inferred from a filename.

Measured discovery also binds the native observation's exact ctime, rechecked
against the current stat tuple at commit. The receipt retains the measured file,
ctime and duration facts. This does not redefine `ExactFileRevision`, which does
not include ctime: a host must keep its durable full probe fingerprint and deny
playback/projection reuse after a ctime-only replacement. Ready identity alone is
not evidence that a later physical file still matches the measured probe.
