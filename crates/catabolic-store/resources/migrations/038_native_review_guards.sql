-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors
-- SPDX-License-Identifier: MIT
-- Additive schema-38 review guards. Migrations 033-037 are released, checksummed
-- and signature-pinned, so their tables are not rebuilt and their DDL is not
-- rewritten. Every guard below is a new trigger, or a new table whose deferred
-- foreign key supplies commit-time semantics that SQLite cannot add to an
-- existing table without a rebuild. No PRAGMA, transaction boundary, receipt
-- creation, generation change or existing-row mutation.

-- SC1. Tag-suppression heads cascade with their item/file subject while the
-- retained history does not. Like identity pins, a subject with a suppression
-- head stays managed until an explicit retirement owner exists: deletion,
-- REPLACE (which deletes under the required recursive_triggers=ON) and rekey are
-- refused. A subject whose heads were already cascaded away before schema 38
-- cannot be recreated under the same ID, because its next decision would
-- restart at revision 1 and collide with the retained history.
CREATE TRIGGER catalog_tag_suppression_item_subject_delete_guard BEFORE DELETE ON items
 WHEN EXISTS(SELECT 1 FROM catalog_item_tag_suppressions WHERE item_id=OLD.id)
 BEGIN SELECT RAISE(ABORT,'catalog_tag_suppression_subject_retirement_required'); END;
CREATE TRIGGER catalog_tag_suppression_item_subject_rekey_guard BEFORE UPDATE OF id ON items
 WHEN NEW.id IS NOT OLD.id AND EXISTS(SELECT 1 FROM catalog_item_tag_suppressions WHERE item_id=OLD.id)
 BEGIN SELECT RAISE(ABORT,'catalog_tag_suppression_subject_retirement_required'); END;
CREATE TRIGGER catalog_tag_suppression_item_subject_recreate_guard BEFORE INSERT ON items
 WHEN NOT EXISTS(SELECT 1 FROM items WHERE id=NEW.id)
 AND EXISTS(SELECT 1 FROM catalog_tag_suppression_revisions WHERE subject_type='item' AND subject_id=NEW.id)
 BEGIN SELECT RAISE(ABORT,'catalog_tag_suppression_subject_history_retained'); END;
CREATE TRIGGER catalog_tag_suppression_item_subject_rekey_target_guard BEFORE UPDATE OF id ON items
 WHEN NEW.id IS NOT OLD.id
 AND EXISTS(SELECT 1 FROM catalog_tag_suppression_revisions WHERE subject_type='item' AND subject_id=NEW.id)
 BEGIN SELECT RAISE(ABORT,'catalog_tag_suppression_subject_history_retained'); END;
CREATE TRIGGER catalog_tag_suppression_file_subject_delete_guard BEFORE DELETE ON files
 WHEN EXISTS(SELECT 1 FROM catalog_file_tag_suppressions WHERE file_id=OLD.id)
 BEGIN SELECT RAISE(ABORT,'catalog_tag_suppression_subject_retirement_required'); END;
CREATE TRIGGER catalog_tag_suppression_file_subject_rekey_guard BEFORE UPDATE OF id ON files
 WHEN NEW.id IS NOT OLD.id AND EXISTS(SELECT 1 FROM catalog_file_tag_suppressions WHERE file_id=OLD.id)
 BEGIN SELECT RAISE(ABORT,'catalog_tag_suppression_subject_retirement_required'); END;
CREATE TRIGGER catalog_tag_suppression_file_subject_recreate_guard BEFORE INSERT ON files
 WHEN NOT EXISTS(SELECT 1 FROM files WHERE id=NEW.id)
 AND EXISTS(SELECT 1 FROM catalog_tag_suppression_revisions WHERE subject_type='file' AND subject_id=NEW.id)
 BEGIN SELECT RAISE(ABORT,'catalog_tag_suppression_subject_history_retained'); END;
CREATE TRIGGER catalog_tag_suppression_file_subject_rekey_target_guard BEFORE UPDATE OF id ON files
 WHEN NEW.id IS NOT OLD.id
 AND EXISTS(SELECT 1 FROM catalog_tag_suppression_revisions WHERE subject_type='file' AND subject_id=NEW.id)
 BEGIN SELECT RAISE(ABORT,'catalog_tag_suppression_subject_history_retained'); END;

-- SC2. Metadata subjects/history carry a command key without a receipt FK.
-- Each distinct key is recorded once in an immutable obligation row whose
-- deferred composite FK must resolve to a committed receipt at COMMIT. Receipt
-- keys are permanent, so a satisfied obligation stays satisfied. Existing
-- provenance is backfilled; an orphan makes the migration's foreign-key check
-- fail closed. Obligations are derived provenance, not catalog-visible state,
-- so they deliberately do not advance generation.
CREATE TABLE catalog_metadata_receipt_obligations (
 command_catalog_id TEXT NOT NULL,
 command_host_namespace TEXT NOT NULL,
 command_operation_id TEXT NOT NULL,
 PRIMARY KEY(command_catalog_id,command_host_namespace,command_operation_id),
 FOREIGN KEY(command_catalog_id,command_host_namespace,command_operation_id)
  REFERENCES catalog_command_receipts(catalog_id,host_namespace,operation_id) DEFERRABLE INITIALLY DEFERRED
) WITHOUT ROWID;
INSERT INTO catalog_metadata_receipt_obligations(command_catalog_id,command_host_namespace,command_operation_id)
 SELECT command_catalog_id,command_host_namespace,command_operation_id FROM catalog_metadata_subjects
 UNION SELECT command_catalog_id,command_host_namespace,command_operation_id FROM catalog_metadata_history;
CREATE TRIGGER catalog_metadata_receipt_obligation_no_update BEFORE UPDATE ON catalog_metadata_receipt_obligations
 BEGIN SELECT RAISE(ABORT,'catalog_metadata_receipt_obligation_immutable'); END;
CREATE TRIGGER catalog_metadata_receipt_obligation_no_delete BEFORE DELETE ON catalog_metadata_receipt_obligations
 BEGIN SELECT RAISE(ABORT,'catalog_metadata_receipt_obligation_retained'); END;
CREATE TRIGGER catalog_metadata_subject_receipt_obligation AFTER INSERT ON catalog_metadata_subjects
 BEGIN INSERT INTO catalog_metadata_receipt_obligations(command_catalog_id,command_host_namespace,command_operation_id)
  SELECT NEW.command_catalog_id,NEW.command_host_namespace,NEW.command_operation_id
  WHERE NOT EXISTS(SELECT 1 FROM catalog_metadata_receipt_obligations WHERE command_catalog_id=NEW.command_catalog_id
   AND command_host_namespace=NEW.command_host_namespace AND command_operation_id=NEW.command_operation_id); END;
CREATE TRIGGER catalog_metadata_history_receipt_obligation AFTER INSERT ON catalog_metadata_history
 BEGIN INSERT INTO catalog_metadata_receipt_obligations(command_catalog_id,command_host_namespace,command_operation_id)
  SELECT NEW.command_catalog_id,NEW.command_host_namespace,NEW.command_operation_id
  WHERE NOT EXISTS(SELECT 1 FROM catalog_metadata_receipt_obligations WHERE command_catalog_id=NEW.command_catalog_id
   AND command_host_namespace=NEW.command_host_namespace AND command_operation_id=NEW.command_operation_id); END;

-- SC3. Schema-33 receipt sealing (catalog_*_revisions_uncommitted) for every
-- schema-34 curation record keyed by a command: the receipt is written last, so
-- a record naming an already-committed receipt is forged or replayed provenance.
CREATE TRIGGER catalog_metadata_subjects_uncommitted BEFORE INSERT ON catalog_metadata_subjects WHEN EXISTS (SELECT 1 FROM catalog_command_receipts r WHERE r.catalog_id=NEW.command_catalog_id AND r.host_namespace=NEW.command_host_namespace AND r.operation_id=NEW.command_operation_id) BEGIN SELECT RAISE(ABORT,'catalog_committed_history_sealed'); END;
CREATE TRIGGER catalog_metadata_history_uncommitted BEFORE INSERT ON catalog_metadata_history WHEN EXISTS (SELECT 1 FROM catalog_command_receipts r WHERE r.catalog_id=NEW.command_catalog_id AND r.host_namespace=NEW.command_host_namespace AND r.operation_id=NEW.command_operation_id) BEGIN SELECT RAISE(ABORT,'catalog_committed_history_sealed'); END;
CREATE TRIGGER catalog_identity_pin_revisions_uncommitted BEFORE INSERT ON catalog_identity_pin_revisions WHEN EXISTS (SELECT 1 FROM catalog_command_receipts r WHERE r.catalog_id=NEW.command_catalog_id AND r.host_namespace=NEW.command_host_namespace AND r.operation_id=NEW.command_operation_id) BEGIN SELECT RAISE(ABORT,'catalog_committed_history_sealed'); END;
CREATE TRIGGER catalog_tag_suppression_revisions_uncommitted BEFORE INSERT ON catalog_tag_suppression_revisions WHEN EXISTS (SELECT 1 FROM catalog_command_receipts r WHERE r.catalog_id=NEW.command_catalog_id AND r.host_namespace=NEW.command_host_namespace AND r.operation_id=NEW.command_operation_id) BEGIN SELECT RAISE(ABORT,'catalog_committed_history_sealed'); END;
CREATE TRIGGER catalog_tag_merges_uncommitted BEFORE INSERT ON catalog_tag_merges WHEN EXISTS (SELECT 1 FROM catalog_command_receipts r WHERE r.catalog_id=NEW.command_catalog_id AND r.host_namespace=NEW.command_host_namespace AND r.operation_id=NEW.command_operation_id) BEGIN SELECT RAISE(ABORT,'catalog_committed_history_sealed'); END;
CREATE TRIGGER catalog_logical_retirements_uncommitted BEFORE INSERT ON catalog_logical_retirements WHEN EXISTS (SELECT 1 FROM catalog_command_receipts r WHERE r.catalog_id=NEW.command_catalog_id AND r.host_namespace=NEW.command_host_namespace AND r.operation_id=NEW.command_operation_id) BEGIN SELECT RAISE(ABORT,'catalog_committed_history_sealed'); END;
CREATE TRIGGER catalog_version_retirements_uncommitted BEFORE INSERT ON catalog_version_retirements WHEN EXISTS (SELECT 1 FROM catalog_command_receipts r WHERE r.catalog_id=NEW.command_catalog_id AND r.host_namespace=NEW.command_host_namespace AND r.operation_id=NEW.command_operation_id) BEGIN SELECT RAISE(ABORT,'catalog_committed_history_sealed'); END;

-- SC4. Schema-36's table CHECK evaluates NULL, and therefore passes, for a
-- quiescent row whose cleanup_kind is NULL. Require the complete NULL-safe
-- terminal witness for every non-owned row. Persisted rows are validated by the
-- migration owner (full validation) before this schema can commit or open.
CREATE TRIGGER native_execution_lifetimes_terminal_witness_insert BEFORE INSERT ON native_execution_lifetimes
 WHEN NEW.state IS NOT 'owned' AND NOT (NEW.state IS 'quiescent' AND NEW.cleanup_kind IS 'builtin_no_child_v1'
  AND NEW.child_obligations IS 0 AND NEW.verified_retirements IS 0 AND NEW.capacity_released IS 1 AND NEW.sealed_at IS NOT NULL)
 BEGIN SELECT RAISE(ABORT,'invalid_native_execution_terminal_witness'); END;
CREATE TRIGGER native_execution_lifetimes_terminal_witness_update BEFORE UPDATE ON native_execution_lifetimes
 WHEN NEW.state IS NOT 'owned' AND NOT (NEW.state IS 'quiescent' AND NEW.cleanup_kind IS 'builtin_no_child_v1'
  AND NEW.child_obligations IS 0 AND NEW.verified_retirements IS 0 AND NEW.capacity_released IS 1 AND NEW.sealed_at IS NOT NULL)
 BEGIN SELECT RAISE(ABORT,'invalid_native_execution_terminal_witness'); END;
