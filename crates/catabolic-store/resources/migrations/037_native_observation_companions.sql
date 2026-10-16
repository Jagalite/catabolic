-- Candidate additive closed observation companion protocol. No historical backfill.
CREATE TABLE native_observation_companions (
 reservation_nonce TEXT PRIMARY KEY NOT NULL CHECK(length(reservation_nonce)=32 AND reservation_nonce NOT GLOB '*[^0-9a-f]*'),
 catalog_id TEXT NOT NULL,
 incarnation TEXT NOT NULL CHECK(length(incarnation)=32 AND incarnation NOT GLOB '*[^0-9a-f]*'),
 protocol_kind TEXT NOT NULL CHECK(protocol_kind='native_observation_no_child'),
 protocol_version INTEGER NOT NULL CHECK(typeof(protocol_version)='integer' AND protocol_version=1),
 helper_digest TEXT NOT NULL CHECK(length(helper_digest)=64 AND helper_digest NOT GLOB '*[^0-9a-f]*'),
 profile TEXT NOT NULL CHECK(length(profile)>0),
 source TEXT NOT NULL CHECK(length(source)>0),
 requester TEXT NOT NULL CHECK(length(requester)>0),
 args_digest TEXT NOT NULL CHECK(length(args_digest)=64 AND args_digest NOT GLOB '*[^0-9a-f]*'),
 state TEXT NOT NULL DEFAULT 'reserved' CHECK(state IN('reserved','started','completed','quiescent')),
 child_pid INTEGER CHECK(child_pid IS NULL OR (typeof(child_pid)='integer' AND child_pid>0 AND child_pid<=2147483647)),
 request_id TEXT,
 demand_job_id TEXT,
 owned_job_id TEXT,
 owned_generation INTEGER,
 owned_claim_token_digest TEXT,
 cleanup_kind TEXT,
 sealed_at TEXT,
 CHECK((owned_job_id IS NULL AND owned_generation IS NULL AND owned_claim_token_digest IS NULL)
    OR (owned_job_id IS NOT NULL AND owned_job_id=demand_job_id AND typeof(owned_generation)='integer' AND owned_generation>0 AND typeof(owned_claim_token_digest)='text' AND length(owned_claim_token_digest)=64 AND owned_claim_token_digest NOT GLOB '*[^0-9a-f]*')),
 CHECK((state='reserved' AND child_pid IS NULL AND request_id IS NULL AND demand_job_id IS NULL AND owned_job_id IS NULL AND cleanup_kind IS NULL AND sealed_at IS NULL)
    OR (state='started' AND child_pid IS NOT NULL AND request_id IS NULL AND demand_job_id IS NULL AND owned_job_id IS NULL AND cleanup_kind IS NULL AND sealed_at IS NULL)
    OR (state='completed' AND child_pid IS NOT NULL AND typeof(request_id)='text' AND length(request_id)>0 AND typeof(demand_job_id)='text' AND length(demand_job_id)>0 AND cleanup_kind IS NULL AND sealed_at IS NULL)
    OR (state='quiescent' AND child_pid IS NOT NULL AND typeof(request_id)='text' AND length(request_id)>0 AND typeof(demand_job_id)='text' AND length(demand_job_id)>0 AND typeof(cleanup_kind)='text' AND cleanup_kind='closed_child_exit_v1' AND typeof(sealed_at)='text' AND length(sealed_at)>0))
);
CREATE INDEX native_observation_companion_claim ON native_observation_companions(owned_job_id,owned_generation,owned_claim_token_digest) WHERE state='quiescent';
CREATE TRIGGER native_observation_companions_insert_guard BEFORE INSERT ON native_observation_companions BEGIN
 SELECT CASE WHEN NEW.state!='reserved' OR NOT EXISTS(SELECT 1 FROM catalog_state WHERE id=1 AND catalog_id=NEW.catalog_id AND incarnation=NEW.incarnation) THEN RAISE(ABORT,'invalid_observation_companion_reservation') END;
END;
CREATE TRIGGER native_observation_companions_update_guard BEFORE UPDATE ON native_observation_companions BEGIN
 SELECT CASE WHEN NEW.reservation_nonce IS NOT OLD.reservation_nonce OR NEW.catalog_id IS NOT OLD.catalog_id OR NEW.incarnation IS NOT OLD.incarnation OR NEW.protocol_kind IS NOT OLD.protocol_kind OR NEW.protocol_version IS NOT OLD.protocol_version OR NEW.helper_digest IS NOT OLD.helper_digest OR NEW.profile IS NOT OLD.profile OR NEW.source IS NOT OLD.source OR NEW.requester IS NOT OLD.requester OR NEW.args_digest IS NOT OLD.args_digest OR NOT EXISTS(SELECT 1 FROM catalog_state WHERE id=1 AND catalog_id=NEW.catalog_id AND incarnation=NEW.incarnation) THEN RAISE(ABORT,'immutable_observation_companion_identity') END;
 SELECT CASE WHEN NOT ((OLD.state='reserved' AND NEW.state='started') OR (OLD.state='started' AND NEW.state='completed' AND NEW.child_pid IS OLD.child_pid) OR (OLD.state='completed' AND NEW.state='quiescent' AND NEW.child_pid IS OLD.child_pid AND NEW.request_id IS OLD.request_id AND NEW.demand_job_id IS OLD.demand_job_id AND NEW.owned_job_id IS OLD.owned_job_id AND NEW.owned_generation IS OLD.owned_generation AND NEW.owned_claim_token_digest IS OLD.owned_claim_token_digest)) THEN RAISE(ABORT,'invalid_observation_companion_transition') END;
 SELECT CASE WHEN NEW.state IN('completed','quiescent') AND NOT EXISTS(SELECT 1 FROM observation_requests r JOIN observation_jobs j ON j.id=r.job_id WHERE r.id=NEW.request_id AND r.profile=NEW.profile AND r.source=NEW.source AND r.requester=NEW.requester AND r.job_id=NEW.demand_job_id AND j.profile=NEW.profile AND j.source=NEW.source) THEN RAISE(ABORT,'invalid_observation_companion_demand') END;
 SELECT CASE WHEN NEW.owned_job_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM observation_jobs WHERE id=NEW.owned_job_id AND profile=NEW.profile AND source=NEW.source AND generation=NEW.owned_generation AND worker_pid=NEW.child_pid AND typeof(claim_token)='text' AND length(claim_token)>0) THEN RAISE(ABORT,'invalid_observation_companion_claim') END;
END;
CREATE TRIGGER native_observation_companions_delete_guard BEFORE DELETE ON native_observation_companions BEGIN SELECT RAISE(ABORT,'observation_companions_are_immutable'); END;
CREATE TRIGGER catalog_change_native_observation_companions_insert AFTER INSERT ON native_observation_companions BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES('native_observation_companions',json_array(NEW.reservation_nonce),1,0) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
END;
CREATE TRIGGER catalog_change_native_observation_companions_update AFTER UPDATE ON native_observation_companions BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)=9223372036854775807 THEN RAISE(ABORT,'catalog_generation_exhausted') END;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES('native_observation_companions',json_array(NEW.reservation_nonce),1,0) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
END;
