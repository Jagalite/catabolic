ALTER TABLE processing_recipes ADD COLUMN operation_kind TEXT NOT NULL DEFAULT 'render' CHECK(operation_kind IN ('render','analysis','external'));
CREATE TABLE processing_rules_next(
  id TEXT PRIMARY KEY,
  profile TEXT NOT NULL REFERENCES profiles(id),
  name TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision>0),
  recipe_id TEXT NOT NULL REFERENCES processing_recipes(id),
  location TEXT REFERENCES locations(id),
  selection TEXT NOT NULL,
  estimate_options TEXT NOT NULL,
  required INTEGER NOT NULL CHECK(required IN (0,1)),
  enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0,1)),
  digest TEXT NOT NULL,
  allow_derived INTEGER NOT NULL DEFAULT 0 CHECK(allow_derived IN (0,1)),
  query_id TEXT REFERENCES saved_queries(id),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(profile,name,revision), UNIQUE(profile,name,digest),
  FOREIGN KEY(profile,location) REFERENCES generated_locations(profile,location));
INSERT INTO processing_rules_next(id,profile,name,revision,recipe_id,location,selection,estimate_options,required,enabled,digest,created_at,query_id) SELECT id,profile,name,revision,recipe_id,location,selection,estimate_options,required,enabled,digest,created_at,query_id FROM processing_rules;
DROP TABLE processing_rules;
ALTER TABLE processing_rules_next RENAME TO processing_rules;
CREATE UNIQUE INDEX enabled_rule_name ON processing_rules(profile,name) WHERE enabled=1;
CREATE TABLE rule_jobs_next(
  rule_id TEXT NOT NULL REFERENCES processing_rules(id),
  job_id TEXT NOT NULL REFERENCES processing_jobs(id),
  file_id TEXT NOT NULL REFERENCES files(id),
  item_id TEXT REFERENCES items(id),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY(rule_id,job_id));
INSERT INTO rule_jobs_next SELECT * FROM rule_jobs;
DROP TABLE rule_jobs;
ALTER TABLE rule_jobs_next RENAME TO rule_jobs;
CREATE INDEX rule_jobs_job ON rule_jobs(job_id,rule_id);
CREATE TABLE rule_processor_jobs(
 rule_id TEXT NOT NULL REFERENCES processing_rules(id),
 job_id TEXT NOT NULL REFERENCES processor_jobs(id),
 file_id TEXT NOT NULL REFERENCES files(id),
 item_id TEXT NOT NULL REFERENCES items(id),
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(rule_id,job_id));
CREATE INDEX rule_processor_jobs_job ON rule_processor_jobs(job_id,rule_id);
ALTER TABLE rule_requirements ADD COLUMN processor_job_id TEXT REFERENCES processor_jobs(id);
ALTER TABLE processor_jobs ADD COLUMN operation_id TEXT REFERENCES processing_recipes(id);
