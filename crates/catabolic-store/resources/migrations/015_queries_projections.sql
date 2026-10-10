CREATE TABLE saved_queries(
  id TEXT PRIMARY KEY,
  profile TEXT NOT NULL REFERENCES profiles(id),
  name TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision>0),
  definition TEXT NOT NULL,
  digest TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(profile,name,revision), UNIQUE(profile,name,digest));
CREATE TABLE query_dependencies(
  query_id TEXT NOT NULL REFERENCES saved_queries(id),
  dependency_id TEXT NOT NULL REFERENCES saved_queries(id),
  PRIMARY KEY(query_id,dependency_id), CHECK(query_id!=dependency_id));
ALTER TABLE processing_rules ADD COLUMN query_id TEXT REFERENCES saved_queries(id);
INSERT INTO saved_queries(id,profile,name,revision,definition,digest)
  SELECT 'legacy-rule-'||id,profile,'legacy-rule-'||id,1,
  json_object('version',1,'mode','selection','selection',json(selection),
    'max_ids',coalesce(json_extract(selection,'$.max_ids'),10000),
    'timeout_ms',coalesce(json_extract(selection,'$.timeout_ms'),5000)),digest
  FROM processing_rules;
UPDATE processing_rules SET query_id='legacy-rule-'||id;
INSERT INTO saved_queries(id,profile,name,revision,definition,digest)
  SELECT 'legacy-layout-'||substr(key,8),
  coalesce(json_extract(value,'$.selection.profile'),'default'),
       'legacy-layout-'||printf('%016x',rowid),1,
  json_object('version',1,'mode','selection','selection',json_extract(value,'$.selection'),
    'max_ids',coalesce(json_extract(value,'$.selection.max_ids'),10000),
    'timeout_ms',coalesce(json_extract(value,'$.selection.timeout_ms'),5000)),
  'legacy:'||key FROM meta WHERE key LIKE 'layout:%' AND json_type(value,'$.selection')='object';
UPDATE saved_queries SET definition=catabolic_query_v1(definition,profile);
UPDATE saved_queries SET digest=catabolic_sha256_v1(definition);
CREATE TABLE projection_bindings(
  profile TEXT NOT NULL REFERENCES profiles(id),
  catalog TEXT NOT NULL REFERENCES catalogs(id),
  query_id TEXT NOT NULL REFERENCES saved_queries(id),
  layout TEXT NOT NULL,
  PRIMARY KEY(profile,catalog));
