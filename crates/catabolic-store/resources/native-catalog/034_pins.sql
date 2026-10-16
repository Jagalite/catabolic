-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors
-- SPDX-License-Identifier: MIT
-- ADDITIVE SCHEMA-34 FRAGMENT. The schema migration owner executes this in its
-- transaction. No PRAGMA, transaction boundary, legacy tuple mutation, or receipt
-- creation. This fragment is NOT independent schema-33 review clearance.

-- Snapshot all old tuples exactly. No FK: provenance survives subject deletion.
-- Rows written later by legacy item_put also have unknown origin. The legacy
-- identities table has no authenticated provider/manual provenance to infer.
CREATE TABLE catalog_legacy_identity_evidence (
 namespace TEXT NOT NULL,
 value TEXT NOT NULL,
 item_id TEXT NOT NULL,
 provenance TEXT NOT NULL DEFAULT 'legacy_import' CHECK(provenance='legacy_import'),
 PRIMARY KEY(namespace,value,item_id)
);
INSERT INTO catalog_legacy_identity_evidence(namespace,value,item_id)
 SELECT namespace,value,item_id FROM identities;
CREATE TRIGGER catalog_pin_legacy_evidence_no_update BEFORE UPDATE ON catalog_legacy_identity_evidence
 BEGIN SELECT RAISE(ABORT,'catalog_pin_legacy_evidence_immutable'); END;
CREATE TRIGGER catalog_pin_legacy_evidence_no_delete BEFORE DELETE ON catalog_legacy_identity_evidence
 BEGIN SELECT RAISE(ABORT,'catalog_pin_legacy_evidence_retained'); END;
CREATE TRIGGER catalog_pin_capture_legacy_insert AFTER INSERT ON identities
 BEGIN INSERT INTO catalog_legacy_identity_evidence(namespace,value,item_id)
 VALUES(NEW.namespace,NEW.value,NEW.item_id) ON CONFLICT DO NOTHING; END;
CREATE TRIGGER catalog_pin_capture_legacy_update AFTER UPDATE ON identities
 BEGIN INSERT INTO catalog_legacy_identity_evidence(namespace,value,item_id)
 VALUES(NEW.namespace,NEW.value,NEW.item_id) ON CONFLICT DO NOTHING; END;

-- Every explicit decision, including removal, has a retained head and history.
-- No subject FK: this audit is not owned by legacy cascading deletion paths.
CREATE TABLE catalog_identity_pins (
 item_id TEXT NOT NULL CHECK(length(CAST(item_id AS BLOB)) BETWEEN 1 AND 256),
 namespace TEXT NOT NULL CHECK(length(CAST(namespace AS BLOB)) BETWEEN 1 AND 256),
 current_revision INTEGER NOT NULL CHECK(typeof(current_revision)='integer' AND current_revision>=1),
 value TEXT CHECK(value IS NULL OR length(CAST(value AS BLOB)) BETWEEN 1 AND 1024),
 PRIMARY KEY(item_id,namespace),
 FOREIGN KEY(item_id,namespace,current_revision)
 REFERENCES catalog_identity_pin_revisions(item_id,namespace,revision) DEFERRABLE INITIALLY DEFERRED
);
CREATE UNIQUE INDEX catalog_identity_pin_unique_value ON catalog_identity_pins(namespace,value)
 WHERE value IS NOT NULL;
CREATE TABLE catalog_identity_pin_revisions (
 item_id TEXT NOT NULL CHECK(length(CAST(item_id AS BLOB)) BETWEEN 1 AND 256),
 namespace TEXT NOT NULL CHECK(length(CAST(namespace AS BLOB)) BETWEEN 1 AND 256),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=1),
 value TEXT CHECK(value IS NULL OR length(CAST(value AS BLOB)) BETWEEN 1 AND 1024),
 recorded_at_us INTEGER NOT NULL CHECK(typeof(recorded_at_us)='integer' AND recorded_at_us>=0),
 command_catalog_id TEXT NOT NULL,
 command_host_namespace TEXT NOT NULL,
 command_operation_id TEXT NOT NULL,
 PRIMARY KEY(item_id,namespace,revision),
 FOREIGN KEY(item_id,namespace) REFERENCES catalog_identity_pins(item_id,namespace) DEFERRABLE INITIALLY DEFERRED,
 FOREIGN KEY(command_catalog_id,command_host_namespace,command_operation_id)
 REFERENCES catalog_command_receipts(catalog_id,host_namespace,operation_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER catalog_pin_revision_insert_guard BEFORE INSERT ON catalog_identity_pin_revisions
 WHEN NOT EXISTS(SELECT 1 FROM items WHERE id=NEW.item_id)
 OR NEW.command_catalog_id IS NOT (SELECT catalog_id FROM catalog_state WHERE id=1)
 OR NEW.revision IS NOT COALESCE((SELECT current_revision+1 FROM catalog_identity_pins
                                WHERE item_id=NEW.item_id AND namespace=NEW.namespace),1)
 OR (NEW.revision=1 AND NEW.value IS NULL)
 BEGIN SELECT RAISE(ABORT,'catalog_pin_invalid_revision'); END;
CREATE TRIGGER catalog_pin_revision_no_update BEFORE UPDATE ON catalog_identity_pin_revisions
 BEGIN SELECT RAISE(ABORT,'catalog_pin_revision_immutable'); END;
CREATE TRIGGER catalog_pin_revision_no_delete BEFORE DELETE ON catalog_identity_pin_revisions
 BEGIN SELECT RAISE(ABORT,'catalog_pin_history_retained'); END;
CREATE TRIGGER catalog_pin_head_insert_guard BEFORE INSERT ON catalog_identity_pins
 WHEN NEW.current_revision!=1 OR NEW.value IS NULL
 OR NOT EXISTS(SELECT 1 FROM items WHERE id=NEW.item_id)
 OR NOT EXISTS(SELECT 1 FROM catalog_identity_pin_revisions r WHERE r.item_id=NEW.item_id
   AND r.namespace=NEW.namespace AND r.revision=NEW.current_revision AND r.value IS NEW.value)
 BEGIN SELECT RAISE(ABORT,'catalog_pin_invalid_head'); END;
CREATE TRIGGER catalog_pin_head_update_guard BEFORE UPDATE ON catalog_identity_pins
 WHEN NEW.item_id IS NOT OLD.item_id OR NEW.namespace IS NOT OLD.namespace
 OR NEW.current_revision IS NOT OLD.current_revision+1 OR NEW.value IS OLD.value
 OR NOT EXISTS(SELECT 1 FROM catalog_identity_pin_revisions r WHERE r.item_id=NEW.item_id
   AND r.namespace=NEW.namespace AND r.revision=NEW.current_revision AND r.value IS NEW.value)
 BEGIN SELECT RAISE(ABORT,'catalog_pin_invalid_head'); END;
CREATE TRIGGER catalog_pin_head_no_delete BEFORE DELETE ON catalog_identity_pins
 BEGIN SELECT RAISE(ABORT,'catalog_pin_tombstone_retained'); END;
-- No reviewed item retirement owner exists in this increment. Even removed pins
-- retain subject management: legacy delete/recreate cannot erase their CAS head.
CREATE TRIGGER catalog_pin_subject_delete_guard BEFORE DELETE ON items
 WHEN EXISTS(SELECT 1 FROM catalog_identity_pins WHERE item_id=OLD.id)
 BEGIN SELECT RAISE(ABORT,'catalog_pin_subject_retirement_required'); END;
CREATE TRIGGER catalog_pin_subject_rekey_guard BEFORE UPDATE OF id ON items
 WHEN NEW.id IS NOT OLD.id AND EXISTS(SELECT 1 FROM catalog_identity_pins WHERE item_id=OLD.id)
 BEGIN SELECT RAISE(ABORT,'catalog_pin_subject_retirement_required'); END;

-- Explicit generation/entity-revision coverage for every new mutable table.
-- Immutable tables can INSERT only; heads can INSERT or advance a revision.
-- Existing schema-33 identities/items triggers cover changes to legacy rows.
INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
 SELECT 'catalog_legacy_identity_evidence',json_array(namespace,value,item_id),1,0 FROM catalog_legacy_identity_evidence;
CREATE TRIGGER catalog_change_catalog_legacy_identity_evidence_insert AFTER INSERT ON catalog_legacy_identity_evidence
 BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807
 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM catalog_entity_revisions WHERE entity_kind='catalog_legacy_identity_evidence'
 AND entity_key=json_array(NEW.namespace,NEW.value,NEW.item_id) AND revision=9223372036854775807)
 THEN RAISE(ABORT,'catalog_entity_revision_exhausted') END;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
 VALUES('catalog_legacy_identity_evidence',json_array(NEW.namespace,NEW.value,NEW.item_id),1,0) ON CONFLICT(entity_kind,entity_key)
 DO UPDATE SET revision=revision+1,deleted=0;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 END;
CREATE TRIGGER catalog_change_catalog_identity_pins_insert AFTER INSERT ON catalog_identity_pins
 BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807
 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM catalog_entity_revisions WHERE entity_kind='catalog_identity_pins'
 AND entity_key=json_array(NEW.item_id,NEW.namespace) AND revision=9223372036854775807)
 THEN RAISE(ABORT,'catalog_entity_revision_exhausted') END;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
 VALUES('catalog_identity_pins',json_array(NEW.item_id,NEW.namespace),1,0) ON CONFLICT(entity_kind,entity_key)
 DO UPDATE SET revision=revision+1,deleted=0;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 END;
CREATE TRIGGER catalog_change_catalog_identity_pins_update AFTER UPDATE ON catalog_identity_pins
 BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807
 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM catalog_entity_revisions WHERE entity_kind='catalog_identity_pins'
 AND entity_key=json_array(NEW.item_id,NEW.namespace) AND revision=9223372036854775807)
 THEN RAISE(ABORT,'catalog_entity_revision_exhausted') END;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
 VALUES('catalog_identity_pins',json_array(NEW.item_id,NEW.namespace),1,0) ON CONFLICT(entity_kind,entity_key)
 DO UPDATE SET revision=revision+1,deleted=0;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 END;
CREATE TRIGGER catalog_change_catalog_identity_pin_revisions_insert AFTER INSERT ON catalog_identity_pin_revisions
 BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807
 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
 SELECT CASE WHEN EXISTS(SELECT 1 FROM catalog_entity_revisions WHERE entity_kind='catalog_identity_pin_revisions'
 AND entity_key=json_array(NEW.item_id,NEW.namespace,NEW.revision) AND revision=9223372036854775807)
 THEN RAISE(ABORT,'catalog_entity_revision_exhausted') END;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
 VALUES('catalog_identity_pin_revisions',json_array(NEW.item_id,NEW.namespace,NEW.revision),1,0) ON CONFLICT(entity_kind,entity_key)
 DO UPDATE SET revision=revision+1,deleted=0;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 END;
-- Migration-only backfill preserves the pre-upgrade catalog generation. New
-- pin operations require schema 34, and all subsequent material writes above
-- advance the shared generation. Existing schema-33 operations retain their
-- catalog-state preconditions without a synthetic upgrade mutation.
