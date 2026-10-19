-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors
-- SPDX-License-Identifier: MIT
-- Additive schema-39 source ctimes (work plan A5, filesystem review FS3).
-- Migrations 033-038 are released, checksummed and signature-pinned, and the
-- legacy observations table is shared with frozen Python's schema 32, so ctime
-- is not added as a column. A companion row records the ctime a native scan saw
-- together with the exact (size, mtime_ns, device, inode) observation it was
-- captured with. No PRAGMA, transaction boundary, backfill, existing-row
-- mutation or generation change: no historical ctime is known, so the first
-- native scan after the upgrade records it.

CREATE TABLE native_source_ctimes (
 profile TEXT NOT NULL,
 file_id TEXT NOT NULL,
 size INTEGER NOT NULL CHECK(typeof(size)='integer'),
 mtime_ns INTEGER NOT NULL CHECK(typeof(mtime_ns)='integer'),
 device INTEGER NOT NULL CHECK(typeof(device)='integer'),
 inode INTEGER NOT NULL CHECK(typeof(inode)='integer'),
 ctime_ns INTEGER NOT NULL CHECK(typeof(ctime_ns)='integer'),
 PRIMARY KEY(profile,file_id)
);

-- A companion is bound to the present observation it was captured with. It
-- cannot be written for another tuple, a missing file or an unknown key.
CREATE TRIGGER native_source_ctimes_bound_insert BEFORE INSERT ON native_source_ctimes
 WHEN NOT EXISTS(SELECT 1 FROM observations o WHERE o.profile=NEW.profile AND o.file_id=NEW.file_id
  AND o.status='present' AND o.size IS NEW.size AND o.mtime_ns IS NEW.mtime_ns
  AND o.device IS NEW.device AND o.inode IS NEW.inode)
 BEGIN SELECT RAISE(ABORT,'native_source_ctime_unbound'); END;
CREATE TRIGGER native_source_ctimes_bound_update BEFORE UPDATE ON native_source_ctimes
 WHEN NEW.profile IS NOT OLD.profile OR NEW.file_id IS NOT OLD.file_id
  OR NOT EXISTS(SELECT 1 FROM observations o WHERE o.profile=NEW.profile AND o.file_id=NEW.file_id
  AND o.status='present' AND o.size IS NEW.size AND o.mtime_ns IS NEW.mtime_ns
  AND o.device IS NEW.device AND o.inode IS NEW.inode)
 BEGIN SELECT RAISE(ABORT,'native_source_ctime_unbound'); END;

-- Any writer that changes the observed tuple or status, or removes the
-- observation, discards the companion: a stale ctime never survives, whichever
-- owner (scan, remount repair, artifact publication) wrote the observation.
CREATE TRIGGER native_source_ctimes_observation_update AFTER UPDATE ON observations
 WHEN NEW.profile IS NOT OLD.profile OR NEW.file_id IS NOT OLD.file_id
  OR NEW.size IS NOT OLD.size OR NEW.mtime_ns IS NOT OLD.mtime_ns
  OR NEW.device IS NOT OLD.device OR NEW.inode IS NOT OLD.inode
  OR NEW.status IS NOT OLD.status
 BEGIN DELETE FROM native_source_ctimes WHERE profile=OLD.profile AND file_id=OLD.file_id; END;
CREATE TRIGGER native_source_ctimes_observation_delete AFTER DELETE ON observations
 BEGIN DELETE FROM native_source_ctimes WHERE profile=OLD.profile AND file_id=OLD.file_id; END;

-- A ctime change rotates the content revision, so it advances generation like
-- every other source row.
CREATE TRIGGER catalog_change_native_source_ctimes_insert AFTER INSERT ON native_source_ctimes BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES('native_source_ctimes',json_array(NEW.profile,NEW.file_id),1,0) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
END;
CREATE TRIGGER catalog_change_native_source_ctimes_update AFTER UPDATE ON native_source_ctimes
 WHEN NEW.size IS NOT OLD.size OR NEW.mtime_ns IS NOT OLD.mtime_ns OR NEW.device IS NOT OLD.device
  OR NEW.inode IS NOT OLD.inode OR NEW.ctime_ns IS NOT OLD.ctime_ns BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES('native_source_ctimes',json_array(NEW.profile,NEW.file_id),1,0) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
END;
CREATE TRIGGER catalog_change_native_source_ctimes_delete AFTER DELETE ON native_source_ctimes BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES('native_source_ctimes',json_array(OLD.profile,OLD.file_id),1,1) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=1;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
END;
