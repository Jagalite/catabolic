CREATE TABLE processing_rules(
  id TEXT PRIMARY KEY,
  profile TEXT NOT NULL REFERENCES profiles(id),
  name TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision>0),
  recipe_id TEXT NOT NULL REFERENCES processing_recipes(id),
  location TEXT NOT NULL REFERENCES locations(id),
  selection TEXT NOT NULL,
  estimate_options TEXT NOT NULL,
  required INTEGER NOT NULL CHECK(required IN (0,1)),
  enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0,1)),
  digest TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(profile,name,revision), UNIQUE(profile,name,digest),
  FOREIGN KEY(profile,location) REFERENCES generated_locations(profile,location));
CREATE UNIQUE INDEX enabled_rule_name ON processing_rules(profile,name) WHERE enabled=1;
CREATE TABLE rule_jobs(
  rule_id TEXT NOT NULL REFERENCES processing_rules(id),
  job_id TEXT NOT NULL REFERENCES processing_jobs(id),
  file_id TEXT NOT NULL REFERENCES files(id),
  item_id TEXT NOT NULL REFERENCES items(id),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY(rule_id,job_id));
CREATE INDEX rule_jobs_job ON rule_jobs(job_id,rule_id);
