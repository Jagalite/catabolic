-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors
-- SPDX-License-Identifier: MIT
-- REVIEW DRAFT. Apply only through the explicit schema-33 native migration owner.
-- No transaction boundary, PRAGMA, legacy-row UPDATE/DELETE, or legacy DDL here.
-- SQL integer checks deliberately reject SQLite overflow promotion to REAL.

CREATE TABLE catalog_state (
 id INTEGER PRIMARY KEY CHECK(id = 1),
 catalog_id TEXT NOT NULL UNIQUE CHECK(length(CAST(catalog_id AS BLOB)) BETWEEN 1 AND 256),
 incarnation TEXT NOT NULL UNIQUE CHECK(length(incarnation)=32 AND incarnation NOT GLOB '*[^0-9a-f]*'),
 generation INTEGER NOT NULL CHECK(typeof(generation) = 'integer' AND generation >= 0),
 contract_version INTEGER NOT NULL CHECK(contract_version = 1)
);
INSERT INTO catalog_state(id,catalog_id,incarnation,generation,contract_version)
 SELECT 1,catabolic_catalog_id_v1(value),lower(hex(randomblob(16))),0,1 FROM meta WHERE key='database_id';

-- A permit is visible only to its SQLite writer transaction. The deliberately
-- unsatisfied deferred FK makes a forgotten permit an uncommittable transaction.
-- This guards application invariants; privileged raw SQL is not an auth boundary.
CREATE TABLE catalog_commit_barrier(id INTEGER PRIMARY KEY CHECK(id = 1));
CREATE TRIGGER catalog_commit_barrier_no_insert BEFORE INSERT ON catalog_commit_barrier
 BEGIN SELECT RAISE(ABORT,'catalog_permit_barrier_must_remain_empty'); END;
CREATE TRIGGER catalog_commit_barrier_no_update BEFORE UPDATE ON catalog_commit_barrier
 BEGIN SELECT RAISE(ABORT,'catalog_permit_barrier_must_remain_empty'); END;
CREATE TABLE catalog_mutation_permits (
 scope_kind TEXT NOT NULL CHECK(scope_kind IN ('version','context','timeline','restore')),
 scope_id TEXT NOT NULL CHECK(length(CAST(scope_id AS BLOB)) BETWEEN 1 AND 256),
 incarnation TEXT NOT NULL,
 barrier_id INTEGER NOT NULL DEFAULT 1 CHECK(barrier_id = 1),
 PRIMARY KEY(scope_kind,scope_id),
 FOREIGN KEY(barrier_id) REFERENCES catalog_commit_barrier(id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER catalog_permit_current_incarnation BEFORE INSERT ON catalog_mutation_permits
 WHEN NEW.incarnation IS NOT (SELECT incarnation FROM catalog_state WHERE id=1)
 BEGIN SELECT RAISE(ABORT,'catalog_stale_incarnation'); END;
CREATE TRIGGER catalog_permit_no_update BEFORE UPDATE ON catalog_mutation_permits
 BEGIN SELECT RAISE(ABORT,'catalog_permit_immutable'); END;
CREATE TRIGGER catalog_state_no_delete BEFORE DELETE ON catalog_state
 BEGIN SELECT RAISE(ABORT,'catalog_state_required'); END;
CREATE TRIGGER catalog_state_guard BEFORE UPDATE ON catalog_state
 WHEN NEW.id IS NOT OLD.id OR NEW.catalog_id IS NOT OLD.catalog_id
   OR NEW.contract_version IS NOT OLD.contract_version OR NEW.generation < OLD.generation
   OR (NEW.incarnation IS NOT OLD.incarnation AND NOT EXISTS (
       SELECT 1 FROM catalog_mutation_permits WHERE scope_kind='restore'
       AND scope_id=OLD.catalog_id AND incarnation=OLD.incarnation))
 BEGIN SELECT RAISE(ABORT,'catalog_state_immutable_or_stale'); END;
CREATE TRIGGER catalog_identity_meta_update BEFORE UPDATE ON meta
 WHEN (OLD.key='database_id' OR NEW.key='database_id')
   AND (NEW.key IS NOT OLD.key OR NEW.value IS NOT OLD.value)
 BEGIN SELECT RAISE(ABORT,'catalog_identity_immutable'); END;
CREATE TRIGGER catalog_identity_meta_delete BEFORE DELETE ON meta WHEN OLD.key='database_id'
 BEGIN SELECT RAISE(ABORT,'catalog_identity_immutable'); END;

CREATE TABLE catalog_entity_revisions (
 entity_kind TEXT NOT NULL,
 entity_key TEXT NOT NULL CHECK(json_valid(entity_key) AND json_type(entity_key)='array'),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision >= 1),
 deleted INTEGER NOT NULL CHECK(deleted IN (0,1)),
 PRIMARY KEY(entity_kind,entity_key)
);

-- Immutable key/digest/envelope fields survive result compaction forever.
CREATE TABLE catalog_command_receipts (
 catalog_id TEXT NOT NULL REFERENCES catalog_state(catalog_id),
 host_namespace TEXT NOT NULL CHECK(length(CAST(host_namespace AS BLOB)) BETWEEN 1 AND 256),
 operation_id TEXT NOT NULL CHECK(length(CAST(operation_id AS BLOB)) BETWEEN 1 AND 256),
 contract_version INTEGER NOT NULL CHECK(contract_version=1),
 canonicalization_version INTEGER NOT NULL CHECK(canonicalization_version=1),
 payload_digest TEXT NOT NULL CHECK(length(payload_digest)=64 AND payload_digest NOT GLOB '*[^0-9a-f]*'),
 admission_incarnation TEXT NOT NULL CHECK(length(admission_incarnation)=32 AND admission_incarnation NOT GLOB '*[^0-9a-f]*'),
 expected_generation INTEGER NOT NULL CHECK(typeof(expected_generation)='integer' AND expected_generation >= 0),
 committed_generation INTEGER NOT NULL CHECK(typeof(committed_generation)='integer' AND committed_generation >= expected_generation),
 committed_at_us INTEGER NOT NULL CHECK(typeof(committed_at_us)='integer' AND committed_at_us >= 0),
 retain_until_us INTEGER NOT NULL CHECK(typeof(retain_until_us)='integer' AND retain_until_us >= committed_at_us),
 result_state TEXT NOT NULL CHECK(result_state IN ('full','expired')),
 result_json TEXT,
 expired_at_us INTEGER,
 PRIMARY KEY(catalog_id,host_namespace,operation_id),
 CHECK((result_state='full' AND result_json IS NOT NULL AND json_valid(result_json)
         AND length(CAST(result_json AS BLOB)) <= 4194304 AND expired_at_us IS NULL)
    OR (result_state='expired' AND result_json IS NULL AND typeof(expired_at_us)='integer'
         AND expired_at_us >= retain_until_us))
);
CREATE TABLE catalog_receipt_holds (
 catalog_id TEXT NOT NULL,
 host_namespace TEXT NOT NULL,
 operation_id TEXT NOT NULL,
 holder_namespace TEXT NOT NULL CHECK(length(CAST(holder_namespace AS BLOB)) BETWEEN 1 AND 256),
 hold_id TEXT NOT NULL CHECK(length(CAST(hold_id AS BLOB)) BETWEEN 1 AND 256),
 acquired_at_us INTEGER NOT NULL CHECK(typeof(acquired_at_us)='integer' AND acquired_at_us >= 0),
 released_at_us INTEGER CHECK(released_at_us IS NULL OR (typeof(released_at_us)='integer' AND released_at_us >= acquired_at_us)),
 PRIMARY KEY(catalog_id,host_namespace,operation_id,holder_namespace,hold_id),
 FOREIGN KEY(catalog_id,host_namespace,operation_id)
  REFERENCES catalog_command_receipts(catalog_id,host_namespace,operation_id)
);
CREATE INDEX catalog_receipt_active_holds ON catalog_receipt_holds(catalog_id,host_namespace,operation_id) WHERE released_at_us IS NULL;
CREATE TRIGGER catalog_receipt_initial_full BEFORE INSERT ON catalog_command_receipts
 WHEN NEW.result_state != 'full' OR NEW.admission_incarnation IS NOT (SELECT incarnation FROM catalog_state WHERE id=1)
   OR NEW.committed_generation IS NOT (SELECT generation FROM catalog_state WHERE id=1)
 BEGIN SELECT RAISE(ABORT,'catalog_receipt_invalid_commit'); END;
CREATE TRIGGER catalog_receipt_no_delete BEFORE DELETE ON catalog_command_receipts
 BEGIN SELECT RAISE(ABORT,'catalog_receipt_key_permanent'); END;
CREATE TRIGGER catalog_receipt_compaction_only BEFORE UPDATE ON catalog_command_receipts
 WHEN NEW.catalog_id IS NOT OLD.catalog_id OR NEW.host_namespace IS NOT OLD.host_namespace
 OR NEW.operation_id IS NOT OLD.operation_id OR NEW.contract_version IS NOT OLD.contract_version
 OR NEW.canonicalization_version IS NOT OLD.canonicalization_version OR NEW.payload_digest IS NOT OLD.payload_digest
 OR NEW.admission_incarnation IS NOT OLD.admission_incarnation OR NEW.expected_generation IS NOT OLD.expected_generation
 OR NEW.committed_generation IS NOT OLD.committed_generation OR NEW.committed_at_us IS NOT OLD.committed_at_us
 OR NEW.retain_until_us IS NOT OLD.retain_until_us OR OLD.result_state != 'full' OR NEW.result_state != 'expired'
 OR EXISTS (SELECT 1 FROM catalog_receipt_holds h WHERE h.catalog_id=OLD.catalog_id
  AND h.host_namespace=OLD.host_namespace AND h.operation_id=OLD.operation_id AND h.released_at_us IS NULL)
 BEGIN SELECT RAISE(ABORT,'catalog_receipt_immutable_or_held'); END;
CREATE TRIGGER catalog_hold_full_receipt BEFORE INSERT ON catalog_receipt_holds
 WHEN NOT EXISTS (SELECT 1 FROM catalog_command_receipts r WHERE r.catalog_id=NEW.catalog_id
  AND r.host_namespace=NEW.host_namespace AND r.operation_id=NEW.operation_id AND r.result_state='full')
 BEGIN SELECT RAISE(ABORT,'catalog_receipt_expired'); END;
CREATE TRIGGER catalog_hold_release_only BEFORE UPDATE ON catalog_receipt_holds
 WHEN NEW.catalog_id IS NOT OLD.catalog_id OR NEW.host_namespace IS NOT OLD.host_namespace
 OR NEW.operation_id IS NOT OLD.operation_id OR NEW.holder_namespace IS NOT OLD.holder_namespace
 OR NEW.hold_id IS NOT OLD.hold_id OR NEW.acquired_at_us IS NOT OLD.acquired_at_us
 OR OLD.released_at_us IS NOT NULL OR NEW.released_at_us IS NULL
 BEGIN SELECT RAISE(ABORT,'catalog_receipt_hold_immutable'); END;
CREATE TRIGGER catalog_hold_no_delete BEFORE DELETE ON catalog_receipt_holds
 BEGIN SELECT RAISE(ABORT,'catalog_receipt_hold_history_retained'); END;

-- These migration audit records intentionally have NO FK to legacy rows. An
-- unclassified association/item can still be deleted by its existing owner.
CREATE TABLE catalog_legacy_associations (
 association_id TEXT PRIMARY KEY NOT NULL,
 original_file_id TEXT NOT NULL,
 original_item_id TEXT NOT NULL,
 original_row_json TEXT NOT NULL CHECK(json_valid(original_row_json) AND json_type(original_row_json)='object'),
 classification TEXT NOT NULL DEFAULT 'unclassified' CHECK(classification='unclassified')
);
INSERT INTO catalog_legacy_associations(association_id,original_file_id,original_item_id,original_row_json)
 SELECT id,file_id,item_id,json_object('id',id,'file_id',file_id,'item_id',item_id,'role',role,
  'part',part,'metadata',metadata,'origin',origin,'active',active) FROM item_files;
CREATE TABLE catalog_legacy_item_metadata (
 item_id TEXT PRIMARY KEY NOT NULL,
 metadata_raw TEXT NOT NULL,
 provenance TEXT NOT NULL DEFAULT 'legacy_import' CHECK(provenance='legacy_import')
);
INSERT INTO catalog_legacy_item_metadata(item_id,metadata_raw) SELECT id,metadata FROM items;

CREATE TABLE catalog_identity_evidence (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(CAST(id AS BLOB)) BETWEEN 1 AND 256),
 basis TEXT NOT NULL CHECK(basis IN ('declared','verified','legacy_import')),
 evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json) AND json_type(evidence_json)='object'),
 recorded_at_us INTEGER NOT NULL CHECK(typeof(recorded_at_us)='integer' AND recorded_at_us >= 0),
 UNIQUE(id,basis)
);
-- The revision string is the existing Components v1 snapshot revision. The
-- trusted capture owner canonicalizes/recomputes it; SQL does not hash or infer.
CREATE TABLE catalog_exact_file_revisions (
 profile TEXT NOT NULL REFERENCES profiles(id),
 file_id TEXT NOT NULL REFERENCES files(id),
 revision TEXT NOT NULL CHECK(length(CAST(revision AS BLOB)) BETWEEN 1 AND 256),
 revision_contract_version INTEGER NOT NULL CHECK(revision_contract_version=1),
 snapshot_json TEXT NOT NULL CHECK(json_valid(snapshot_json) AND json_type(snapshot_json)='object'),
 captured_at_us INTEGER NOT NULL CHECK(typeof(captured_at_us)='integer' AND captured_at_us >= 0),
 PRIMARY KEY(profile,file_id,revision)
);

CREATE TABLE catalog_timelines (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(CAST(id AS BLOB)) BETWEEN 1 AND 256),
 edition_item_id TEXT NOT NULL REFERENCES items(id),
 current_revision INTEGER NOT NULL CHECK(typeof(current_revision)='integer' AND current_revision >= 1),
 FOREIGN KEY(id,current_revision) REFERENCES catalog_timeline_revisions(timeline_id,revision) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE catalog_timeline_revisions (
 timeline_id TEXT NOT NULL REFERENCES catalog_timelines(id),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision >= 1),
 identity_state TEXT NOT NULL CHECK(identity_state IN ('provisional','established')),
 coverage_start_us INTEGER,
 coverage_end_us INTEGER,
 recorded_at_us INTEGER NOT NULL CHECK(typeof(recorded_at_us)='integer' AND recorded_at_us >= 0),
 command_catalog_id TEXT NOT NULL,
 command_host_namespace TEXT NOT NULL,
 command_operation_id TEXT NOT NULL,
 PRIMARY KEY(timeline_id,revision),
 CHECK((coverage_start_us IS NULL AND coverage_end_us IS NULL) OR
   (typeof(coverage_start_us)='integer' AND typeof(coverage_end_us)='integer'
    AND coverage_start_us >= 0 AND coverage_end_us > coverage_start_us)),
 FOREIGN KEY(command_catalog_id,command_host_namespace,command_operation_id)
  REFERENCES catalog_command_receipts(catalog_id,host_namespace,operation_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE catalog_segmentation_contexts (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(CAST(id AS BLOB)) BETWEEN 1 AND 256),
 authority_namespace TEXT NOT NULL CHECK(length(CAST(authority_namespace AS BLOB)) BETWEEN 1 AND 256),
 context_key TEXT NOT NULL CHECK(length(CAST(context_key AS BLOB)) BETWEEN 1 AND 256),
 current_revision INTEGER NOT NULL CHECK(typeof(current_revision)='integer' AND current_revision >= 1),
 UNIQUE(authority_namespace,context_key),
 FOREIGN KEY(id,current_revision) REFERENCES catalog_segmentation_revisions(context_id,revision) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE catalog_segmentation_revisions (
 context_id TEXT NOT NULL REFERENCES catalog_segmentation_contexts(id),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision >= 1),
 recorded_at_us INTEGER NOT NULL CHECK(typeof(recorded_at_us)='integer' AND recorded_at_us >= 0),
 command_catalog_id TEXT NOT NULL,
 command_host_namespace TEXT NOT NULL,
 command_operation_id TEXT NOT NULL,
 PRIMARY KEY(context_id,revision),
 FOREIGN KEY(command_catalog_id,command_host_namespace,command_operation_id)
  REFERENCES catalog_command_receipts(catalog_id,host_namespace,operation_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE catalog_versions (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(CAST(id AS BLOB)) BETWEEN 1 AND 256),
 association_item_id TEXT NOT NULL UNIQUE REFERENCES items(id),
 segmentation_context_id TEXT NOT NULL REFERENCES catalog_segmentation_contexts(id),
 current_revision INTEGER NOT NULL CHECK(typeof(current_revision)='integer' AND current_revision >= 1),
 FOREIGN KEY(id,current_revision) REFERENCES catalog_version_revisions(version_id,revision) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE catalog_version_revisions (
 version_id TEXT NOT NULL REFERENCES catalog_versions(id),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision >= 1),
 timeline_id TEXT NOT NULL,
 timeline_revision INTEGER NOT NULL CHECK(typeof(timeline_revision)='integer' AND timeline_revision >= 1),
 state TEXT NOT NULL CHECK(state IN ('draft','ready')),
 coverage_start_us INTEGER,
 coverage_end_us INTEGER,
 equivalence_evidence_id TEXT,
 equivalence_basis TEXT CHECK(equivalence_basis IN ('declared','verified')),
 recorded_at_us INTEGER NOT NULL CHECK(typeof(recorded_at_us)='integer' AND recorded_at_us >= 0),
 command_catalog_id TEXT NOT NULL,
 command_host_namespace TEXT NOT NULL,
 command_operation_id TEXT NOT NULL,
 PRIMARY KEY(version_id,revision),
 CHECK((coverage_start_us IS NULL AND coverage_end_us IS NULL AND state='draft') OR
   (typeof(coverage_start_us)='integer' AND typeof(coverage_end_us)='integer'
    AND coverage_start_us >= 0 AND coverage_end_us > coverage_start_us)),
 CHECK((equivalence_evidence_id IS NULL AND equivalence_basis IS NULL) OR
       (equivalence_evidence_id IS NOT NULL AND equivalence_basis IS NOT NULL)),
 FOREIGN KEY(timeline_id,timeline_revision) REFERENCES catalog_timeline_revisions(timeline_id,revision),
 FOREIGN KEY(equivalence_evidence_id,equivalence_basis) REFERENCES catalog_identity_evidence(id,basis),
 FOREIGN KEY(command_catalog_id,command_host_namespace,command_operation_id)
  REFERENCES catalog_command_receipts(catalog_id,host_namespace,operation_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE catalog_bindings (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(CAST(id AS BLOB)) BETWEEN 1 AND 256),
 version_id TEXT NOT NULL REFERENCES catalog_versions(id),
 association_id TEXT NOT NULL UNIQUE REFERENCES item_files(id),
 UNIQUE(id,version_id)
);
CREATE TABLE catalog_binding_revisions (
 binding_id TEXT NOT NULL REFERENCES catalog_bindings(id),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision >= 1),
 profile TEXT NOT NULL,
 file_id TEXT NOT NULL,
 file_revision TEXT NOT NULL,
 part INTEGER NOT NULL CHECK(typeof(part)='integer' AND part BETWEEN 1 AND 4294967295),
 source_kind TEXT NOT NULL CHECK(source_kind IN ('whole_file','slice')),
 source_start_us INTEGER,
 source_end_us INTEGER,
 timeline_start_us INTEGER,
 timeline_end_us INTEGER,
 normalization_evidence_id TEXT REFERENCES catalog_identity_evidence(id),
 recorded_at_us INTEGER NOT NULL CHECK(typeof(recorded_at_us)='integer' AND recorded_at_us >= 0),
 command_catalog_id TEXT NOT NULL,
 command_host_namespace TEXT NOT NULL,
 command_operation_id TEXT NOT NULL,
 PRIMARY KEY(binding_id,revision),
 CHECK((source_kind='whole_file' AND source_start_us IS NULL AND source_end_us IS NULL
         AND timeline_start_us IS NULL AND timeline_end_us IS NULL)
    OR (typeof(source_start_us)='integer' AND typeof(source_end_us)='integer'
        AND source_start_us >= 0 AND source_end_us > source_start_us
        AND (source_kind='slice' OR source_start_us=0))),
 CHECK((timeline_start_us IS NULL AND timeline_end_us IS NULL AND source_kind='whole_file') OR
       (typeof(timeline_start_us)='integer' AND typeof(timeline_end_us)='integer'
        AND timeline_start_us >= 0 AND timeline_end_us > timeline_start_us
        AND timeline_end_us-timeline_start_us=source_end_us-source_start_us)),
 FOREIGN KEY(profile,file_id,file_revision) REFERENCES catalog_exact_file_revisions(profile,file_id,revision),
 FOREIGN KEY(command_catalog_id,command_host_namespace,command_operation_id)
  REFERENCES catalog_command_receipts(catalog_id,host_namespace,operation_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE catalog_version_bindings (
 version_id TEXT NOT NULL,
 version_revision INTEGER NOT NULL,
 binding_id TEXT NOT NULL,
 binding_revision INTEGER NOT NULL,
 PRIMARY KEY(version_id,version_revision,binding_id),
 FOREIGN KEY(version_id,version_revision) REFERENCES catalog_version_revisions(version_id,revision),
 FOREIGN KEY(binding_id,binding_revision) REFERENCES catalog_binding_revisions(binding_id,revision),
 FOREIGN KEY(binding_id,version_id) REFERENCES catalog_bindings(id,version_id)
);
CREATE TABLE catalog_segmentation_sets (
 id TEXT PRIMARY KEY NOT NULL CHECK(length(CAST(id AS BLOB)) BETWEEN 1 AND 256),
 context_id TEXT NOT NULL REFERENCES catalog_segmentation_contexts(id),
 profile TEXT NOT NULL,
 file_id TEXT NOT NULL,
 file_revision TEXT NOT NULL,
 UNIQUE(context_id,profile,file_id,file_revision),
 UNIQUE(id,context_id),
 FOREIGN KEY(profile,file_id,file_revision) REFERENCES catalog_exact_file_revisions(profile,file_id,revision)
);
CREATE TABLE catalog_segment_members (
 context_id TEXT NOT NULL,
 context_revision INTEGER NOT NULL,
 set_id TEXT NOT NULL,
 binding_id TEXT NOT NULL,
 binding_revision INTEGER NOT NULL,
 PRIMARY KEY(context_id,context_revision,binding_id),
 FOREIGN KEY(context_id,context_revision) REFERENCES catalog_segmentation_revisions(context_id,revision),
 FOREIGN KEY(set_id,context_id) REFERENCES catalog_segmentation_sets(id,context_id),
 FOREIGN KEY(binding_id,binding_revision) REFERENCES catalog_binding_revisions(binding_id,revision)
);
CREATE INDEX catalog_binding_revision_files ON catalog_binding_revisions(profile,file_id,file_revision);
CREATE INDEX catalog_version_timeline ON catalog_version_revisions(timeline_id,timeline_revision);
CREATE INDEX catalog_segment_set_members ON catalog_segment_members(set_id,context_id,context_revision);

CREATE TRIGGER catalog_segment_members_consistent BEFORE INSERT ON catalog_segment_members
 WHEN NOT EXISTS (
  SELECT 1 FROM catalog_binding_revisions r JOIN catalog_bindings b ON b.id=r.binding_id
   JOIN catalog_versions v ON v.id=b.version_id JOIN catalog_segmentation_sets s ON s.id=NEW.set_id
  WHERE r.binding_id=NEW.binding_id AND r.revision=NEW.binding_revision AND r.source_kind='slice'
   AND s.profile=r.profile AND s.file_id=r.file_id AND s.file_revision=r.file_revision
   AND s.context_id=NEW.context_id AND v.segmentation_context_id=NEW.context_id)
 BEGIN SELECT RAISE(ABORT,'catalog_segmentation_binding_mismatch'); END;
CREATE TRIGGER catalog_segment_members_nonoverlap BEFORE INSERT ON catalog_segment_members
 WHEN EXISTS (
  SELECT 1 FROM catalog_segment_members m
   JOIN catalog_binding_revisions old ON old.binding_id=m.binding_id AND old.revision=m.binding_revision
   JOIN catalog_binding_revisions proposed ON proposed.binding_id=NEW.binding_id AND proposed.revision=NEW.binding_revision
  WHERE m.context_id=NEW.context_id AND m.context_revision=NEW.context_revision AND m.set_id=NEW.set_id
   AND old.source_start_us < proposed.source_end_us AND proposed.source_start_us < old.source_end_us)
 BEGIN SELECT RAISE(ABORT,'catalog_identity_overlapping_coverage'); END;

-- Claims are derived from typed binding identities; historical legacy audit rows
-- are not overwritten and are never mistaken for a typed authority.
CREATE VIEW catalog_legacy_association_classification AS
 SELECT a.association_id,a.original_file_id,a.original_item_id,a.original_row_json,
  CASE WHEN b.id IS NULL THEN 'unclassified' ELSE 'claimed' END AS classification,
  b.id AS binding_id,b.version_id,
  CASE WHEN live.id IS NULL THEN 0 ELSE 1 END AS legacy_row_present
 FROM catalog_legacy_associations a LEFT JOIN catalog_bindings b ON b.association_id=a.association_id
 LEFT JOIN item_files live ON live.id=a.association_id;

CREATE VIEW catalog_active_binding_revisions AS
 SELECT v.id AS version_id,v.current_revision AS version_revision,v.association_item_id,
  v.segmentation_context_id,vr.timeline_id,vr.timeline_revision,t.edition_item_id,vr.state,
  b.id AS binding_id,b.association_id,br.revision AS binding_revision,
  br.profile,br.file_id,br.file_revision,br.part,br.source_kind,
  br.source_start_us,br.source_end_us,br.timeline_start_us,br.timeline_end_us
 FROM catalog_versions v
 JOIN catalog_version_revisions vr ON vr.version_id=v.id AND vr.revision=v.current_revision
 JOIN catalog_timelines t ON t.id=vr.timeline_id
 JOIN catalog_version_bindings m ON m.version_id=v.id AND m.version_revision=v.current_revision
 JOIN catalog_bindings b ON b.id=m.binding_id AND b.version_id=v.id
 JOIN catalog_binding_revisions br ON br.binding_id=b.id AND br.revision=m.binding_revision;
CREATE TRIGGER catalog_state_no_replace BEFORE INSERT ON catalog_state
 WHEN EXISTS (SELECT 1 FROM catalog_state)
 BEGIN SELECT RAISE(ABORT,'catalog_state_immutable'); END;
CREATE TRIGGER catalog_identity_meta_no_replace BEFORE INSERT ON meta
 WHEN NEW.key='database_id' AND EXISTS (SELECT 1 FROM meta WHERE key=NEW.key AND value IS NOT NEW.value)
 BEGIN SELECT RAISE(ABORT,'catalog_identity_immutable'); END;
