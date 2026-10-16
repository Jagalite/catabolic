# Independent review lanes, October 16, 2026

The `schema`, `broad_definition` and `filesystem` lanes previously stopped
because the reviewer tool returned an automated platform error
(`tests/parity/native-catalog-schema/schema-review-blocked.json`,
`tests/parity/native-m4-plus-checkpoint2.json`). Nobody declined them. The
user's October 16 instruction to finish the remaining reviews authorized
running them.

GPT-6.1-Sol reviewed commit `e8f0c520e93383628c47a1ac4aa913523e6bea0f`
statically, at extra-high reasoning effort, with read-only access. It ran no
builds, tests, SQL, fault injection or mount scenarios. The findings below are
deductions from the source. They do not qualify any milestone.

Status values: **open**, **fixed** (with a commit), or **accepted** (with a
reason).

## Schema lane

Reviewed: migrations 033–037, schema-33 trigger generation, identity
admission, migration signatures and checksums, plans, quiescence, backups,
rehearsal, locking, WAL rejection and commit boundaries.

| ID | Sev | Finding | Status |
| --- | --- | --- | --- |
| SC1 | P1 | Item and file suppression heads cascade on subject delete, but their history survives. A recreated ID then collides at revision 1 (`034_native_curation.sql:248,257`). | fixed: `9f9f05c` (schema 38) |
| SC2 | P2 | Metadata subjects and history have command-key columns but no foreign key to `catalog_command_receipts`, so orphaned provenance can commit (`034_native_curation.sql:23,35`). | fixed: `68af975` (schema 38) |
| SC3 | P2 | Schema-34 pin, tag and retirement histories accept inserts that reference an already-sealed receipt (`034_native_curation.sql:461`). | fixed: `13b7dcc` (schema 38) |
| SC4 | P2 | A NULL `cleanup_kind` bypasses the terminal-state CHECK in execution cleanup (`036_native_execution_cleanup.sql:21`). | fixed: `30d54e2` (schema 38) |
| SC5 | P2 | Newly created backup-root ancestors are not fsynced in their parent directories (`migration.rs:698`). | fixed: `1021c31` |

## Broad-definition lane

Reviewed: the M4 owners for definitions, curation, programs, plans,
interchange, manifests, layouts and layout application, against frozen
Python.

| ID | Sev | Finding | Status |
| --- | --- | --- | --- |
| BD1 | P1 | Bundle import walks keys in sorted order, while Python walks them in insertion order. Revision assignment for same-named records therefore differs (`programs.rs:669`). | fixed: `2009b0d` |
| BD2 | P1 | Copy-policy requirements and preferences use JSON `Value` equality, not Python numeric equality. For example, `1080` and `1080.0` compare unequal (`layout.rs:1055,1082`). | fixed: `82d1306` |
| BD3 | P1 | Package-policy bundles omit `package.requirements[].fallbacks[]` queries and `package.operation_id`, and don't remap them (`programs.rs:212,359`). Python shares this defect, which contradicts the documented dependency-closed bundles. | fixed: `3a655a9` |
| BD4 | P2 | Supplied layout options of the wrong type (for example, null `normalization` or boolean `direction`) silently become defaults (`layout.rs:174,258`). | fixed: `0d0d6f6` |
| BD5 | P2 | Inline SQL layout admission does not validate parameter names (`layout.rs:180`, `selection.rs:1251`). | fixed: `4799291` |
| BD6 | P2 | Integer formatting rejects metadata integers outside `i64` that Python renders (`layout.rs:341`). | fixed: `1626ea3` |

## Filesystem lane

Reviewed: filesystem and hardlink mutation, source, observation, remount,
watcher, content access, artifact usability, and the native event backend,
against frozen Python.

| ID | Sev | Finding | Status |
| --- | --- | --- | --- |
| FS1 | P0 | Temporary and claim-marker cleanup unlinks by pathname after only a regular-file check, so it can delete a substituted foreign file. This includes release recovery (`filesystem_mutation.rs:552,556,1534`). | fixed: claims never create or delete temporaries; see [hardening](NATIVE_INTEGRITY_HARDENING.md) |
| FS2 | P0 | Hardlink creation validates a pinned descriptor, then `linkat` re-resolves the source name. A substituted file or symlink becomes visible output (`hardlink_mutation.rs:455,479`). | fixed: staged, verified, exclusive publication; see [hardening](NATIVE_INTEGRITY_HARDENING.md) |
| FS3 | P0 | Content snapshots omit ctime, so an edit with a restored mtime keeps the old revision. Direct serving of generated content does not enforce the artifact digest (`content_access.rs:135`, `source.rs:211`). Inherited from Python. | partly fixed: generated outputs are digest-verified on coarse filesystems. Source ctime remains open (A5) |
| FS4 | P0 | Unrecognized orphaned private sidecars (rollback journals, zeroed headers) remain serveable (`content_access.rs:20,199`). This blocker was already known. | open |
| FS5 | P1 | Recovery that finds the intended postcondition skips the parent-directory sync before acknowledging (`filesystem_mutation.rs:1342`, `hardlink_mutation.rs:564`). | fixed: ancestry is fsynced before every acknowledgement |
| FS6 | P1 | A pinned directory descriptor moved outside the owned root stays usable. Source validation reopens the leaf through the old parent (`filesystem_mutation.rs:283`, `source.rs:217`). | fixed: containment re-walked after effects and reads |
| FS7 | P1 | Incomplete event subscriptions re-dirty sources on every poll, which fences progressing scans (`source_events/platform.rs:122,192`, `source_events.rs:187`). | fixed: coverage transitions invalidate once; degraded sources reconcile every 30 s, only after the previous scan finishes; see [hardening](NATIVE_INTEGRITY_HARDENING.md) |
| FS8 | P1 | Watcher notifications duplicate when the process crashes between event commit and `finish` (`watcher.rs:987,1018`). Python shares this. | fixed: event admission commits with `finish`; an unchanged retry reuses the planned run identity; see [hardening](NATIVE_INTEGRITY_HARDENING.md) |

The lane also proposed a protected-namespace replace/remove protocol for the
gated destructive paths. It also proposed versioned content revisions,
including a digest of an immutable representation, for weak-timestamp
filesystems. Both designs are tracked in
[the work plan](NATIVE_RETIREMENT_WORK_PLAN.md), tracks A and C.
