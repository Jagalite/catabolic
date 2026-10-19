-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors
-- SPDX-License-Identifier: MIT
-- Additive schema34 fragment. No schema33 history/check changes. Execute in the
-- migration owner's transaction. This is not clearance of any other review.
CREATE TABLE catalog_logical_edit_permits (
 operation_id TEXT PRIMARY KEY,
 incarnation TEXT NOT NULL,
 barrier_id INTEGER NOT NULL DEFAULT 1 CHECK(barrier_id=1),
 FOREIGN KEY(barrier_id) REFERENCES catalog_commit_barrier(id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER catalog_logical_permit_insert BEFORE INSERT ON catalog_logical_edit_permits
 WHEN NEW.incarnation IS NOT (SELECT incarnation FROM catalog_state WHERE id=1)
 BEGIN SELECT RAISE(ABORT,'catalog_logical_stale_incarnation'); END;
CREATE TRIGGER catalog_logical_permit_no_update BEFORE UPDATE ON catalog_logical_edit_permits
 BEGIN SELECT RAISE(ABORT,'catalog_logical_permit_immutable'); END;

CREATE TABLE catalog_version_retirements (
 version_id TEXT PRIMARY KEY REFERENCES catalog_versions(id) ON DELETE RESTRICT,
 version_revision INTEGER NOT NULL CHECK(typeof(version_revision)='integer' AND version_revision>=1),
 command_catalog_id TEXT NOT NULL,
 command_host_namespace TEXT NOT NULL,
 command_operation_id TEXT NOT NULL,
 recorded_at_us INTEGER NOT NULL CHECK(typeof(recorded_at_us)='integer' AND recorded_at_us>=0),
 FOREIGN KEY(version_id,version_revision) REFERENCES catalog_version_revisions(version_id,revision),
 FOREIGN KEY(command_catalog_id,command_host_namespace,command_operation_id)
 REFERENCES catalog_command_receipts(catalog_id,host_namespace,operation_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER catalog_version_retirement_guard BEFORE INSERT ON catalog_version_retirements
 WHEN NEW.version_revision IS NOT (SELECT current_revision FROM catalog_versions WHERE id=NEW.version_id)
 OR NEW.command_catalog_id IS NOT (SELECT catalog_id FROM catalog_state WHERE id=1)
 OR NOT EXISTS(SELECT 1 FROM catalog_logical_edit_permits p JOIN catalog_state s ON s.id=1
   AND p.incarnation=s.incarnation WHERE p.operation_id=NEW.command_operation_id)
 BEGIN SELECT RAISE(ABORT,'catalog_logical_invalid_retirement'); END;
CREATE TRIGGER catalog_version_retirement_no_update BEFORE UPDATE ON catalog_version_retirements
 BEGIN SELECT RAISE(ABORT,'catalog_logical_retirement_immutable'); END;
CREATE TRIGGER catalog_version_retirement_no_delete BEFORE DELETE ON catalog_version_retirements
 BEGIN SELECT RAISE(ABORT,'catalog_logical_retirement_retained'); END;
CREATE TRIGGER catalog_retired_version_no_update BEFORE UPDATE ON catalog_versions
 WHEN EXISTS(SELECT 1 FROM catalog_version_retirements WHERE version_id=OLD.id)
 BEGIN SELECT RAISE(ABORT,'catalog_logical_version_retired'); END;
CREATE TRIGGER catalog_retired_version_no_insert BEFORE INSERT ON catalog_version_revisions
 WHEN EXISTS(SELECT 1 FROM catalog_version_retirements WHERE version_id=NEW.version_id)
 BEGIN SELECT RAISE(ABORT,'catalog_logical_version_retired'); END;

-- Original view remains a historical-head view. Every active owner/admission
-- path in the staged identity patch selects these views on schema34.
CREATE VIEW catalog_live_versions AS SELECT v.* FROM catalog_versions v
 WHERE NOT EXISTS(SELECT 1 FROM catalog_version_retirements x WHERE x.version_id=v.id);
CREATE VIEW catalog_live_binding_revisions AS SELECT b.* FROM catalog_active_binding_revisions b
 WHERE NOT EXISTS(SELECT 1 FROM catalog_version_retirements x WHERE x.version_id=b.version_id);

CREATE TABLE catalog_logical_retirements (
 item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE RESTRICT,
 kind TEXT NOT NULL CHECK(kind IN ('merge','split')),
 survivor_item_id TEXT REFERENCES items(id) ON DELETE RESTRICT,
 children_json TEXT NOT NULL CHECK(json_valid(children_json) AND json_type(children_json)='array'),
 reference_map_json TEXT NOT NULL CHECK(json_valid(reference_map_json) AND json_type(reference_map_json)='object'),
 command_catalog_id TEXT NOT NULL,
 command_host_namespace TEXT NOT NULL,
 command_operation_id TEXT NOT NULL,
 recorded_at_us INTEGER NOT NULL CHECK(typeof(recorded_at_us)='integer' AND recorded_at_us>=0),
 CHECK((kind='merge' AND survivor_item_id IS NOT NULL AND survivor_item_id!=item_id AND json_array_length(children_json)=0)
    OR (kind='split' AND survivor_item_id IS NULL AND json_array_length(children_json) BETWEEN 2 AND 16)),
 FOREIGN KEY(command_catalog_id,command_host_namespace,command_operation_id)
 REFERENCES catalog_command_receipts(catalog_id,host_namespace,operation_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER catalog_logical_retirement_guard BEFORE INSERT ON catalog_logical_retirements
 WHEN NEW.command_catalog_id IS NOT (SELECT catalog_id FROM catalog_state WHERE id=1)
 OR NOT EXISTS(SELECT 1 FROM catalog_logical_edit_permits p JOIN catalog_state s ON s.id=1
   AND p.incarnation=s.incarnation WHERE p.operation_id=NEW.command_operation_id)
 OR (NEW.kind='merge' AND EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=NEW.survivor_item_id))
 OR (NEW.kind='split' AND (EXISTS(SELECT 1 FROM json_each(NEW.children_json) j
   WHERE j.type!='text' OR j.value=NEW.item_id OR NOT EXISTS(SELECT 1 FROM items WHERE id=j.value)
   OR EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=j.value))
   OR (SELECT count(DISTINCT value) FROM json_each(NEW.children_json))!=json_array_length(NEW.children_json)))
 OR EXISTS(SELECT 1 FROM catalog_live_versions v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision
   JOIN catalog_timelines t ON t.id=r.timeline_id WHERE t.edition_item_id=NEW.item_id OR v.association_item_id=NEW.item_id)
 BEGIN SELECT RAISE(ABORT,'catalog_logical_invalid_retirement'); END;
CREATE TRIGGER catalog_logical_retirement_no_update BEFORE UPDATE ON catalog_logical_retirements
 BEGIN SELECT RAISE(ABORT,'catalog_logical_retirement_immutable'); END;
CREATE TRIGGER catalog_logical_retirement_no_delete BEFORE DELETE ON catalog_logical_retirements
 BEGIN SELECT RAISE(ABORT,'catalog_logical_retirement_retained'); END;
CREATE VIEW catalog_logical_aliases AS SELECT item_id AS source_item_id,survivor_item_id
 FROM catalog_logical_retirements WHERE kind='merge';
CREATE VIEW catalog_live_items AS SELECT i.* FROM items i
 WHERE NOT EXISTS(SELECT 1 FROM catalog_logical_retirements r WHERE r.item_id=i.id)
 AND NOT EXISTS(SELECT 1 FROM catalog_versions v JOIN catalog_version_retirements r ON r.version_id=v.id WHERE v.association_item_id=i.id);

CREATE TRIGGER catalog_retired_item_insert BEFORE INSERT ON items
 WHEN EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=NEW.id)
 OR EXISTS(SELECT 1 FROM catalog_versions v JOIN catalog_version_retirements r ON r.version_id=v.id WHERE v.association_item_id=NEW.id)
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;
CREATE TRIGGER catalog_retired_item_update BEFORE UPDATE ON items
 WHEN EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=OLD.id OR item_id=NEW.id)
 OR EXISTS(SELECT 1 FROM catalog_versions v JOIN catalog_version_retirements r ON r.version_id=v.id WHERE v.association_item_id IN (OLD.id,NEW.id))
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;
CREATE TRIGGER catalog_retired_item_delete BEFORE DELETE ON items
 WHEN EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=OLD.id)
 OR EXISTS(SELECT 1 FROM catalog_versions v JOIN catalog_version_retirements r ON r.version_id=v.id WHERE v.association_item_id=OLD.id)
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;
CREATE TRIGGER catalog_retired_identity_insert BEFORE INSERT ON identities
 WHEN EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=NEW.item_id)
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;
CREATE TRIGGER catalog_retired_identity_update BEFORE UPDATE ON identities
 WHEN EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id IN (OLD.item_id,NEW.item_id))
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;
CREATE TRIGGER catalog_retired_identity_delete BEFORE DELETE ON identities
 WHEN EXISTS(SELECT 1 FROM catalog_logical_retirements WHERE item_id=OLD.item_id)
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;
CREATE TRIGGER catalog_retired_association_insert BEFORE INSERT ON item_files
 WHEN NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=NEW.item_id)
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;
CREATE TRIGGER catalog_retired_association_update BEFORE UPDATE ON item_files
 WHEN NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=OLD.item_id)
 OR NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=NEW.item_id)
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;
CREATE TRIGGER catalog_retired_association_delete BEFORE DELETE ON item_files
 WHEN NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=OLD.item_id)
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;
CREATE TRIGGER catalog_retired_relationship_insert BEFORE INSERT ON item_relationships
 WHEN NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=NEW.source_id)
 OR NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=NEW.target_id)
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;
CREATE TRIGGER catalog_retired_relationship_update BEFORE UPDATE ON item_relationships
 WHEN NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=OLD.source_id)
 OR NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=OLD.target_id)
 OR NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=NEW.source_id)
 OR NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=NEW.target_id)
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;
CREATE TRIGGER catalog_retired_relationship_delete BEFORE DELETE ON item_relationships
 WHEN NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=OLD.source_id)
 OR NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=OLD.target_id)
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;

CREATE TRIGGER catalog_logical_retirements_revision_insert AFTER INSERT ON catalog_logical_retirements BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807
  THEN RAISE(ABORT,'catalog_logical_generation_exhausted') END;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
 VALUES('catalog_logical_retirements',json_array(NEW.item_id),1,0);
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
END;
CREATE TRIGGER catalog_version_retirements_revision_insert AFTER INSERT ON catalog_version_retirements BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807
  THEN RAISE(ABORT,'catalog_logical_generation_exhausted') END;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
 VALUES('catalog_version_retirements',json_array(NEW.version_id),1,0);
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
END;

-- A retired logical ID cannot acquire new legacy publication/expected-set
-- memberships. Existing inactive historical rows stay pinned and readable.
CREATE TRIGGER catalog_retired_mappings_insert BEFORE INSERT ON mappings
 WHEN (NEW.item_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=NEW.item_id))
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;
CREATE TRIGGER catalog_retired_mappings_update BEFORE UPDATE ON mappings
 WHEN (OLD.item_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=OLD.item_id)) OR (NEW.item_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=NEW.item_id))
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;
CREATE TRIGGER catalog_retired_mappings_delete BEFORE DELETE ON mappings
 WHEN (OLD.item_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=OLD.item_id))
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;
CREATE TRIGGER catalog_retired_expected_sets_insert BEFORE INSERT ON expected_sets
 WHEN (NEW.collection_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=NEW.collection_id))
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;
CREATE TRIGGER catalog_retired_expected_sets_update BEFORE UPDATE ON expected_sets
 WHEN (OLD.collection_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=OLD.collection_id)) OR (NEW.collection_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=NEW.collection_id))
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;
CREATE TRIGGER catalog_retired_expected_sets_delete BEFORE DELETE ON expected_sets
 WHEN (OLD.collection_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM catalog_live_items WHERE id=OLD.collection_id))
 BEGIN SELECT RAISE(ABORT,'catalog_logical_item_retired'); END;
