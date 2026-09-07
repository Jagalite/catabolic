CREATE TABLE output_definitions(
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision>0),
  definition TEXT NOT NULL,
  digest TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(name,revision), UNIQUE(name,digest));
ALTER TABLE processing_recipes ADD COLUMN output_definition_id TEXT REFERENCES output_definitions(id);
CREATE TABLE media_outputs(
  id TEXT PRIMARY KEY,
  profile TEXT NOT NULL REFERENCES profiles(id),
  file_id TEXT NOT NULL REFERENCES files(id),
  source_file_id TEXT NOT NULL REFERENCES files(id),
  source_item_id TEXT NOT NULL REFERENCES items(id),
  item_id TEXT NOT NULL REFERENCES items(id),
  definition_id TEXT NOT NULL REFERENCES output_definitions(id),
  artifact_id TEXT UNIQUE REFERENCES processing_artifacts(id),
  origin TEXT NOT NULL CHECK(origin IN ('generated','registered')),
  metadata TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(profile,file_id),
  CHECK(file_id!=source_file_id),
  CHECK((origin='generated' AND artifact_id IS NOT NULL) OR
        (origin='registered' AND artifact_id IS NULL)));
CREATE INDEX outputs_from ON media_outputs(source_file_id,profile,id);
CREATE INDEX outputs_file ON media_outputs(file_id,source_file_id);
CREATE INDEX outputs_item ON media_outputs(item_id,profile,id);
CREATE INDEX outputs_page ON media_outputs(profile,id);
CREATE INDEX outputs_definition ON media_outputs(definition_id,profile,id);
