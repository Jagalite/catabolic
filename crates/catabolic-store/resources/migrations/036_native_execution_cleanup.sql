-- Additive native cleanup evidence. Version assignment/combined signature belongs
-- to the central migration owner. No historical claims are backfilled.
CREATE TABLE native_execution_lifetimes (
 catalog_id TEXT NOT NULL,
 incarnation TEXT NOT NULL CHECK(length(CAST(incarnation AS BLOB))=32 AND incarnation NOT GLOB '*[^0-9a-f]*'),
 job_id TEXT NOT NULL,
 generation INTEGER NOT NULL CHECK(typeof(generation)='integer' AND generation>0),
 attempt INTEGER NOT NULL CHECK(typeof(attempt)='integer' AND attempt>0),
 claim_token_digest TEXT NOT NULL CHECK(length(claim_token_digest)=64 AND claim_token_digest NOT GLOB '*[^0-9a-f]*'),
 snapshot_digest TEXT NOT NULL CHECK(length(snapshot_digest)=64 AND snapshot_digest NOT GLOB '*[^0-9a-f]*'),
 options_digest TEXT NOT NULL CHECK(length(options_digest)=64 AND options_digest NOT GLOB '*[^0-9a-f]*'),
 runtime_owner_nonce TEXT NOT NULL UNIQUE CHECK(length(runtime_owner_nonce)=32 AND runtime_owner_nonce NOT GLOB '*[^0-9a-f]*'),
 protocol_version INTEGER NOT NULL CHECK(protocol_version=1),
 state TEXT NOT NULL DEFAULT 'owned' CHECK(state IN ('owned','quiescent')),
 cleanup_kind TEXT,
 child_obligations INTEGER NOT NULL DEFAULT 0 CHECK(typeof(child_obligations)='integer' AND child_obligations>=0),
 verified_retirements INTEGER NOT NULL DEFAULT 0 CHECK(typeof(verified_retirements)='integer' AND verified_retirements>=0),
 capacity_released INTEGER NOT NULL DEFAULT 0 CHECK(capacity_released IN (0,1)),
 sealed_at TEXT,
 PRIMARY KEY(incarnation,job_id,generation,attempt,claim_token_digest),
 CHECK((state='owned' AND cleanup_kind IS NULL AND capacity_released=0 AND sealed_at IS NULL)
    OR (state='quiescent' AND cleanup_kind='builtin_no_child_v1' AND child_obligations=0 AND verified_retirements=0 AND capacity_released=1 AND sealed_at IS NOT NULL))
);
CREATE TRIGGER native_execution_lifetimes_insert_guard BEFORE INSERT ON native_execution_lifetimes BEGIN
 SELECT CASE WHEN NEW.state!='owned' OR NOT EXISTS(SELECT 1 FROM catalog_state WHERE id=1 AND catalog_id=NEW.catalog_id AND incarnation=NEW.incarnation) OR NOT EXISTS(SELECT 1 FROM execution_claims WHERE job_id=NEW.job_id AND generation=NEW.generation AND attempt=NEW.attempt) THEN RAISE(ABORT,'invalid_native_execution_owner') END;
END;
CREATE TRIGGER native_execution_lifetimes_update_guard BEFORE UPDATE ON native_execution_lifetimes BEGIN
 SELECT CASE WHEN OLD.state!='owned' OR NEW.state!='quiescent' OR NEW.catalog_id IS NOT OLD.catalog_id OR NEW.incarnation IS NOT OLD.incarnation OR NEW.job_id IS NOT OLD.job_id OR NEW.generation IS NOT OLD.generation OR NEW.attempt IS NOT OLD.attempt OR NEW.claim_token_digest IS NOT OLD.claim_token_digest OR NEW.snapshot_digest IS NOT OLD.snapshot_digest OR NEW.options_digest IS NOT OLD.options_digest OR NEW.runtime_owner_nonce IS NOT OLD.runtime_owner_nonce OR NEW.protocol_version IS NOT OLD.protocol_version OR NOT EXISTS(SELECT 1 FROM catalog_state WHERE id=1 AND catalog_id=NEW.catalog_id AND incarnation=NEW.incarnation) OR NOT EXISTS(SELECT 1 FROM execution_claims WHERE job_id=NEW.job_id AND generation=NEW.generation AND attempt=NEW.attempt) THEN RAISE(ABORT,'invalid_native_execution_cleanup') END;
END;
CREATE TRIGGER native_execution_lifetimes_delete_guard BEFORE DELETE ON native_execution_lifetimes BEGIN SELECT RAISE(ABORT,'native_execution_lifetimes_are_immutable'); END;
CREATE TRIGGER catalog_change_native_execution_lifetimes_insert AFTER INSERT ON native_execution_lifetimes BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES('native_execution_lifetimes',json_array(NEW.incarnation,NEW.job_id,NEW.generation,NEW.attempt,NEW.claim_token_digest),1,0) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
END;
CREATE TRIGGER catalog_change_native_execution_lifetimes_update AFTER UPDATE ON native_execution_lifetimes BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES('native_execution_lifetimes',json_array(NEW.incarnation,NEW.job_id,NEW.generation,NEW.attempt,NEW.claim_token_digest),1,0) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
END;
