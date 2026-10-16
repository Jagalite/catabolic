-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors
-- SPDX-License-Identifier: MIT
-- Additive derived search schema35. Existing source rows/generation are unchanged.
-- Input revision is local to the derived index, not catalog generation.
CREATE TABLE catalog_search_state(
 id INTEGER PRIMARY KEY CHECK(id=1),
 contract_version INTEGER NOT NULL CHECK(contract_version=1),
 tokenizer TEXT NOT NULL CHECK(tokenizer='unicode61 remove_diacritics 2'),
 extractor TEXT NOT NULL CHECK(extractor='native-search-values-v1'),
 ready INTEGER NOT NULL CHECK(ready IN (0,1)),
 input_revision INTEGER NOT NULL CHECK(typeof(input_revision)='integer' AND input_revision>=0),
 indexed_revision INTEGER NOT NULL CHECK(typeof(indexed_revision)='integer' AND indexed_revision>=0 AND indexed_revision<=input_revision),
 catalog_id TEXT, incarnation TEXT, rebuilt_generation INTEGER,
 CHECK(ready=0 OR (catalog_id IS NOT NULL AND incarnation IS NOT NULL AND rebuilt_generation IS NOT NULL))
);
INSERT INTO catalog_search_state VALUES(1,1,'unicode61 remove_diacritics 2','native-search-values-v1',0,0,0,NULL,NULL,NULL);
CREATE TABLE catalog_search_values(
 item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
 field_key TEXT NOT NULL,
 origin_key TEXT NOT NULL,
 rank_class TEXT NOT NULL CHECK(rank_class IN ('title','aliases','accepted_identities','effective_tags','description')),
 value_text TEXT NOT NULL,
 provenance_json TEXT NOT NULL CHECK(json_valid(provenance_json) AND json_type(provenance_json)='object'),
 required_subjects_json TEXT NOT NULL CHECK(json_valid(required_subjects_json) AND json_type(required_subjects_json)='array'),
 PRIMARY KEY(item_id,field_key,origin_key)
);
CREATE TABLE catalog_search_documents(
 rowid INTEGER PRIMARY KEY,
 item_id TEXT NOT NULL UNIQUE REFERENCES items(id) ON DELETE CASCADE,
 title TEXT NOT NULL, aliases TEXT NOT NULL, accepted_identities TEXT NOT NULL,
 effective_tags TEXT NOT NULL, description TEXT NOT NULL
);
CREATE VIRTUAL TABLE catalog_search_fts USING fts5(
 title, aliases, accepted_identities, effective_tags, description,
 content='catalog_search_documents',content_rowid='rowid',
 tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER catalog_search_document_insert AFTER INSERT ON catalog_search_documents BEGIN
 INSERT INTO catalog_search_fts(rowid,title,aliases,accepted_identities,effective_tags,description)
 VALUES(NEW.rowid,NEW.title,NEW.aliases,NEW.accepted_identities,NEW.effective_tags,NEW.description);
END;
CREATE TRIGGER catalog_search_document_delete AFTER DELETE ON catalog_search_documents BEGIN
 INSERT INTO catalog_search_fts(catalog_search_fts,rowid,title,aliases,accepted_identities,effective_tags,description)
 VALUES('delete',OLD.rowid,OLD.title,OLD.aliases,OLD.accepted_identities,OLD.effective_tags,OLD.description);
END;
CREATE TRIGGER catalog_search_document_update AFTER UPDATE ON catalog_search_documents BEGIN
 INSERT INTO catalog_search_fts(catalog_search_fts,rowid,title,aliases,accepted_identities,effective_tags,description)
 VALUES('delete',OLD.rowid,OLD.title,OLD.aliases,OLD.accepted_identities,OLD.effective_tags,OLD.description);
 INSERT INTO catalog_search_fts(rowid,title,aliases,accepted_identities,effective_tags,description)
 VALUES(NEW.rowid,NEW.title,NEW.aliases,NEW.accepted_identities,NEW.effective_tags,NEW.description);
END;
CREATE TABLE catalog_search_dirty(
 item_id TEXT PRIMARY KEY,
 barrier_id INTEGER NOT NULL DEFAULT 1 CHECK(barrier_id=1),
 FOREIGN KEY(barrier_id) REFERENCES catalog_commit_barrier(id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER catalog_search_dirty_limit BEFORE INSERT ON catalog_search_dirty
 WHEN NOT EXISTS(SELECT 1 FROM catalog_search_dirty WHERE item_id=NEW.item_id)
 AND (SELECT count(*) FROM catalog_search_dirty)>=100000
 BEGIN SELECT RAISE(ABORT,'catalog_search_dirty_budget'); END;
CREATE INDEX catalog_search_alias_reverse ON catalog_logical_retirements(survivor_item_id,item_id) WHERE kind='merge';
CREATE TRIGGER catalog_search_input_items_insert AFTER INSERT ON items BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_items_update AFTER UPDATE ON items WHEN NEW.id IS NOT OLD.id OR NEW.metadata IS NOT OLD.metadata BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_items_delete AFTER DELETE ON items BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_metadata_subjects_insert AFTER INSERT ON catalog_metadata_subjects BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_metadata_subjects_update AFTER UPDATE ON catalog_metadata_subjects BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_metadata_subjects_delete AFTER DELETE ON catalog_metadata_subjects BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_metadata_projection_insert AFTER INSERT ON catalog_metadata_projection BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_metadata_projection_update AFTER UPDATE ON catalog_metadata_projection BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_metadata_projection_delete AFTER DELETE ON catalog_metadata_projection BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_metadata_sources_insert AFTER INSERT ON catalog_metadata_sources BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_metadata_sources_update AFTER UPDATE ON catalog_metadata_sources BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_metadata_sources_delete AFTER DELETE ON catalog_metadata_sources BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_identity_pins_insert AFTER INSERT ON catalog_identity_pins BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_identity_pins_update AFTER UPDATE ON catalog_identity_pins BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_identity_pins_delete AFTER DELETE ON catalog_identity_pins BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_item_tags_insert AFTER INSERT ON item_tags BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_item_tags_update AFTER UPDATE ON item_tags BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_item_tags_delete AFTER DELETE ON item_tags BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_item_tag_suppressions_insert AFTER INSERT ON catalog_item_tag_suppressions BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_item_tag_suppressions_update AFTER UPDATE ON catalog_item_tag_suppressions BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_item_tag_suppressions_delete AFTER DELETE ON catalog_item_tag_suppressions BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_tag_suppression_revisions_insert AFTER INSERT ON catalog_tag_suppression_revisions WHEN NEW.subject_type='item' BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.subject_id AS item_id WHERE NEW.subject_type='item') AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_tag_names_insert BEFORE INSERT ON tag_names BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT item_id AS item_id FROM item_tags WHERE tag_id=NEW.tag_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_tag_names_update BEFORE UPDATE ON tag_names BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT item_id AS item_id FROM item_tags WHERE tag_id=OLD.tag_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT item_id AS item_id FROM item_tags WHERE tag_id=NEW.tag_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_tag_names_delete BEFORE DELETE ON tag_names BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT item_id AS item_id FROM item_tags WHERE tag_id=OLD.tag_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_tags_insert BEFORE INSERT ON tags BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT item_id AS item_id FROM item_tags WHERE tag_id=NEW.id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_tags_update BEFORE UPDATE ON tags BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT item_id AS item_id FROM item_tags WHERE tag_id=OLD.id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT item_id AS item_id FROM item_tags WHERE tag_id=NEW.id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_tags_delete BEFORE DELETE ON tags BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT item_id AS item_id FROM item_tags WHERE tag_id=OLD.id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_tag_redirects_insert AFTER INSERT ON catalog_tag_redirects BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT item_id AS item_id FROM item_tags WHERE tag_id IN (NEW.source_tag_id,NEW.target_tag_id)) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_tag_redirects_update AFTER UPDATE ON catalog_tag_redirects BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT item_id AS item_id FROM item_tags WHERE tag_id IN (OLD.source_tag_id,OLD.target_tag_id)) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT item_id AS item_id FROM item_tags WHERE tag_id IN (NEW.source_tag_id,NEW.target_tag_id)) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_logical_retirements_insert AFTER INSERT ON catalog_logical_retirements BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT id AS item_id FROM catalog_live_items) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT item_id AS item_id FROM catalog_search_documents) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.survivor_item_id AS item_id WHERE NEW.survivor_item_id IS NOT NULL) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_version_retirements_insert AFTER INSERT ON catalog_version_retirements BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT association_item_id AS item_id FROM catalog_versions WHERE id=NEW.version_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_versions_insert AFTER INSERT ON catalog_versions BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.association_item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_versions_update AFTER UPDATE ON catalog_versions BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.association_item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT NEW.association_item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
CREATE TRIGGER catalog_search_input_catalog_versions_delete AFTER DELETE ON catalog_versions BEGIN
 INSERT INTO catalog_search_dirty(item_id) SELECT DISTINCT candidate.item_id FROM (SELECT OLD.association_item_id AS item_id) AS candidate WHERE NOT EXISTS(SELECT 1 FROM catalog_search_dirty prior WHERE prior.item_id=candidate.item_id);
 UPDATE catalog_search_state SET input_revision=input_revision+1 WHERE id=1 AND EXISTS(SELECT 1 FROM catalog_search_dirty);
END;
