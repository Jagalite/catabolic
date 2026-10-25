-- Candidate41 only. Fixed reviewed native40 observation replacement.
-- Copy occurs without observation triggers: old table values, entity heads,
-- ctimes, publication generations and ledger must all remain unchanged.
-- No ALTER rename, writable_schema, PRAGMA or target-schema-derived SQL.
DROP TRIGGER publication_source_version;
DROP TRIGGER fallback_change_observations_insert;
DROP TRIGGER fallback_change_observations_update;
DROP TRIGGER fallback_change_observations_delete;
DROP TRIGGER catalog_change_observations_insert;
DROP TRIGGER catalog_change_observations_update;
DROP TRIGGER catalog_change_observations_delete;
DROP TRIGGER native_source_ctimes_observation_update;
DROP TRIGGER native_source_ctimes_observation_delete;
CREATE TABLE catalog_source_observation_migration_copy AS
 SELECT profile,file_id,size,mtime_ns,device,inode,status,scan_id FROM observations;
DROP TABLE observations;
CREATE TABLE observations (
 profile TEXT NOT NULL REFERENCES profiles(id),
 file_id TEXT NOT NULL REFERENCES files(id),
 size INTEGER NOT NULL, mtime_ns INTEGER NOT NULL,
 device INTEGER NOT NULL, inode INTEGER NOT NULL,
 status TEXT NOT NULL CHECK(status IN ('present','missing')),
 scan_id TEXT REFERENCES scans(id),
 observation_origin TEXT NOT NULL DEFAULT 'scan' CHECK(observation_origin IN ('scan','succession')),
 succession_source_id TEXT,
 succession_revision INTEGER,
 PRIMARY KEY(profile,file_id),
 CHECK((observation_origin='scan' AND scan_id IS NOT NULL AND succession_source_id IS NULL AND succession_revision IS NULL)
 OR (observation_origin='succession' AND scan_id IS NULL AND succession_source_id IS NOT NULL
 AND typeof(succession_revision)='integer' AND succession_revision>0 AND status='present')),
 FOREIGN KEY(profile,succession_source_id,succession_revision,file_id)
 REFERENCES catalog_source_succession_files(profile,source_id,succession_revision,file_id)
 DEFERRABLE INITIALLY DEFERRED
);
INSERT INTO observations(profile,file_id,size,mtime_ns,device,inode,status,scan_id)
 SELECT profile,file_id,size,mtime_ns,device,inode,status,scan_id FROM catalog_source_observation_migration_copy;
DROP TABLE catalog_source_observation_migration_copy;
CREATE TRIGGER publication_source_version AFTER UPDATE OF size,mtime_ns,device,inode ON observations
 WHEN NOT EXISTS(SELECT 1 FROM remount_guard WHERE profile=NEW.profile) AND NEW.status='present' AND (OLD.size!=NEW.size OR OLD.mtime_ns!=NEW.mtime_ns OR OLD.device!=NEW.device OR OLD.inode!=NEW.inode)
 BEGIN
 INSERT INTO publication_generations(profile,catalog,local_binding,generation)
 SELECT DISTINCT profile,catalog,local_binding,1 FROM publication_source_paths
 WHERE profile=NEW.profile AND file_id=NEW.file_id
 ON CONFLICT(profile,catalog) DO UPDATE SET generation=publication_generations.generation+1,local_binding=excluded.local_binding;
 UPDATE consumer_bindings SET generation=generation+1,due_at=CAST(strftime('%s','now') AS REAL)+debounce,indexing=NULL
 WHERE profile=NEW.profile AND enabled=1 AND EXISTS(
 SELECT 1 FROM publication_source_paths p WHERE p.profile=NEW.profile AND p.file_id=NEW.file_id AND p.catalog=consumer_bindings.catalog
 AND (consumer_bindings.subtree='' OR p.path=consumer_bindings.subtree OR substr(p.path,1,length(consumer_bindings.subtree)+1)=consumer_bindings.subtree||'/'));
 END;
CREATE TRIGGER fallback_change_observations_insert AFTER INSERT ON "observations" BEGIN UPDATE fallback_epoch SET generation=generation+1 WHERE id=1; END;
CREATE TRIGGER fallback_change_observations_update AFTER UPDATE ON "observations" WHEN NEW."profile" IS NOT OLD."profile" OR NEW."file_id" IS NOT OLD."file_id" OR NEW."size" IS NOT OLD."size" OR NEW."mtime_ns" IS NOT OLD."mtime_ns" OR NEW."device" IS NOT OLD."device" OR NEW."inode" IS NOT OLD."inode" OR NEW."status" IS NOT OLD."status" OR NEW."scan_id" IS NOT OLD."scan_id" OR NEW.observation_origin IS NOT OLD.observation_origin OR NEW.succession_source_id IS NOT OLD.succession_source_id OR NEW.succession_revision IS NOT OLD.succession_revision BEGIN UPDATE fallback_epoch SET generation=generation+1 WHERE id=1; END;
CREATE TRIGGER fallback_change_observations_delete AFTER DELETE ON "observations" BEGIN UPDATE fallback_epoch SET generation=generation+1 WHERE id=1; END;
CREATE TRIGGER catalog_change_observations_insert AFTER INSERT ON observations BEGIN SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END; INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES ('observations',json_array(NEW."profile",NEW."file_id"),1,0) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=excluded.deleted; UPDATE catalog_state SET generation=generation+1 WHERE id=1; END;
CREATE TRIGGER catalog_change_observations_update AFTER UPDATE ON observations WHEN NEW."profile" IS NOT OLD."profile" OR NEW."file_id" IS NOT OLD."file_id" OR NEW."size" IS NOT OLD."size" OR NEW."mtime_ns" IS NOT OLD."mtime_ns" OR NEW."device" IS NOT OLD."device" OR NEW."inode" IS NOT OLD."inode" OR NEW."status" IS NOT OLD."status" OR NEW."scan_id" IS NOT OLD."scan_id" OR NEW.observation_origin IS NOT OLD.observation_origin OR NEW.succession_source_id IS NOT OLD.succession_source_id OR NEW.succession_revision IS NOT OLD.succession_revision BEGIN SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END; INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) SELECT 'observations',json_array(OLD."profile",OLD."file_id"),1,1 WHERE json_array(NEW."profile",NEW."file_id") IS NOT json_array(OLD."profile",OLD."file_id") ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=1; INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES ('observations',json_array(NEW."profile",NEW."file_id"),1,0) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=excluded.deleted; UPDATE catalog_state SET generation=generation+1 WHERE id=1; END;
CREATE TRIGGER catalog_change_observations_delete AFTER DELETE ON observations BEGIN SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END; INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES ('observations',json_array(OLD."profile",OLD."file_id"),1,1) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=excluded.deleted; UPDATE catalog_state SET generation=generation+1 WHERE id=1; END;
CREATE TRIGGER native_source_ctimes_observation_update AFTER UPDATE ON observations
 WHEN NEW.profile IS NOT OLD.profile OR NEW.file_id IS NOT OLD.file_id
  OR NEW.size IS NOT OLD.size OR NEW.mtime_ns IS NOT OLD.mtime_ns
  OR NEW.device IS NOT OLD.device OR NEW.inode IS NOT OLD.inode
  OR NEW.status IS NOT OLD.status
 BEGIN DELETE FROM native_source_ctimes WHERE profile=OLD.profile AND file_id=OLD.file_id; END;
CREATE TRIGGER native_source_ctimes_observation_delete AFTER DELETE ON observations
 BEGIN DELETE FROM native_source_ctimes WHERE profile=OLD.profile AND file_id=OLD.file_id; END;

CREATE TRIGGER catalog_source_observation_insert BEFORE INSERT ON observations
 WHEN NEW.observation_origin='succession' AND (
 NOT EXISTS(SELECT 1 FROM catalog_source_succession_permits p
  WHERE p.profile=NEW.profile AND p.source_id=NEW.succession_source_id
  AND p.succession_revision=NEW.succession_revision
  AND p.incarnation=(SELECT incarnation FROM catalog_state WHERE id=1))
 OR NOT EXISTS(SELECT 1 FROM catalog_source_succession_files f
  WHERE f.profile=NEW.profile AND f.source_id=NEW.succession_source_id
  AND f.succession_revision=NEW.succession_revision AND f.file_id=NEW.file_id
  AND json_extract(f.new_occurrence_json,'$.size') IS NEW.size
  AND json_extract(f.new_occurrence_json,'$.mtime_ns') IS NEW.mtime_ns
  AND json_extract(f.new_occurrence_json,'$.device') IS NEW.device
  AND json_extract(f.new_occurrence_json,'$.inode') IS NEW.inode))
 BEGIN SELECT RAISE(ABORT,'exact retained source renewal required'); END;

CREATE TRIGGER catalog_source_observation_update BEFORE UPDATE ON observations
 WHEN NEW.observation_origin='succession' AND (
 NOT EXISTS(SELECT 1 FROM catalog_source_succession_permits p
  WHERE p.profile=NEW.profile AND p.source_id=NEW.succession_source_id
  AND p.succession_revision=NEW.succession_revision
  AND p.incarnation=(SELECT incarnation FROM catalog_state WHERE id=1))
 OR NOT EXISTS(SELECT 1 FROM catalog_source_succession_files f
  WHERE f.profile=NEW.profile AND f.source_id=NEW.succession_source_id
  AND f.succession_revision=NEW.succession_revision AND f.file_id=NEW.file_id
  AND json_extract(f.new_occurrence_json,'$.size') IS NEW.size
  AND json_extract(f.new_occurrence_json,'$.mtime_ns') IS NEW.mtime_ns
  AND json_extract(f.new_occurrence_json,'$.device') IS NEW.device
  AND json_extract(f.new_occurrence_json,'$.inode') IS NEW.inode))
 BEGIN SELECT RAISE(ABORT,'exact retained source renewal required'); END;
