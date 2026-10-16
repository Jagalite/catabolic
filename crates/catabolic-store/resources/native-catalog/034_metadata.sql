-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors
-- SPDX-License-Identifier: MIT
-- Additive metadata fragment for migration034. Execute in the native upgrade
-- transaction after schema033. Does not alter schema32/33 tables/triggers,
-- rewrite existing metadata, initialize subjects, or change the existing permit
-- constraint. Native schema manifests/checksums must be regenerated at integration.
CREATE TABLE catalog_metadata_permits (
    item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE RESTRICT,
    incarnation TEXT NOT NULL,
    barrier_id INTEGER NOT NULL DEFAULT 1 CHECK(barrier_id=1),
    FOREIGN KEY(barrier_id) REFERENCES catalog_commit_barrier(id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER catalog_metadata_permit_insert BEFORE INSERT ON catalog_metadata_permits BEGIN
    SELECT CASE WHEN NEW.incarnation IS NOT (SELECT incarnation FROM catalog_state WHERE id=1)
        THEN RAISE(ABORT,'metadata permit incarnation mismatch') END;
END;
CREATE TRIGGER catalog_metadata_permit_update BEFORE UPDATE ON catalog_metadata_permits BEGIN
    SELECT RAISE(ABORT,'metadata permits are immutable');
END;

CREATE TABLE catalog_metadata_subjects (
    item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE RESTRICT,
    contract_version INTEGER NOT NULL CHECK(contract_version=1),
    schema_fingerprint TEXT NOT NULL CHECK(length(schema_fingerprint)=64 AND schema_fingerprint NOT GLOB '*[^0-9a-f]*'),
    policies_json TEXT NOT NULL CHECK(json_valid(policies_json) AND json_type(policies_json)='array' AND json_array_length(policies_json) BETWEEN 1 AND 64 AND length(CAST(policies_json AS BLOB))<=32768),
    legacy_raw_json TEXT NOT NULL CHECK(json_valid(legacy_raw_json) AND json_type(legacy_raw_json)='object' AND length(CAST(legacy_raw_json AS BLOB))<=65536),
    command_catalog_id TEXT NOT NULL,
    command_host_namespace TEXT NOT NULL,
    command_operation_id TEXT NOT NULL,
    admitted_at_us INTEGER NOT NULL CHECK(typeof(admitted_at_us)='integer' AND admitted_at_us>=0),
    UNIQUE(item_id,schema_fingerprint)
);
CREATE TABLE catalog_metadata_history (
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
    FOREIGN KEY(item_id,schema_fingerprint) REFERENCES catalog_metadata_subjects(item_id,schema_fingerprint) ON DELETE RESTRICT,
    CHECK(source_kind!='legacy_import' OR (namespace='catabolic.legacy_import.v1' AND record_id='items.metadata')),
    CHECK(namespace!='catabolic.legacy_import.v1' OR source_kind='legacy_import')
);
CREATE TABLE catalog_metadata_sources (
    item_id TEXT NOT NULL,
    namespace TEXT NOT NULL CHECK(length(CAST(namespace AS BLOB)) BETWEEN 1 AND 256),
    record_id TEXT NOT NULL CHECK(length(CAST(record_id AS BLOB)) BETWEEN 1 AND 256),
    source_kind TEXT NOT NULL CHECK(source_kind IN ('local','provider','scan','legacy_import')),
    revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=1),
    schema_fingerprint TEXT NOT NULL,
    fields_json TEXT NOT NULL CHECK(json_valid(fields_json) AND json_type(fields_json)='object' AND length(CAST(fields_json AS BLOB))<=65536),
    evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json) AND json_type(evidence_json)='object' AND length(CAST(evidence_json AS BLOB))<=8192),
    PRIMARY KEY(item_id,namespace,record_id),
    FOREIGN KEY(item_id,schema_fingerprint) REFERENCES catalog_metadata_subjects(item_id,schema_fingerprint) ON DELETE RESTRICT,
    FOREIGN KEY(item_id,namespace,record_id,revision) REFERENCES catalog_metadata_history(item_id,namespace,record_id,revision) ON DELETE RESTRICT,
    CHECK(source_kind!='legacy_import' OR (namespace='catabolic.legacy_import.v1' AND record_id='items.metadata')),
    CHECK(namespace!='catabolic.legacy_import.v1' OR source_kind='legacy_import')
);
CREATE UNIQUE INDEX catalog_metadata_one_local ON catalog_metadata_sources(item_id) WHERE source_kind='local';
CREATE UNIQUE INDEX catalog_metadata_one_legacy ON catalog_metadata_sources(item_id) WHERE source_kind='legacy_import';
CREATE TABLE catalog_metadata_projection (
    item_id TEXT PRIMARY KEY,
    schema_fingerprint TEXT NOT NULL,
    resolution_json TEXT NOT NULL CHECK(json_valid(resolution_json) AND json_type(resolution_json)='object' AND length(CAST(resolution_json AS BLOB))<=8388608),
    effective_json TEXT NOT NULL CHECK(json_valid(effective_json) AND json_type(effective_json)='object' AND length(CAST(effective_json AS BLOB))<=2097152),
    FOREIGN KEY(item_id,schema_fingerprint) REFERENCES catalog_metadata_subjects(item_id,schema_fingerprint) ON DELETE RESTRICT
);

CREATE TRIGGER catalog_metadata_subject_insert BEFORE INSERT ON catalog_metadata_subjects BEGIN
    SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation WHERE p.item_id=NEW.item_id)
        THEN RAISE(ABORT,'metadata subject requires typed owner') END;
    SELECT CASE WHEN NEW.legacy_raw_json IS NOT (SELECT metadata FROM items WHERE id=NEW.item_id)
        THEN RAISE(ABORT,'metadata initialization must preserve exact current bytes') END;
END;
CREATE TRIGGER catalog_metadata_subject_update BEFORE UPDATE ON catalog_metadata_subjects BEGIN
    SELECT RAISE(ABORT,'metadata schemas and original import bytes are immutable');
END;
CREATE TRIGGER catalog_metadata_subject_delete BEFORE DELETE ON catalog_metadata_subjects BEGIN
    SELECT RAISE(ABORT,'metadata subjects require explicit retained retirement');
END;
CREATE TRIGGER catalog_metadata_history_insert BEFORE INSERT ON catalog_metadata_history BEGIN
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
    SELECT CASE WHEN NEW.item_id!=OLD.item_id OR NEW.namespace!=OLD.namespace OR NEW.record_id!=OLD.record_id OR NEW.source_kind!=OLD.source_kind OR NEW.schema_fingerprint!=OLD.schema_fingerprint OR OLD.revision=9223372036854775807 OR NEW.revision!=OLD.revision+1
        THEN RAISE(ABORT,'metadata source identity or revision invalid') END;
    SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_history h WHERE h.item_id=NEW.item_id AND h.namespace=NEW.namespace AND h.record_id=NEW.record_id AND h.revision=NEW.revision AND h.source_kind=NEW.source_kind AND h.schema_fingerprint=NEW.schema_fingerprint AND h.fields_json=NEW.fields_json AND h.evidence_json=NEW.evidence_json)
        THEN RAISE(ABORT,'metadata head must match retained history') END;
END;
CREATE TRIGGER catalog_metadata_source_delete BEFORE DELETE ON catalog_metadata_sources BEGIN
    SELECT RAISE(ABORT,'metadata source heads must remain revisioned, including empty heads');
END;
CREATE TRIGGER catalog_metadata_projection_insert BEFORE INSERT ON catalog_metadata_projection BEGIN
    SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation WHERE p.item_id=NEW.item_id)
        THEN RAISE(ABORT,'metadata projection requires typed owner') END;
END;
CREATE TRIGGER catalog_metadata_projection_update BEFORE UPDATE ON catalog_metadata_projection BEGIN
    SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation WHERE p.item_id=NEW.item_id)
        THEN RAISE(ABORT,'metadata projection requires typed owner') END;
    SELECT CASE WHEN NEW.item_id!=OLD.item_id OR NEW.schema_fingerprint!=OLD.schema_fingerprint
        THEN RAISE(ABORT,'metadata projection identity is immutable') END;
END;
CREATE TRIGGER catalog_metadata_projection_delete BEFORE DELETE ON catalog_metadata_projection BEGIN
    SELECT RAISE(ABORT,'metadata projection must be rebuilt in place');
END;
CREATE TRIGGER catalog_metadata_items_update BEFORE UPDATE OF metadata ON items
WHEN EXISTS(SELECT 1 FROM catalog_metadata_subjects WHERE item_id=OLD.id) AND NEW.metadata IS NOT OLD.metadata BEGIN
    SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM catalog_metadata_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation WHERE p.item_id=OLD.id)
        THEN RAISE(ABORT,'managed metadata changes require typed owner') END;
END;
CREATE TRIGGER catalog_metadata_items_delete BEFORE DELETE ON items
WHEN EXISTS(SELECT 1 FROM catalog_metadata_subjects WHERE item_id=OLD.id) BEGIN
    SELECT RAISE(ABORT,'managed metadata subject deletion requires retained retirement');
END;
CREATE TRIGGER catalog_metadata_items_rekey BEFORE UPDATE OF id ON items
WHEN NEW.id!=OLD.id AND EXISTS(SELECT 1 FROM catalog_metadata_subjects WHERE item_id=OLD.id) BEGIN
    SELECT RAISE(ABORT,'managed metadata subject identity is immutable');
END;

-- Every durable metadata table mutation advances the coarse generation. This
-- covers new/empty/removed source values, raw history, schema installation and
-- projection rebuilds. Existing schema033 item triggers remain in place. Permits
-- are ephemeral and deliberately do not advance generation. Overflow aborts the
-- statement (and the owner propagates the error for whole-command rollback).
CREATE TRIGGER catalog_metadata_generation_subjects_insert AFTER INSERT ON catalog_metadata_subjects BEGIN
    SELECT CASE WHEN (SELECT generation FROM catalog_state WHERE id=1)>=9223372036854775807
        THEN RAISE(ABORT,'catalog generation exhausted') END;
    SELECT CASE WHEN coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_metadata_subjects' AND entity_key=json_array(NEW.item_id)),0)>=9223372036854775807
        THEN RAISE(ABORT,'metadata entity invalidation revision exhausted') END;
    UPDATE catalog_state SET generation=generation+1 WHERE id=1;
    INSERT INTO catalog_entity_revisions(entity_kind,entity_key,revision,deleted)
        VALUES('catalog_metadata_subjects',json_array(NEW.item_id),1,0)
        ON CONFLICT(entity_kind,entity_key) DO UPDATE SET revision=revision+1,deleted=0;
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
