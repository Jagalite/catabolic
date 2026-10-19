-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors
-- SPDX-License-Identifier: MIT
-- PREPARATION ONLY: additive tag fragment for a later migration 034.
-- Requires native schema 33; no migration registration or transaction boundary.
-- Runtime item-decision checks also require catalog_logical_retirements(item_id)
-- from the logical owner's same R3 migration; that table is not created here.
-- Existing tag assertions/normalization and their schema-33 generation triggers
-- remain unchanged. Receipt ownership stays with catalog_commands.

-- Retained decisions intentionally have no subject/tag FK: their exact prior
-- identity must remain evidence after subject deletion or tag-vocabulary merge.
CREATE TABLE catalog_tag_suppression_revisions (
 subject_type TEXT NOT NULL CHECK(subject_type IN ('item','file')),
 subject_id TEXT NOT NULL CHECK(length(CAST(subject_id AS BLOB)) BETWEEN 1 AND 256),
 tag_id TEXT NOT NULL CHECK(length(CAST(tag_id AS BLOB)) BETWEEN 1 AND 256),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision >= 1),
 suppressed INTEGER NOT NULL CHECK(suppressed IN (0,1)),
 reason TEXT NOT NULL CHECK(length(CAST(reason AS BLOB)) <= 4000),
 recorded_at_us INTEGER NOT NULL CHECK(typeof(recorded_at_us)='integer' AND recorded_at_us >= 0),
 command_catalog_id TEXT NOT NULL,
 command_host_namespace TEXT NOT NULL,
 command_operation_id TEXT NOT NULL,
 PRIMARY KEY(subject_type,subject_id,tag_id,revision),
 FOREIGN KEY(command_catalog_id,command_host_namespace,command_operation_id)
  REFERENCES catalog_command_receipts(catalog_id,host_namespace,operation_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE catalog_item_tag_suppressions (
 subject_type TEXT NOT NULL DEFAULT 'item' CHECK(subject_type='item'),
 item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
 tag_id TEXT NOT NULL REFERENCES tags(id),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision >= 1),
 PRIMARY KEY(item_id,tag_id),
 FOREIGN KEY(subject_type,item_id,tag_id,revision)
  REFERENCES catalog_tag_suppression_revisions(subject_type,subject_id,tag_id,revision) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE catalog_file_tag_suppressions (
 subject_type TEXT NOT NULL DEFAULT 'file' CHECK(subject_type='file'),
 file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
 tag_id TEXT NOT NULL REFERENCES tags(id),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision >= 1),
 PRIMARY KEY(file_id,tag_id),
 FOREIGN KEY(subject_type,file_id,tag_id,revision)
  REFERENCES catalog_tag_suppression_revisions(subject_type,subject_id,tag_id,revision) DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX catalog_item_tag_suppressions_by_tag ON catalog_item_tag_suppressions(tag_id,item_id);
CREATE INDEX catalog_file_tag_suppressions_by_tag ON catalog_file_tag_suppressions(tag_id,file_id);

-- A merge only retires a vocabulary ID, never an item/file/media identity.
-- original_target_tag_id and the full snapshot are historical, not live FKs.
CREATE TABLE catalog_tag_merges (
 source_tag_id TEXT PRIMARY KEY NOT NULL CHECK(length(CAST(source_tag_id AS BLOB)) BETWEEN 1 AND 256),
 original_target_tag_id TEXT NOT NULL CHECK(length(CAST(original_target_tag_id AS BLOB)) BETWEEN 1 AND 256),
 snapshot_json TEXT NOT NULL CHECK(json_valid(snapshot_json) AND json_type(snapshot_json)='object'
    AND length(CAST(snapshot_json AS BLOB)) <= 2097152),
 recorded_at_us INTEGER NOT NULL CHECK(typeof(recorded_at_us)='integer' AND recorded_at_us >= 0),
 command_catalog_id TEXT NOT NULL,
 command_host_namespace TEXT NOT NULL,
 command_operation_id TEXT NOT NULL,
 CHECK(source_tag_id != original_target_tag_id),
 FOREIGN KEY(command_catalog_id,command_host_namespace,command_operation_id)
  REFERENCES catalog_command_receipts(catalog_id,host_namespace,operation_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE catalog_tag_redirects (
 source_tag_id TEXT PRIMARY KEY NOT NULL REFERENCES catalog_tag_merges(source_tag_id),
 target_tag_id TEXT NOT NULL REFERENCES tags(id),
 CHECK(source_tag_id != target_tag_id)
);
CREATE INDEX catalog_tag_redirects_by_target ON catalog_tag_redirects(target_tag_id,source_tag_id);
CREATE TRIGGER catalog_retired_tag_no_reuse BEFORE INSERT ON tags
 WHEN EXISTS(SELECT 1 FROM catalog_tag_merges WHERE source_tag_id=NEW.id)
 BEGIN SELECT RAISE(ABORT,'catalog_tag_id_retired'); END;
CREATE TRIGGER catalog_retired_tag_no_rekey BEFORE UPDATE OF id ON tags
 WHEN EXISTS(SELECT 1 FROM catalog_tag_merges WHERE source_tag_id=NEW.id)
 BEGIN SELECT RAISE(ABORT,'catalog_tag_id_retired'); END;
CREATE TRIGGER catalog_tag_suppression_history_no_update BEFORE UPDATE ON catalog_tag_suppression_revisions
 BEGIN SELECT RAISE(ABORT,'catalog_tag_history_immutable'); END;
CREATE TRIGGER catalog_tag_suppression_history_no_delete BEFORE DELETE ON catalog_tag_suppression_revisions
 BEGIN SELECT RAISE(ABORT,'catalog_tag_history_immutable'); END;
CREATE TRIGGER catalog_tag_merge_no_update BEFORE UPDATE ON catalog_tag_merges
 BEGIN SELECT RAISE(ABORT,'catalog_tag_history_immutable'); END;
CREATE TRIGGER catalog_tag_merge_no_delete BEFORE DELETE ON catalog_tag_merges
 BEGIN SELECT RAISE(ABORT,'catalog_tag_history_immutable'); END;
CREATE TRIGGER catalog_tag_redirect_no_rekey BEFORE UPDATE OF source_tag_id ON catalog_tag_redirects
 WHEN NEW.source_tag_id IS NOT OLD.source_tag_id
 BEGIN SELECT RAISE(ABORT,'catalog_tag_redirect_permanent'); END;
CREATE TRIGGER catalog_tag_redirect_no_delete BEFORE DELETE ON catalog_tag_redirects
 BEGIN SELECT RAISE(ABORT,'catalog_tag_redirect_permanent'); END;
CREATE TRIGGER catalog_item_suppression_revision_guard BEFORE UPDATE ON catalog_item_tag_suppressions
 WHEN NEW.subject_type IS NOT OLD.subject_type OR NEW.item_id IS NOT OLD.item_id OR NEW.tag_id IS NOT OLD.tag_id
  OR typeof(NEW.revision) != 'integer' OR NEW.revision != OLD.revision+1
 BEGIN SELECT RAISE(ABORT,'catalog_tag_suppression_revision'); END;
CREATE TRIGGER catalog_file_suppression_revision_guard BEFORE UPDATE ON catalog_file_tag_suppressions
 WHEN NEW.subject_type IS NOT OLD.subject_type OR NEW.file_id IS NOT OLD.file_id OR NEW.tag_id IS NOT OLD.tag_id
  OR typeof(NEW.revision) != 'integer' OR NEW.revision != OLD.revision+1
 BEGIN SELECT RAISE(ABORT,'catalog_tag_suppression_revision'); END;

-- Native effective projections: raw legacy assertion queries retain provenance.
CREATE VIEW catalog_effective_item_tags AS
 SELECT a.* FROM item_tags a WHERE a.active=1 AND NOT EXISTS (
  SELECT 1 FROM catalog_item_tag_suppressions s
  JOIN catalog_tag_suppression_revisions h ON h.subject_type=s.subject_type
   AND h.subject_id=s.item_id AND h.tag_id=s.tag_id AND h.revision=s.revision
  WHERE s.item_id=a.item_id AND s.tag_id=a.tag_id AND h.suppressed=1);
CREATE VIEW catalog_effective_file_tags AS
 SELECT a.* FROM file_tags a WHERE a.active=1 AND NOT EXISTS (
  SELECT 1 FROM catalog_file_tag_suppressions s
  JOIN catalog_tag_suppression_revisions h ON h.subject_type=s.subject_type
   AND h.subject_id=s.file_id AND h.tag_id=s.tag_id AND h.revision=s.revision
  WHERE s.file_id=a.file_id AND s.tag_id=a.tag_id AND h.suppressed=1);

-- Explicit mutation-generation/entity-revision coverage for EVERY new table.
-- History tables reject UPDATE/DELETE above; redirects reject DELETE/rekey.
-- Old tag vocabulary/assertions continue using existing schema-33 triggers.
CREATE TRIGGER catalog_change_catalog_tag_suppression_revisions_insert AFTER INSERT ON catalog_tag_suppression_revisions
 BEGIN
  SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
  INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES ('catalog_tag_suppression_revisions',json_array(NEW.subject_type,NEW.subject_id,NEW.tag_id,NEW.revision),1,0)
   ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=excluded.deleted;
  UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 END;
CREATE TRIGGER catalog_change_catalog_item_tag_suppressions_insert AFTER INSERT ON catalog_item_tag_suppressions
 BEGIN
  SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
  INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES ('catalog_item_tag_suppressions',json_array(NEW.item_id,NEW.tag_id),1,0)
   ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=excluded.deleted;
  UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 END;
CREATE TRIGGER catalog_change_catalog_item_tag_suppressions_update AFTER UPDATE ON catalog_item_tag_suppressions WHEN NEW.subject_type IS NOT OLD.subject_type OR NEW.item_id IS NOT OLD.item_id OR NEW.tag_id IS NOT OLD.tag_id OR NEW.revision IS NOT OLD.revision
 BEGIN
  SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
  INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES ('catalog_item_tag_suppressions',json_array(NEW.item_id,NEW.tag_id),1,0)
   ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=excluded.deleted;
  UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 END;
CREATE TRIGGER catalog_change_catalog_item_tag_suppressions_delete AFTER DELETE ON catalog_item_tag_suppressions
 BEGIN
  SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
  INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES ('catalog_item_tag_suppressions',json_array(OLD.item_id,OLD.tag_id),1,1)
   ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=excluded.deleted;
  UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 END;
CREATE TRIGGER catalog_change_catalog_file_tag_suppressions_insert AFTER INSERT ON catalog_file_tag_suppressions
 BEGIN
  SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
  INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES ('catalog_file_tag_suppressions',json_array(NEW.file_id,NEW.tag_id),1,0)
   ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=excluded.deleted;
  UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 END;
CREATE TRIGGER catalog_change_catalog_file_tag_suppressions_update AFTER UPDATE ON catalog_file_tag_suppressions WHEN NEW.subject_type IS NOT OLD.subject_type OR NEW.file_id IS NOT OLD.file_id OR NEW.tag_id IS NOT OLD.tag_id OR NEW.revision IS NOT OLD.revision
 BEGIN
  SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
  INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES ('catalog_file_tag_suppressions',json_array(NEW.file_id,NEW.tag_id),1,0)
   ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=excluded.deleted;
  UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 END;
CREATE TRIGGER catalog_change_catalog_file_tag_suppressions_delete AFTER DELETE ON catalog_file_tag_suppressions
 BEGIN
  SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
  INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES ('catalog_file_tag_suppressions',json_array(OLD.file_id,OLD.tag_id),1,1)
   ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=excluded.deleted;
  UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 END;
CREATE TRIGGER catalog_change_catalog_tag_merges_insert AFTER INSERT ON catalog_tag_merges
 BEGIN
  SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
  INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES ('catalog_tag_merges',json_array(NEW.source_tag_id),1,0)
   ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=excluded.deleted;
  UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 END;
CREATE TRIGGER catalog_change_catalog_tag_redirects_insert AFTER INSERT ON catalog_tag_redirects
 BEGIN
  SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
  INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES ('catalog_tag_redirects',json_array(NEW.source_tag_id),1,0)
   ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=excluded.deleted;
  UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 END;
CREATE TRIGGER catalog_change_catalog_tag_redirects_update AFTER UPDATE ON catalog_tag_redirects WHEN NEW.source_tag_id IS NOT OLD.source_tag_id OR NEW.target_tag_id IS NOT OLD.target_tag_id
 BEGIN
  SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
  INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES ('catalog_tag_redirects',json_array(NEW.source_tag_id),1,0)
   ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=excluded.deleted;
  UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 END;
