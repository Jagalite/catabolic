CREATE TABLE media_components (
 id TEXT PRIMARY KEY, kind TEXT NOT NULL,
 CHECK(kind IN ('video','audio','subtitle','attachment'))
);
CREATE TABLE component_occurrences (
 id TEXT PRIMARY KEY, component_id TEXT NOT NULL REFERENCES media_components(id),
 profile TEXT NOT NULL REFERENCES profiles(id), file_id TEXT NOT NULL REFERENCES files(id),
 association_id TEXT NOT NULL REFERENCES item_files(id), item_id TEXT NOT NULL REFERENCES items(id),
 revision TEXT NOT NULL, snapshot TEXT NOT NULL, association_evidence TEXT NOT NULL,
 item_evidence TEXT NOT NULL, storage TEXT NOT NULL CHECK(storage IN ('embedded','external')),
 locator TEXT NOT NULL, probe_job_id TEXT REFERENCES processing_jobs(id), stream_key INTEGER, probe_path TEXT NOT NULL DEFAULT '',
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX component_file ON component_occurrences(profile,file_id,revision);
CREATE INDEX component_item ON component_occurrences(profile,item_id);
CREATE TABLE component_assertions (
 id TEXT PRIMARY KEY REFERENCES proposals(id), occurrence_id TEXT NOT NULL REFERENCES component_occurrences(id),
 component_id TEXT NOT NULL REFERENCES media_components(id), payload TEXT NOT NULL,
 active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1))
);
CREATE TABLE component_job_inputs (
 job_id TEXT NOT NULL REFERENCES processing_jobs(id), ordinal INTEGER NOT NULL,
 occurrence_id TEXT NOT NULL REFERENCES component_occurrences(id), evidence TEXT NOT NULL,
 PRIMARY KEY(job_id,ordinal)
);
CREATE TABLE component_output_lineage (
 artifact_id TEXT NOT NULL REFERENCES processing_artifacts(id), ordinal INTEGER NOT NULL,
 occurrence_id TEXT NOT NULL REFERENCES component_occurrences(id), output_stream_index INTEGER NOT NULL,
 relationship TEXT NOT NULL, component_id TEXT NOT NULL REFERENCES media_components(id), PRIMARY KEY(artifact_id,ordinal)
);
CREATE TRIGGER component_occurrence_insert AFTER INSERT ON component_occurrences BEGIN
 UPDATE fallback_epoch SET generation=generation+1 WHERE id=1;
END;
CREATE TRIGGER component_occurrence_update AFTER UPDATE ON component_occurrences BEGIN
 UPDATE fallback_epoch SET generation=generation+1 WHERE id=1;
END;
CREATE TRIGGER component_assertion_insert AFTER INSERT ON component_assertions BEGIN
 UPDATE fallback_epoch SET generation=generation+1 WHERE id=1;
END;
CREATE TRIGGER component_assertion_update AFTER UPDATE ON component_assertions BEGIN
 UPDATE fallback_epoch SET generation=generation+1 WHERE id=1;
END;
CREATE TRIGGER component_lineage_insert AFTER INSERT ON component_output_lineage BEGIN
 UPDATE fallback_epoch SET generation=generation+1 WHERE id=1;
END;
