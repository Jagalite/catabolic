-- UNREGISTERED proposal. Apply only through the native migration owner after
-- the bootstrap/PUT/refresh capability guards and preservation proof qualify.
-- No old metadata, receipt, schema registry, or identity-pin tuple is rewritten.
CREATE TABLE catalog_nfo_states (
 item_id TEXT PRIMARY KEY NOT NULL CHECK(length(CAST(item_id AS BLOB)) BETWEEN 1 AND 256),
 current_revision INTEGER NOT NULL CHECK(typeof(current_revision)='integer' AND current_revision>=1),
 FOREIGN KEY(item_id,current_revision) REFERENCES catalog_nfo_state_revisions(item_id,revision) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE catalog_nfo_state_revisions (
 item_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=1),
 source_revision INTEGER NOT NULL CHECK(typeof(source_revision)='integer' AND source_revision>=1),
 source_namespace TEXT NOT NULL DEFAULT 'motion.nfo' CHECK(source_namespace='motion.nfo'),
 source_record_id TEXT NOT NULL DEFAULT 'document' CHECK(source_record_id='document'),
 public_incarnation INTEGER NOT NULL CHECK(typeof(public_incarnation)='integer' AND public_incarnation>=1),
 public_revision INTEGER CHECK(public_revision IS NULL OR (typeof(public_revision)='integer' AND public_revision>=1)),
 document_identity TEXT CHECK(document_identity IS NULL OR length(CAST(document_identity AS BLOB)) BETWEEN 1 AND 1024),
 active_identity TEXT CHECK(active_identity IS NULL OR length(CAST(active_identity AS BLOB)) BETWEEN 1 AND 1024),
 origin_json TEXT CHECK(origin_json IS NULL OR (length(CAST(origin_json AS BLOB))<=65536 AND json_valid(origin_json) AND json_type(origin_json)='object')),
 origin_physical_json TEXT CHECK(origin_physical_json IS NULL OR (length(CAST(origin_physical_json AS BLOB))<=65536 AND json_valid(origin_physical_json) AND json_type(origin_physical_json)='object')),
 origin_proof_digest TEXT CHECK(origin_proof_digest IS NULL OR (length(origin_proof_digest)=64 AND origin_proof_digest NOT GLOB '*[^0-9a-f]*')),
 source_proof_digest TEXT NOT NULL CHECK(length(source_proof_digest)=64 AND source_proof_digest NOT GLOB '*[^0-9a-f]*'),
 attribution_audit_digest TEXT NOT NULL CHECK(length(attribution_audit_digest)=64 AND attribution_audit_digest NOT GLOB '*[^0-9a-f]*'),
 command_catalog_id TEXT NOT NULL,
 command_host_namespace TEXT NOT NULL,
 command_operation_id TEXT NOT NULL,
 PRIMARY KEY(item_id,revision),
 CHECK(public_revision IS NOT NULL OR (document_identity IS NULL AND active_identity IS NULL AND origin_json IS NULL)),
 CHECK(active_identity IS NULL OR active_identity IS document_identity),
 CHECK((origin_physical_json IS NULL AND origin_proof_digest IS NULL) OR (origin_physical_json IS NOT NULL AND origin_proof_digest IS NOT NULL AND coalesce(json_extract(origin_json,'$.provenance.kind')='imported',0))),
 CHECK(coalesce(json_extract(origin_json,'$.provenance.kind'),'none')<>'imported' OR (origin_physical_json IS NOT NULL AND origin_proof_digest IS NOT NULL)),
 FOREIGN KEY(item_id,source_namespace,source_record_id,source_revision) REFERENCES catalog_metadata_history(item_id,namespace,record_id,revision) DEFERRABLE INITIALLY DEFERRED,
 FOREIGN KEY(item_id) REFERENCES catalog_nfo_states(item_id) DEFERRABLE INITIALLY DEFERRED,
 FOREIGN KEY(command_catalog_id,command_host_namespace,command_operation_id) REFERENCES catalog_command_receipts(catalog_id,host_namespace,operation_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE catalog_nfo_identity_claims (
 value TEXT PRIMARY KEY NOT NULL CHECK(length(CAST(value AS BLOB)) BETWEEN 1 AND 1024),
 item_id TEXT NOT NULL UNIQUE,
 state_revision INTEGER NOT NULL,
 FOREIGN KEY(item_id,state_revision) REFERENCES catalog_nfo_state_revisions(item_id,revision) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE catalog_nfo_file_attributions (
 item_id TEXT PRIMARY KEY NOT NULL,
 profile TEXT NOT NULL CHECK(length(CAST(profile AS BLOB)) BETWEEN 1 AND 256),
 file_id TEXT NOT NULL CHECK(length(CAST(file_id AS BLOB)) BETWEEN 1 AND 256),
 state_revision INTEGER NOT NULL,
 FOREIGN KEY(item_id,state_revision) REFERENCES catalog_nfo_state_revisions(item_id,revision) DEFERRABLE INITIALLY DEFERRED
);
-- Nonunique by design: all old subject owners must participate in withdrawal.
CREATE INDEX catalog_nfo_file_owners ON catalog_nfo_file_attributions(profile,file_id,item_id);
CREATE TABLE catalog_nfo_bootstrap (
 id INTEGER PRIMARY KEY CHECK(id=1),
 catalog_id TEXT NOT NULL,
 incarnation TEXT NOT NULL,
 inventory_digest TEXT NOT NULL CHECK(length(inventory_digest)=64 AND inventory_digest NOT GLOB '*[^0-9a-f]*'),
 source_count INTEGER NOT NULL CHECK(typeof(source_count)='integer' AND source_count>=0),
 command_catalog_id TEXT NOT NULL,
 command_host_namespace TEXT NOT NULL,
 command_operation_id TEXT NOT NULL,
 FOREIGN KEY(command_catalog_id,command_host_namespace,command_operation_id) REFERENCES catalog_command_receipts(catalog_id,host_namespace,operation_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER catalog_nfo_history_no_update BEFORE UPDATE ON catalog_nfo_state_revisions
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_history_immutable'); END;
CREATE TRIGGER catalog_nfo_history_no_delete BEFORE DELETE ON catalog_nfo_state_revisions
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_history_retained'); END;
CREATE TRIGGER catalog_nfo_state_no_delete BEFORE DELETE ON catalog_nfo_states
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_state_retained'); END;
CREATE TRIGGER catalog_nfo_bootstrap_no_update BEFORE UPDATE ON catalog_nfo_bootstrap
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_bootstrap_immutable'); END;
CREATE TRIGGER catalog_nfo_bootstrap_no_delete BEFORE DELETE ON catalog_nfo_bootstrap
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_bootstrap_retained'); END;
CREATE TRIGGER catalog_nfo_history_sequence BEFORE INSERT ON catalog_nfo_state_revisions
 WHEN NEW.revision IS NOT coalesce((SELECT current_revision+1 FROM catalog_nfo_states WHERE item_id=NEW.item_id),1)
 OR NEW.command_catalog_id IS NOT (SELECT catalog_id FROM catalog_state WHERE id=1)
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_history_sequence'); END;
CREATE TRIGGER catalog_nfo_state_insert_guard BEFORE INSERT ON catalog_nfo_states
 WHEN NEW.current_revision IS NOT 1
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_state_sequence'); END;
CREATE TRIGGER catalog_nfo_state_update_guard BEFORE UPDATE ON catalog_nfo_states
 WHEN NEW.item_id IS NOT OLD.item_id OR NEW.current_revision IS NOT OLD.current_revision+1
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_state_sequence'); END;
CREATE TRIGGER catalog_nfo_claim_no_update BEFORE UPDATE ON catalog_nfo_identity_claims
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_claim_replace_explicitly'); END;
CREATE TRIGGER catalog_nfo_file_no_update BEFORE UPDATE ON catalog_nfo_file_attributions
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_file_replace_explicitly'); END;
CREATE TRIGGER catalog_nfo_claim_insert_guard BEFORE INSERT ON catalog_nfo_identity_claims
 WHEN NOT EXISTS(SELECT 1 FROM catalog_nfo_states s JOIN catalog_nfo_state_revisions h
 ON h.item_id=s.item_id AND h.revision=s.current_revision
 WHERE s.item_id=NEW.item_id AND s.current_revision=NEW.state_revision AND h.active_identity IS NEW.value)
 OR EXISTS(SELECT 1 FROM catalog_identity_pins WHERE namespace='nfo' AND value=NEW.value AND item_id<>NEW.item_id)
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_claim_owner_mismatch'); END;
CREATE TRIGGER catalog_nfo_file_insert_guard BEFORE INSERT ON catalog_nfo_file_attributions
 WHEN NOT EXISTS(SELECT 1 FROM catalog_nfo_states s JOIN catalog_nfo_state_revisions h
 ON h.item_id=s.item_id AND h.revision=s.current_revision
 WHERE s.item_id=NEW.item_id AND s.current_revision=NEW.state_revision
 AND json_extract(h.origin_json,'$.file.profile') IS NEW.profile
 AND json_extract(h.origin_json,'$.file.file_id') IS NEW.file_id)
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_file_owner_mismatch'); END;
CREATE TRIGGER catalog_nfo_states_generation_insert AFTER INSERT ON catalog_nfo_states BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807
 THEN RAISE(ABORT,'catalog generation exhausted') END;
 SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_nfo_states' AND entity_key=json_array(NEW.item_id)),0)>=9223372036854775807
 THEN RAISE(ABORT,'NFO entity invalidation revision exhausted') END;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
 VALUES('catalog_nfo_states',json_array(NEW.item_id),1,0)
 ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
END;
CREATE TRIGGER catalog_nfo_states_generation_update AFTER UPDATE ON catalog_nfo_states BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807
 THEN RAISE(ABORT,'catalog generation exhausted') END;
 SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_nfo_states' AND entity_key=json_array(NEW.item_id)),0)>=9223372036854775807
 THEN RAISE(ABORT,'NFO entity invalidation revision exhausted') END;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
 VALUES('catalog_nfo_states',json_array(NEW.item_id),1,0)
 ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
END;
CREATE TRIGGER catalog_nfo_state_revisions_generation_insert AFTER INSERT ON catalog_nfo_state_revisions BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807
 THEN RAISE(ABORT,'catalog generation exhausted') END;
 SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_nfo_state_revisions' AND entity_key=json_array(NEW.item_id,NEW.revision)),0)>=9223372036854775807
 THEN RAISE(ABORT,'NFO entity invalidation revision exhausted') END;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
 VALUES('catalog_nfo_state_revisions',json_array(NEW.item_id,NEW.revision),1,0)
 ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
END;
CREATE TRIGGER catalog_nfo_identity_claims_generation_insert AFTER INSERT ON catalog_nfo_identity_claims BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807
 THEN RAISE(ABORT,'catalog generation exhausted') END;
 SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_nfo_identity_claims' AND entity_key=json_array(NEW.value)),0)>=9223372036854775807
 THEN RAISE(ABORT,'NFO entity invalidation revision exhausted') END;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
 VALUES('catalog_nfo_identity_claims',json_array(NEW.value),1,0)
 ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
END;
CREATE TRIGGER catalog_nfo_identity_claims_generation_delete AFTER DELETE ON catalog_nfo_identity_claims BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807
 THEN RAISE(ABORT,'catalog generation exhausted') END;
 SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_nfo_identity_claims' AND entity_key=json_array(OLD.value)),0)>=9223372036854775807
 THEN RAISE(ABORT,'NFO entity invalidation revision exhausted') END;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
 VALUES('catalog_nfo_identity_claims',json_array(OLD.value),1,1)
 ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=1;
END;
CREATE TRIGGER catalog_nfo_file_attributions_generation_insert AFTER INSERT ON catalog_nfo_file_attributions BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807
 THEN RAISE(ABORT,'catalog generation exhausted') END;
 SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_nfo_file_attributions' AND entity_key=json_array(NEW.item_id)),0)>=9223372036854775807
 THEN RAISE(ABORT,'NFO entity invalidation revision exhausted') END;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
 VALUES('catalog_nfo_file_attributions',json_array(NEW.item_id),1,0)
 ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
END;
CREATE TRIGGER catalog_nfo_file_attributions_generation_delete AFTER DELETE ON catalog_nfo_file_attributions BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807
 THEN RAISE(ABORT,'catalog generation exhausted') END;
 SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_nfo_file_attributions' AND entity_key=json_array(OLD.item_id)),0)>=9223372036854775807
 THEN RAISE(ABORT,'NFO entity invalidation revision exhausted') END;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
 VALUES('catalog_nfo_file_attributions',json_array(OLD.item_id),1,1)
 ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=1;
END;
CREATE TRIGGER catalog_nfo_bootstrap_generation_insert AFTER INSERT ON catalog_nfo_bootstrap BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807
 THEN RAISE(ABORT,'catalog generation exhausted') END;
 SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_nfo_bootstrap' AND entity_key=json_array(NEW.id)),0)>=9223372036854775807
 THEN RAISE(ABORT,'NFO entity invalidation revision exhausted') END;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
 VALUES('catalog_nfo_bootstrap',json_array(NEW.id),1,0)
 ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
END;
CREATE TRIGGER catalog_nfo_pin_collision_insert BEFORE INSERT ON catalog_identity_pins
 WHEN NEW.namespace='nfo' AND NEW.value IS NOT NULL
 AND EXISTS(SELECT 1 FROM catalog_nfo_identity_claims WHERE value=NEW.value AND item_id<>NEW.item_id)
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_identity_owned'); END;
CREATE TRIGGER catalog_nfo_pin_collision_update BEFORE UPDATE ON catalog_identity_pins
 WHEN NEW.namespace='nfo' AND NEW.value IS NOT NULL
 AND EXISTS(SELECT 1 FROM catalog_nfo_identity_claims WHERE value=NEW.value AND item_id<>NEW.item_id)
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_identity_owned'); END;

-- Closed-owner source effects, independent of the generic metadata permit.
-- No public namespace/flag or existing metadata permit can mint this capability.
CREATE TABLE catalog_nfo_source_effect_permits (
 item_id TEXT PRIMARY KEY NOT NULL CHECK(length(CAST(item_id AS BLOB)) BETWEEN 1 AND 256),
 incarnation TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=1),
 command_catalog_id TEXT NOT NULL,
 command_host_namespace TEXT NOT NULL,
 command_operation_id TEXT NOT NULL,
 outer_digest TEXT NOT NULL CHECK(length(outer_digest)=64 AND outer_digest NOT GLOB '*[^0-9a-f]*'),
 raw_mutation_json TEXT NOT NULL CHECK(length(CAST(raw_mutation_json AS BLOB))<=262144),
 mode TEXT NOT NULL CHECK(mode IN ('publication','import')),
 barrier_id INTEGER NOT NULL DEFAULT 1 CHECK(barrier_id=1),
 FOREIGN KEY(barrier_id) REFERENCES catalog_commit_barrier(id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER catalog_nfo_source_history_guard BEFORE INSERT ON catalog_metadata_history
 WHEN NEW.namespace='motion.nfo' AND (NEW.record_id<>'document' OR NEW.source_kind<>'provider' OR NOT EXISTS(
 SELECT 1 FROM catalog_nfo_source_effect_permits p WHERE p.item_id=NEW.item_id AND p.revision=NEW.revision
 AND p.incarnation=(SELECT incarnation FROM catalog_state WHERE id=1)
 AND p.command_catalog_id=NEW.command_catalog_id AND p.command_host_namespace=NEW.command_host_namespace
 AND p.command_operation_id=NEW.command_operation_id AND p.raw_mutation_json=NEW.raw_mutation_json
 AND (p.mode='publication' OR NOT EXISTS(SELECT 1 FROM catalog_nfo_bootstrap))))
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_source_capability_required'); END;
CREATE TRIGGER catalog_nfo_source_insert_guard BEFORE INSERT ON catalog_metadata_sources
 WHEN NEW.namespace='motion.nfo' AND NOT EXISTS(
 SELECT 1 FROM catalog_nfo_source_effect_permits p JOIN catalog_metadata_history h
 ON h.item_id=p.item_id AND h.namespace='motion.nfo' AND h.record_id='document' AND h.revision=p.revision
 WHERE NEW.item_id=p.item_id AND NEW.record_id='document' AND NEW.revision=p.revision
 AND NEW.source_kind=h.source_kind AND NEW.schema_fingerprint=h.schema_fingerprint
 AND NEW.fields_json=h.fields_json AND NEW.evidence_json=h.evidence_json
 AND p.incarnation=(SELECT incarnation FROM catalog_state WHERE id=1)
 AND (p.mode='publication' OR NOT EXISTS(SELECT 1 FROM catalog_nfo_bootstrap)))
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_source_capability_required'); END;
CREATE TRIGGER catalog_nfo_source_update_guard BEFORE UPDATE ON catalog_metadata_sources
 WHEN (OLD.namespace='motion.nfo' OR NEW.namespace='motion.nfo') AND (
 NEW.item_id IS NOT OLD.item_id OR NEW.namespace IS NOT OLD.namespace OR NEW.record_id IS NOT OLD.record_id
 OR NOT EXISTS(SELECT 1 FROM catalog_nfo_source_effect_permits p JOIN catalog_metadata_history h
 ON h.item_id=p.item_id AND h.namespace='motion.nfo' AND h.record_id='document' AND h.revision=p.revision
 WHERE NEW.item_id=p.item_id AND NEW.record_id='document' AND NEW.revision=p.revision
 AND NEW.source_kind=h.source_kind AND NEW.schema_fingerprint=h.schema_fingerprint
 AND NEW.fields_json=h.fields_json AND NEW.evidence_json=h.evidence_json
 AND p.incarnation=(SELECT incarnation FROM catalog_state WHERE id=1)
 AND (p.mode='publication' OR NOT EXISTS(SELECT 1 FROM catalog_nfo_bootstrap))))
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_source_capability_required'); END;
CREATE TRIGGER catalog_nfo_source_delete_guard BEFORE DELETE ON catalog_metadata_sources
 WHEN OLD.namespace='motion.nfo' BEGIN SELECT RAISE(ABORT,'catalog_nfo_source_retained'); END;
CREATE TRIGGER catalog_nfo_item_tag_insert_guard BEFORE INSERT ON item_tags
 WHEN NEW.source='motion.nfo' AND NOT EXISTS(SELECT 1 FROM catalog_nfo_source_effect_permits p
 WHERE p.item_id=NEW.item_id AND p.incarnation=(SELECT incarnation FROM catalog_state WHERE id=1)
 AND (p.mode='publication' OR NOT EXISTS(SELECT 1 FROM catalog_nfo_bootstrap)))
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_assertion_capability_required'); END;
CREATE TRIGGER catalog_nfo_item_tag_update_guard BEFORE UPDATE ON item_tags
 WHEN (OLD.source='motion.nfo' OR NEW.source='motion.nfo') AND (
 NEW.id IS NOT OLD.id OR NEW.item_id IS NOT OLD.item_id OR NEW.source IS NOT OLD.source OR NEW.tag_id IS NOT OLD.tag_id
 OR NOT EXISTS(SELECT 1 FROM catalog_nfo_source_effect_permits p WHERE p.item_id=NEW.item_id
 AND p.incarnation=(SELECT incarnation FROM catalog_state WHERE id=1)
 AND (p.mode='publication' OR NOT EXISTS(SELECT 1 FROM catalog_nfo_bootstrap))))
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_assertion_capability_required'); END;
CREATE TRIGGER catalog_nfo_item_tag_delete_guard BEFORE DELETE ON item_tags
 WHEN OLD.source='motion.nfo' BEGIN SELECT RAISE(ABORT,'catalog_nfo_assertion_retained'); END;
CREATE TRIGGER catalog_nfo_file_tag_insert_guard BEFORE INSERT ON file_tags
 WHEN NEW.source='motion.nfo' BEGIN SELECT RAISE(ABORT,'catalog_nfo_file_assertion_unsupported'); END;
CREATE TRIGGER catalog_nfo_file_tag_update_guard BEFORE UPDATE ON file_tags
 WHEN OLD.source='motion.nfo' OR NEW.source='motion.nfo' BEGIN SELECT RAISE(ABORT,'catalog_nfo_file_assertion_unsupported'); END;
CREATE TRIGGER catalog_nfo_file_tag_delete_guard BEFORE DELETE ON file_tags
 WHEN OLD.source='motion.nfo' BEGIN SELECT RAISE(ABORT,'catalog_nfo_file_assertion_unsupported'); END;
CREATE TRIGGER catalog_nfo_tag_history_guard BEFORE INSERT ON catalog_metadata_tag_history
 WHEN NEW.namespace='motion.nfo' AND (NEW.record_id<>'document' OR NOT EXISTS(
 SELECT 1 FROM catalog_nfo_source_effect_permits p WHERE p.item_id=NEW.item_id AND p.revision=NEW.revision
 AND p.incarnation=(SELECT incarnation FROM catalog_state WHERE id=1)
 AND p.command_catalog_id=NEW.command_catalog_id AND p.command_host_namespace=NEW.command_host_namespace
 AND p.command_operation_id=NEW.command_operation_id AND p.mode='publication'))
 BEGIN SELECT RAISE(ABORT,'catalog_nfo_tag_history_capability_required'); END;
