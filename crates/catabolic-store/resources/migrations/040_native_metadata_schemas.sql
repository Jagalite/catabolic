-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors
-- SPDX-License-Identifier: MIT
-- Explicit metadata v2 schema registry. Baseline subjects and all historical
-- document bytes/revisions remain unchanged. No external execution authority.
CREATE TABLE catalog_metadata_schemas (
 item_id TEXT NOT NULL REFERENCES items(id) ON DELETE RESTRICT,
 schema_fingerprint TEXT NOT NULL CHECK(length(schema_fingerprint)=64 AND schema_fingerprint NOT GLOB '*[^0-9a-f]*'),
 contract_version INTEGER NOT NULL CHECK(contract_version IN(1,2)),
 policies_json TEXT NOT NULL CHECK(json_valid(policies_json) AND json_type(policies_json)='array' AND length(CAST(policies_json AS BLOB))<=2097152),
 system_fields_json TEXT NOT NULL CHECK(json_valid(system_fields_json) AND json_type(system_fields_json)='array' AND length(CAST(system_fields_json AS BLOB))<=32768),
 prior_fingerprint TEXT,
 command_catalog_id TEXT NOT NULL,
 command_host_namespace TEXT NOT NULL,
 command_operation_id TEXT NOT NULL,
 admitted_at_us INTEGER NOT NULL CHECK(typeof(admitted_at_us)='integer' AND admitted_at_us>=0),
 PRIMARY KEY(item_id,schema_fingerprint),
 FOREIGN KEY(item_id,prior_fingerprint) REFERENCES catalog_metadata_schemas(item_id,schema_fingerprint) ON DELETE RESTRICT
);
INSERT INTO catalog_metadata_schemas SELECT item_id,schema_fingerprint,contract_version,policies_json,'[]',NULL,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us FROM catalog_metadata_subjects;
CREATE TABLE catalog_metadata_subjects_v40 ( item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE RESTRICT, contract_version INTEGER NOT NULL CHECK(contract_version IN(1,2)), schema_fingerprint TEXT NOT NULL CHECK(length(schema_fingerprint)=64 AND schema_fingerprint NOT GLOB '*[^0-9a-f]*'), policies_json TEXT NOT NULL CHECK(json_valid(policies_json) AND json_type(policies_json)='array' AND ((contract_version=1 AND json_array_length(policies_json) BETWEEN 1 AND 64 AND length(CAST(policies_json AS BLOB))<=32768) OR (contract_version=2 AND length(CAST(policies_json AS BLOB))<=2097152))), legacy_raw_json TEXT NOT NULL CHECK(json_valid(legacy_raw_json) AND json_type(legacy_raw_json)='object' AND length(CAST(legacy_raw_json AS BLOB))<=65536), command_catalog_id TEXT NOT NULL, command_host_namespace TEXT NOT NULL, command_operation_id TEXT NOT NULL, admitted_at_us INTEGER NOT NULL CHECK(typeof(admitted_at_us)='integer' AND admitted_at_us>=0), UNIQUE(item_id,schema_fingerprint) );
INSERT INTO catalog_metadata_subjects_v40 SELECT * FROM catalog_metadata_subjects;



CREATE TABLE catalog_metadata_history_v40 (
    item_id TEXT NOT NULL,
    namespace TEXT NOT NULL CHECK(length(CAST(namespace AS BLOB)) BETWEEN 1 AND 256),
    record_id TEXT NOT NULL CHECK(length(CAST(record_id AS BLOB)) BETWEEN 1 AND 256),
    revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=1),
    source_kind TEXT NOT NULL CHECK(source_kind IN ('local','provider','scan','legacy_import')),
    schema_fingerprint TEXT NOT NULL,
    fields_json TEXT NOT NULL CHECK(json_valid(fields_json) AND json_type(fields_json)='object' AND length(CAST(fields_json AS BLOB))<=65536),
    evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json) AND json_type(evidence_json)='object' AND length(CAST(evidence_json AS BLOB))<=8192),
    raw_mutation_json TEXT NOT NULL CHECK(json_valid(raw_mutation_json) AND json_type(raw_mutation_json)='object' AND length(CAST(raw_mutation_json AS BLOB))<=262144),
    command_catalog_id TEXT NOT NULL,
    command_host_namespace TEXT NOT NULL,
    command_operation_id TEXT NOT NULL,
    admitted_at_us INTEGER NOT NULL CHECK(typeof(admitted_at_us)='integer' AND admitted_at_us>=0),
    PRIMARY KEY(item_id,namespace,record_id,revision),
    FOREIGN KEY(item_id,schema_fingerprint) REFERENCES catalog_metadata_schemas(item_id,schema_fingerprint) ON DELETE RESTRICT,
    CHECK(source_kind!='legacy_import' OR (namespace='catabolic.legacy_import.v1' AND record_id='items.metadata')),
    CHECK(namespace!='catabolic.legacy_import.v1' OR source_kind='legacy_import')
);

INSERT INTO catalog_metadata_history_v40 SELECT * FROM catalog_metadata_history;

CREATE TABLE catalog_metadata_sources_v40 (
    item_id TEXT NOT NULL,
    namespace TEXT NOT NULL CHECK(length(CAST(namespace AS BLOB)) BETWEEN 1 AND 256),
    record_id TEXT NOT NULL CHECK(length(CAST(record_id AS BLOB)) BETWEEN 1 AND 256),
    source_kind TEXT NOT NULL CHECK(source_kind IN ('local','provider','scan','legacy_import')),
    revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=1),
    schema_fingerprint TEXT NOT NULL,
    fields_json TEXT NOT NULL CHECK(json_valid(fields_json) AND json_type(fields_json)='object' AND length(CAST(fields_json AS BLOB))<=65536),
    evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json) AND json_type(evidence_json)='object' AND length(CAST(evidence_json AS BLOB))<=8192),
    PRIMARY KEY(item_id,namespace,record_id),
    FOREIGN KEY(item_id,schema_fingerprint) REFERENCES catalog_metadata_schemas(item_id,schema_fingerprint) ON DELETE RESTRICT,
    FOREIGN KEY(item_id,namespace,record_id,revision) REFERENCES catalog_metadata_history_v40(item_id,namespace,record_id,revision) ON DELETE RESTRICT,
    CHECK(source_kind!='legacy_import' OR (namespace='catabolic.legacy_import.v1' AND record_id='items.metadata')),
    CHECK(namespace!='catabolic.legacy_import.v1' OR source_kind='legacy_import')
);

INSERT INTO catalog_metadata_sources_v40 SELECT * FROM catalog_metadata_sources;

CREATE TABLE catalog_metadata_projection_v40 (
    item_id TEXT PRIMARY KEY,
    schema_fingerprint TEXT NOT NULL,
    resolution_json TEXT NOT NULL CHECK(json_valid(resolution_json) AND json_type(resolution_json)='object' AND length(CAST(resolution_json AS BLOB))<=8388608),
    effective_json TEXT NOT NULL CHECK(json_valid(effective_json) AND json_type(effective_json)='object' AND length(CAST(effective_json AS BLOB))<=2097152),
    FOREIGN KEY(item_id,schema_fingerprint) REFERENCES catalog_metadata_schemas(item_id,schema_fingerprint) ON DELETE RESTRICT
);

INSERT INTO catalog_metadata_projection_v40 SELECT * FROM catalog_metadata_projection;

DROP TABLE catalog_metadata_sources;

DROP TABLE catalog_metadata_projection;

DROP TABLE catalog_metadata_history;

DROP TRIGGER catalog_metadata_generation_subjects_insert;
DROP TRIGGER catalog_metadata_items_delete;
DROP TRIGGER catalog_metadata_items_rekey;
DROP TRIGGER catalog_metadata_items_update;
DROP TRIGGER catalog_metadata_subject_delete;
DROP TRIGGER catalog_metadata_subject_insert;
DROP TRIGGER catalog_metadata_subject_receipt_obligation;
DROP TRIGGER catalog_metadata_subject_update;
DROP TRIGGER catalog_metadata_subjects_uncommitted;
DROP TRIGGER catalog_search_input_catalog_metadata_subjects_delete;
DROP TRIGGER catalog_search_input_catalog_metadata_subjects_insert;
DROP TRIGGER catalog_search_input_catalog_metadata_subjects_update;
DROP TABLE catalog_metadata_subjects;
ALTER TABLE catalog_metadata_subjects_v40 RENAME TO catalog_metadata_subjects;
CREATE TABLE catalog_metadata_schema_heads (
 item_id TEXT PRIMARY KEY REFERENCES catalog_metadata_subjects(item_id) ON DELETE RESTRICT,
 schema_fingerprint TEXT NOT NULL,
 FOREIGN KEY(item_id,schema_fingerprint) REFERENCES catalog_metadata_schemas(item_id,schema_fingerprint) ON DELETE RESTRICT
);
INSERT INTO catalog_metadata_schema_heads SELECT item_id,schema_fingerprint FROM catalog_metadata_subjects;
CREATE TRIGGER catalog_metadata_generation_subjects_insert AFTER INSERT ON catalog_metadata_subjects BEGIN SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807 THEN RAISE(ABORT,'catalog generation exhausted') END; SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_metadata_subjects' AND entity_key=json_array(NEW.item_id)),0)>=9223372036854775807 THEN RAISE(ABORT,'metadata entity invalidation revision exhausted') END; UPDATE catalog_state SET generation=generation+1 WHERE id=1; INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES('catalog_metadata_subjects',json_array(NEW.item_id),1,0) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0; END;
CREATE TRIGGER catalog_metadata_items_delete BEFORE DELETE ON items WHEN EXISTS(SELECT 1 FROM catalog_metadata_subjects WHERE item_id=OLD.id) BEGIN SELECT RAISE(ABORT,'managed metadata subject deletion requires retained retirement'); END;
CREATE TRIGGER catalog_metadata_items_rekey BEFORE UPDATE OF id ON items WHEN NEW.id!=OLD.id AND EXISTS(SELECT 1 FROM catalog_metadata_subjects WHERE item_id=OLD.id) BEGIN SELECT RAISE(ABORT,'managed metadata subject identity is immutable'); END;
CREATE TRIGGER catalog_metadata_items_update BEFORE UPDATE OF metadata ON items WHEN EXISTS(SELECT 1 FROM catalog_metadata_subjects WHERE item_id=OLD.id) AND NEW.metadata IS NOT OLD.metadata BEGIN SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation WHERE p.item_id=OLD.id) THEN RAISE(ABORT,'managed metadata changes require typed owner') END; END;
CREATE TRIGGER catalog_metadata_subject_delete BEFORE DELETE ON catalog_metadata_subjects BEGIN SELECT RAISE(ABORT,'metadata subjects require explicit retained retirement'); END;
CREATE TRIGGER catalog_metadata_subject_insert BEFORE INSERT ON catalog_metadata_subjects BEGIN SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation WHERE p.item_id=NEW.item_id) THEN RAISE(ABORT,'metadata subject requires typed owner') END; SELECT CASE WHEN NEW.legacy_raw_json IS NOT (SELECT metadata FROM items WHERE id=NEW.item_id) THEN RAISE(ABORT,'metadata initialization must preserve exact current bytes') END; END;
CREATE TRIGGER catalog_metadata_subject_receipt_obligation AFTER INSERT ON catalog_metadata_subjects BEGIN INSERT INTO catalog_metadata_receipt_obligations(command_catalog_id,command_host_namespace,command_operation_id) SELECT NEW.command_catalog_id,NEW.command_host_namespace,NEW.command_operation_id WHERE NOT EXISTS(SELECT 1 FROM catalog_metadata_receipt_obligations WHERE command_catalog_id=NEW.command_catalog_id AND command_host_namespace=NEW.command_host_namespace AND command_operation_id=NEW.command_operation_id); END;
CREATE TRIGGER catalog_metadata_subject_update BEFORE UPDATE ON catalog_metadata_subjects BEGIN SELECT RAISE(ABORT,'metadata schemas and original import bytes are immutable'); END;
CREATE TRIGGER catalog_metadata_subjects_uncommitted BEFORE INSERT ON catalog_metadata_subjects WHEN EXISTS (SELECT 1 FROM catalog_command_receipts r WHERE r.catalog_id=NEW.command_catalog_id AND r.host_namespace=NEW.command_host_namespace AND r.operation_id=NEW.command_operation_id) BEGIN SELECT RAISE(ABORT,'catalog_committed_history_sealed'); END;
CREATE TRIGGER catalog_search_input_catalog_metadata_subjects_delete AFTER DELETE ON catalog_metadata_subjects BEGIN INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id); UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty); END;
CREATE TRIGGER catalog_search_input_catalog_metadata_subjects_insert AFTER INSERT ON catalog_metadata_subjects BEGIN INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id); UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty); END;
CREATE TRIGGER catalog_search_input_catalog_metadata_subjects_update AFTER UPDATE ON catalog_metadata_subjects BEGIN INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id); INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id); UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty); END;
ALTER TABLE catalog_metadata_history_v40 RENAME TO catalog_metadata_history;

ALTER TABLE catalog_metadata_sources_v40 RENAME TO catalog_metadata_sources;

ALTER TABLE catalog_metadata_projection_v40 RENAME TO catalog_metadata_projection;

CREATE UNIQUE INDEX catalog_metadata_one_local ON catalog_metadata_sources(item_id) WHERE source_kind='local';

CREATE UNIQUE INDEX catalog_metadata_one_legacy ON catalog_metadata_sources(item_id) WHERE source_kind='legacy_import';

CREATE TRIGGER catalog_metadata_history_insert BEFORE INSERT ON catalog_metadata_history BEGIN
    SELECT CASE WHEN NEW.schema_fingerprint IS NOT (SELECT schema_fingerprint FROM catalog_metadata_schema_heads WHERE item_id=NEW.item_id) THEN RAISE(ABORT,'metadata write requires current schema head') END;
    SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation WHERE p.item_id=NEW.item_id)
        THEN RAISE(ABORT,'metadata history requires typed owner') END;
    SELECT CASE WHEN NEW.revision != coalesce((SELECT max(revision) FROM catalog_metadata_history WHERE item_id=NEW.item_id AND namespace=NEW.namespace AND record_id=NEW.record_id),0)+1
        THEN RAISE(ABORT,'metadata history revision is not consecutive') END;
    SELECT CASE WHEN EXISTS(SELECT 1 FROM catalog_metadata_sources WHERE item_id=NEW.item_id AND namespace=NEW.namespace AND record_id=NEW.record_id AND source_kind!=NEW.source_kind)
        THEN RAISE(ABORT,'metadata source kind is immutable') END;
END;

CREATE TRIGGER catalog_metadata_history_update BEFORE UPDATE ON catalog_metadata_history BEGIN
    SELECT RAISE(ABORT,'metadata history is immutable');
END;

CREATE TRIGGER catalog_metadata_history_delete BEFORE DELETE ON catalog_metadata_history BEGIN
    SELECT RAISE(ABORT,'metadata history cannot be deleted');
END;

CREATE TRIGGER catalog_metadata_source_insert BEFORE INSERT ON catalog_metadata_sources BEGIN
    SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation WHERE p.item_id=NEW.item_id)
        THEN RAISE(ABORT,'metadata source requires typed owner') END;
    -- UPSERT invokes the insert trigger for existing keys too. Existing heads
    -- are checked by the update trigger rather than mistaken for recreation.
    SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_sources WHERE item_id=NEW.item_id AND namespace=NEW.namespace AND record_id=NEW.record_id) AND (NEW.revision!=1 OR (SELECT count(*) FROM catalog_metadata_sources WHERE item_id=NEW.item_id)>=128)
        THEN RAISE(ABORT,'metadata new-source revision or source budget invalid') END;
    SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_history h WHERE h.item_id=NEW.item_id AND h.namespace=NEW.namespace AND h.record_id=NEW.record_id AND h.revision=NEW.revision AND h.source_kind=NEW.source_kind AND h.schema_fingerprint=NEW.schema_fingerprint AND h.fields_json=NEW.fields_json AND h.evidence_json=NEW.evidence_json)
        THEN RAISE(ABORT,'metadata head must match retained history') END;
END;

CREATE TRIGGER catalog_metadata_source_update BEFORE UPDATE ON catalog_metadata_sources BEGIN
    SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation WHERE p.item_id=NEW.item_id)
        THEN RAISE(ABORT,'metadata source requires typed owner') END;
    SELECT CASE WHEN NEW.item_id!=OLD.item_id OR NEW.namespace!=OLD.namespace OR NEW.record_id!=OLD.record_id OR NEW.source_kind!=OLD.source_kind OR OLD.revision=9223372036854775807 OR NEW.revision!=OLD.revision+1
        THEN RAISE(ABORT,'metadata source identity or revision invalid') END;
    SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_history h WHERE h.item_id=NEW.item_id AND h.namespace=NEW.namespace AND h.record_id=NEW.record_id AND h.revision=NEW.revision AND h.source_kind=NEW.source_kind AND h.schema_fingerprint=NEW.schema_fingerprint AND h.fields_json=NEW.fields_json AND h.evidence_json=NEW.evidence_json)
        THEN RAISE(ABORT,'metadata head must match retained history') END;
END;

CREATE TRIGGER catalog_metadata_source_delete BEFORE DELETE ON catalog_metadata_sources BEGIN
    SELECT RAISE(ABORT,'metadata source heads must remain revisioned, including empty heads');
END;

CREATE TRIGGER catalog_metadata_projection_insert BEFORE INSERT ON catalog_metadata_projection BEGIN
    SELECT CASE WHEN NEW.schema_fingerprint IS NOT (SELECT schema_fingerprint FROM catalog_metadata_schema_heads WHERE item_id=NEW.item_id) THEN RAISE(ABORT,'metadata write requires current schema head') END;
    SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation WHERE p.item_id=NEW.item_id)
        THEN RAISE(ABORT,'metadata projection requires typed owner') END;
END;

CREATE TRIGGER catalog_metadata_projection_update BEFORE UPDATE ON catalog_metadata_projection BEGIN
    SELECT CASE WHEN NEW.schema_fingerprint IS NOT (SELECT schema_fingerprint FROM catalog_metadata_schema_heads WHERE item_id=NEW.item_id) THEN RAISE(ABORT,'metadata write requires current schema head') END;
    SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation WHERE p.item_id=NEW.item_id)
        THEN RAISE(ABORT,'metadata projection requires typed owner') END;
    SELECT CASE WHEN NEW.item_id!=OLD.item_id
        THEN RAISE(ABORT,'metadata projection identity is immutable') END;
END;

CREATE TRIGGER catalog_metadata_projection_delete BEFORE DELETE ON catalog_metadata_projection BEGIN
    SELECT RAISE(ABORT,'metadata projection must be rebuilt in place');
END;

CREATE TRIGGER catalog_metadata_generation_history_insert AFTER INSERT ON catalog_metadata_history BEGIN
    SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807
        THEN RAISE(ABORT,'catalog generation exhausted') END;
    SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_metadata_history' AND entity_key=json_array(NEW.item_id,NEW.namespace,NEW.record_id,NEW.revision)),0)>=9223372036854775807
        THEN RAISE(ABORT,'metadata entity invalidation revision exhausted') END;
    UPDATE catalog_state SET generation=generation+1 WHERE id=1;
    INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
        VALUES('catalog_metadata_history',json_array(NEW.item_id,NEW.namespace,NEW.record_id,NEW.revision),1,0)
        ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
END;

CREATE TRIGGER catalog_metadata_generation_sources_insert AFTER INSERT ON catalog_metadata_sources BEGIN
    SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807
        THEN RAISE(ABORT,'catalog generation exhausted') END;
    SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_metadata_sources' AND entity_key=json_array(NEW.item_id,NEW.namespace,NEW.record_id)),0)>=9223372036854775807
        THEN RAISE(ABORT,'metadata entity invalidation revision exhausted') END;
    UPDATE catalog_state SET generation=generation+1 WHERE id=1;
    INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
        VALUES('catalog_metadata_sources',json_array(NEW.item_id,NEW.namespace,NEW.record_id),1,0)
        ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
END;

CREATE TRIGGER catalog_metadata_generation_sources_update AFTER UPDATE ON catalog_metadata_sources BEGIN
    SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807
        THEN RAISE(ABORT,'catalog generation exhausted') END;
    SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_metadata_sources' AND entity_key=json_array(NEW.item_id,NEW.namespace,NEW.record_id)),0)>=9223372036854775807
        THEN RAISE(ABORT,'metadata entity invalidation revision exhausted') END;
    UPDATE catalog_state SET generation=generation+1 WHERE id=1;
    INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
        VALUES('catalog_metadata_sources',json_array(NEW.item_id,NEW.namespace,NEW.record_id),1,0)
        ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
END;

CREATE TRIGGER catalog_metadata_generation_projection_insert AFTER INSERT ON catalog_metadata_projection BEGIN
    SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807
        THEN RAISE(ABORT,'catalog generation exhausted') END;
    SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_metadata_projection' AND entity_key=json_array(NEW.item_id)),0)>=9223372036854775807
        THEN RAISE(ABORT,'metadata entity invalidation revision exhausted') END;
    UPDATE catalog_state SET generation=generation+1 WHERE id=1;
    INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
        VALUES('catalog_metadata_projection',json_array(NEW.item_id),1,0)
        ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
END;

CREATE TRIGGER catalog_metadata_generation_projection_update AFTER UPDATE ON catalog_metadata_projection BEGIN
    SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807
        THEN RAISE(ABORT,'catalog generation exhausted') END;
    SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_metadata_projection' AND entity_key=json_array(NEW.item_id)),0)>=9223372036854775807
        THEN RAISE(ABORT,'metadata entity invalidation revision exhausted') END;
    UPDATE catalog_state SET generation=generation+1 WHERE id=1;
    INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
        VALUES('catalog_metadata_projection',json_array(NEW.item_id),1,0)
        ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
END;

CREATE TRIGGER catalog_metadata_schema_insert BEFORE INSERT ON catalog_metadata_schemas BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation WHERE p.item_id=NEW.item_id) THEN RAISE(ABORT,'metadata schema requires typed owner') END;
 SELECT CASE WHEN NEW.prior_fingerprint IS NOT NULL AND NEW.prior_fingerprint IS NOT (SELECT schema_fingerprint FROM catalog_metadata_schema_heads WHERE item_id=NEW.item_id) THEN RAISE(ABORT,'metadata schema extension requires current predecessor') END;
END;
CREATE TRIGGER catalog_metadata_schema_update BEFORE UPDATE ON catalog_metadata_schemas BEGIN SELECT RAISE(ABORT,'registered metadata schemas are immutable'); END;
CREATE TRIGGER catalog_metadata_schema_delete BEFORE DELETE ON catalog_metadata_schemas BEGIN SELECT RAISE(ABORT,'registered metadata schemas are retained'); END;
CREATE TRIGGER catalog_metadata_schema_head_insert BEFORE INSERT ON catalog_metadata_schema_heads BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation WHERE p.item_id=NEW.item_id) THEN RAISE(ABORT,'metadata schema head requires typed owner') END;
END;
CREATE TRIGGER catalog_metadata_schema_head_update BEFORE UPDATE ON catalog_metadata_schema_heads BEGIN
 SELECT CASE WHEN NEW.item_id IS NOT OLD.item_id OR NOT EXISTS(SELECT 1 FROM catalog_metadata_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation WHERE p.item_id=NEW.item_id) THEN RAISE(ABORT,'metadata schema head requires typed owner') END;
 SELECT CASE WHEN OLD.schema_fingerprint IS NOT (SELECT prior_fingerprint FROM catalog_metadata_schemas WHERE item_id=NEW.item_id AND schema_fingerprint=NEW.schema_fingerprint) THEN RAISE(ABORT,'metadata schema head must advance registered predecessor') END;
END;
CREATE TRIGGER catalog_metadata_schema_head_delete BEFORE DELETE ON catalog_metadata_schema_heads BEGIN SELECT RAISE(ABORT,'metadata schema head is retained'); END;
CREATE TRIGGER catalog_metadata_subject_registry AFTER INSERT ON catalog_metadata_subjects BEGIN
 INSERT INTO catalog_metadata_schemas(item_id,schema_fingerprint,contract_version,policies_json,system_fields_json,prior_fingerprint,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us) SELECT NEW.item_id,NEW.schema_fingerprint,NEW.contract_version,NEW.policies_json,'[]',NULL,NEW.command_catalog_id,NEW.command_host_namespace,NEW.command_operation_id,NEW.admitted_at_us WHERE NEW.contract_version=1;
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_schemas s WHERE s.item_id=NEW.item_id AND s.schema_fingerprint=NEW.schema_fingerprint AND s.contract_version=NEW.contract_version AND s.policies_json=NEW.policies_json AND s.prior_fingerprint IS NULL) THEN RAISE(ABORT,'v2 subject requires exact initial registered schema') END;
 INSERT INTO catalog_metadata_schema_heads(item_id,schema_fingerprint) VALUES(NEW.item_id,NEW.schema_fingerprint);
END;

CREATE TRIGGER catalog_metadata_generation_schemas_insert AFTER INSERT ON catalog_metadata_schemas BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807 THEN RAISE(ABORT,'catalog generation exhausted') END;
 SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_metadata_schemas' AND entity_key=json_array(NEW.item_id,NEW.schema_fingerprint)),0)>=9223372036854775807 THEN RAISE(ABORT,'metadata entity invalidation revision exhausted') END;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES('catalog_metadata_schemas',json_array(NEW.item_id,NEW.schema_fingerprint),1,0) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
END;

CREATE TRIGGER catalog_metadata_generation_schema_heads_insert AFTER INSERT ON catalog_metadata_schema_heads BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807 THEN RAISE(ABORT,'catalog generation exhausted') END;
 SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_metadata_schema_heads' AND entity_key=json_array(NEW.item_id)),0)>=9223372036854775807 THEN RAISE(ABORT,'metadata entity invalidation revision exhausted') END;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES('catalog_metadata_schema_heads',json_array(NEW.item_id),1,0) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
END;

CREATE TRIGGER catalog_metadata_generation_schema_heads_update AFTER UPDATE ON catalog_metadata_schema_heads BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807 THEN RAISE(ABORT,'catalog generation exhausted') END;
 SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_metadata_schema_heads' AND entity_key=json_array(NEW.item_id)),0)>=9223372036854775807 THEN RAISE(ABORT,'metadata entity invalidation revision exhausted') END;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES('catalog_metadata_schema_heads',json_array(NEW.item_id),1,0) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
END;

-- Complete before/after source tag state is retained with the exact metadata
-- revision in the same compound command, including empty replacement sets.
CREATE TABLE catalog_metadata_tag_history (
 item_id TEXT NOT NULL,
 namespace TEXT NOT NULL,
 record_id TEXT NOT NULL,
 revision INTEGER NOT NULL,
 before_json TEXT NOT NULL CHECK(json_valid(before_json) AND json_type(before_json)='object' AND length(CAST(before_json AS BLOB))<=65536),
 after_json TEXT NOT NULL CHECK(json_valid(after_json) AND json_type(after_json)='object' AND length(CAST(after_json AS BLOB))<=65536),
 command_catalog_id TEXT NOT NULL,
 command_host_namespace TEXT NOT NULL,
 command_operation_id TEXT NOT NULL,
 admitted_at_us INTEGER NOT NULL CHECK(typeof(admitted_at_us)='integer' AND admitted_at_us>=0),
 PRIMARY KEY(item_id,namespace,record_id,revision),
 FOREIGN KEY(item_id,namespace,record_id,revision) REFERENCES catalog_metadata_history(item_id,namespace,record_id,revision) ON DELETE RESTRICT
);
CREATE TABLE catalog_metadata_tag_suppressions (
 item_id TEXT NOT NULL,
 tag_id TEXT NOT NULL,
 namespace TEXT NOT NULL CHECK(namespace='motion.local'),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=1),
 PRIMARY KEY(item_id,tag_id),
 FOREIGN KEY(item_id,tag_id) REFERENCES catalog_item_tag_suppressions(item_id,tag_id) ON DELETE RESTRICT
);
-- Only exact, still-current native import provenance establishes old local
-- ownership. An unrecognized later policy decision is not reassigned.
INSERT INTO catalog_metadata_tag_suppressions
 SELECT s.item_id,s.tag_id,'motion.local',s.revision
 FROM catalog_item_tag_suppressions s
 JOIN catalog_tag_suppression_revisions h ON h.subject_type='item' AND h.subject_id=s.item_id AND h.tag_id=s.tag_id AND h.revision=s.revision
 JOIN catalog_metadata_sources m ON m.item_id=s.item_id AND m.namespace='motion.local' AND m.record_id='document'
 WHERE json_valid(h.reason) AND json_extract(h.reason,'$.import_operation_id') IS NOT NULL
 AND json_extract(h.reason,'$.import_operation_id')=json_extract(m.evidence_json,'$.import_operation_id')
 AND json_extract(h.reason,'$.source_revision')=json_extract(m.evidence_json,'$.source_revision');
CREATE TRIGGER catalog_metadata_tag_history_insert BEFORE INSERT ON catalog_metadata_tag_history BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation WHERE p.item_id=NEW.item_id) THEN RAISE(ABORT,'metadata tag history requires typed owner') END;
END;
CREATE TRIGGER catalog_metadata_tag_history_update BEFORE UPDATE ON catalog_metadata_tag_history BEGIN SELECT RAISE(ABORT,'metadata tag history is immutable'); END;
CREATE TRIGGER catalog_metadata_tag_history_delete BEFORE DELETE ON catalog_metadata_tag_history BEGIN SELECT RAISE(ABORT,'metadata tag history is retained'); END;
CREATE TRIGGER catalog_metadata_tag_suppression_insert BEFORE INSERT ON catalog_metadata_tag_suppressions BEGIN
 SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation WHERE p.item_id=NEW.item_id) THEN RAISE(ABORT,'metadata tag ownership requires typed owner') END;
 SELECT CASE WHEN NEW.revision IS NOT (SELECT revision FROM catalog_item_tag_suppressions WHERE item_id=NEW.item_id AND tag_id=NEW.tag_id) THEN RAISE(ABORT,'metadata tag ownership must bind current suppression') END;
END;
CREATE TRIGGER catalog_metadata_tag_suppression_update BEFORE UPDATE ON catalog_metadata_tag_suppressions BEGIN
 SELECT CASE WHEN NEW.item_id IS NOT OLD.item_id OR NEW.tag_id IS NOT OLD.tag_id OR NEW.namespace IS NOT OLD.namespace OR NOT EXISTS(SELECT 1 FROM catalog_metadata_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation WHERE p.item_id=NEW.item_id) THEN RAISE(ABORT,'metadata tag ownership requires typed owner') END;
 SELECT CASE WHEN NEW.revision IS NOT (SELECT revision FROM catalog_item_tag_suppressions WHERE item_id=NEW.item_id AND tag_id=NEW.tag_id) THEN RAISE(ABORT,'metadata tag ownership must bind current suppression') END;
END;
CREATE TRIGGER catalog_metadata_tag_suppression_delete BEFORE DELETE ON catalog_metadata_tag_suppressions BEGIN SELECT RAISE(ABORT,'metadata tag ownership is retained'); END;

-- Retain later native35 search and native38 receipt obligations exactly.
CREATE TRIGGER catalog_metadata_history_receipt_obligation AFTER INSERT ON catalog_metadata_history BEGIN INSERT INTO catalog_metadata_receipt_obligations(command_catalog_id,command_host_namespace,command_operation_id) SELECT NEW.command_catalog_id,NEW.command_host_namespace,NEW.command_operation_id WHERE NOT EXISTS(SELECT 1 FROM catalog_metadata_receipt_obligations WHERE command_catalog_id=NEW.command_catalog_id AND command_host_namespace=NEW.command_host_namespace AND command_operation_id=NEW.command_operation_id); END;
CREATE TRIGGER catalog_metadata_history_uncommitted BEFORE INSERT ON catalog_metadata_history WHEN EXISTS (SELECT 1 FROM catalog_command_receipts r WHERE r.catalog_id=NEW.command_catalog_id AND r.host_namespace=NEW.command_host_namespace AND r.operation_id=NEW.command_operation_id) BEGIN SELECT RAISE(ABORT,'catalog_committed_history_sealed'); END;
CREATE TRIGGER catalog_search_input_catalog_metadata_projection_delete AFTER DELETE ON catalog_metadata_projection BEGIN INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id); UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty); END;
CREATE TRIGGER catalog_search_input_catalog_metadata_projection_insert AFTER INSERT ON catalog_metadata_projection BEGIN INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id); UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty); END;
CREATE TRIGGER catalog_search_input_catalog_metadata_projection_update AFTER UPDATE ON catalog_metadata_projection BEGIN INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id); INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id); UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty); END;
CREATE TRIGGER catalog_search_input_catalog_metadata_sources_delete AFTER DELETE ON catalog_metadata_sources BEGIN INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id); UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty); END;
CREATE TRIGGER catalog_search_input_catalog_metadata_sources_insert AFTER INSERT ON catalog_metadata_sources BEGIN INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id); UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty); END;
CREATE TRIGGER catalog_search_input_catalog_metadata_sources_update AFTER UPDATE ON catalog_metadata_sources BEGIN INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id); INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id); UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty); END;

-- Existing registered heads receive explicit initial native entity revisions.
INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) SELECT 'catalog_metadata_schemas',json_array(item_id,schema_fingerprint),1,0 FROM catalog_metadata_schemas;
INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) SELECT 'catalog_metadata_schema_heads',json_array(item_id),1,0 FROM catalog_metadata_schema_heads;
INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) SELECT 'catalog_metadata_tag_suppressions',json_array(item_id,tag_id),1,0 FROM catalog_metadata_tag_suppressions;
CREATE TRIGGER catalog_metadata_generation_tag_history_insert AFTER INSERT ON catalog_metadata_tag_history BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807 THEN RAISE(ABORT,'catalog generation exhausted') END;
 SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_metadata_tag_history' AND entity_key=json_array(NEW.item_id,NEW.namespace,NEW.record_id,NEW.revision)),0)>=9223372036854775807 THEN RAISE(ABORT,'metadata entity invalidation revision exhausted') END;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES('catalog_metadata_tag_history',json_array(NEW.item_id,NEW.namespace,NEW.record_id,NEW.revision),1,0) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
END;
CREATE TRIGGER catalog_metadata_generation_tag_suppressions_insert AFTER INSERT ON catalog_metadata_tag_suppressions BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807 THEN RAISE(ABORT,'catalog generation exhausted') END;
 SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_metadata_tag_suppressions' AND entity_key=json_array(NEW.item_id,NEW.tag_id)),0)>=9223372036854775807 THEN RAISE(ABORT,'metadata entity invalidation revision exhausted') END;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES('catalog_metadata_tag_suppressions',json_array(NEW.item_id,NEW.tag_id),1,0) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
END;
CREATE TRIGGER catalog_metadata_generation_tag_suppressions_update AFTER UPDATE ON catalog_metadata_tag_suppressions BEGIN
 SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807 THEN RAISE(ABORT,'catalog generation exhausted') END;
 SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_metadata_tag_suppressions' AND entity_key=json_array(NEW.item_id,NEW.tag_id)),0)>=9223372036854775807 THEN RAISE(ABORT,'metadata entity invalidation revision exhausted') END;
 UPDATE catalog_state SET generation=generation+1 WHERE id=1;
 INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted) VALUES('catalog_metadata_tag_suppressions',json_array(NEW.item_id,NEW.tag_id),1,0) ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
END;
