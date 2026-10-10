CREATE TABLE rendition_policies(
  profile TEXT NOT NULL REFERENCES profiles(id),
  catalog TEXT NOT NULL REFERENCES catalogs(id),
  definition TEXT NOT NULL,
  digest TEXT NOT NULL,
  PRIMARY KEY(profile,catalog));
CREATE TABLE rendition_decisions(
  profile TEXT NOT NULL REFERENCES profiles(id),
  catalog TEXT NOT NULL REFERENCES catalogs(id),
  output_id TEXT NOT NULL REFERENCES media_outputs(id),
  excluded INTEGER NOT NULL CHECK(excluded IN (0,1)),
  reason TEXT NOT NULL,
  PRIMARY KEY(profile,catalog,output_id));
CREATE TABLE external_receipts(
  id TEXT PRIMARY KEY,
  profile TEXT NOT NULL REFERENCES profiles(id),
  producer TEXT NOT NULL,
  instance TEXT NOT NULL,
  job_id TEXT NOT NULL,
  attempt TEXT NOT NULL,
  digest TEXT NOT NULL,
  payload TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(profile,producer,instance,job_id,attempt));
CREATE TABLE receipt_outputs(
  receipt_id TEXT NOT NULL REFERENCES external_receipts(id),
  output_id TEXT NOT NULL UNIQUE REFERENCES media_outputs(id),
  snapshot TEXT NOT NULL,
  sha256 TEXT NOT NULL,
  PRIMARY KEY(receipt_id,output_id));
CREATE TABLE rule_evaluations(
  id TEXT PRIMARY KEY,
  rule_id TEXT NOT NULL REFERENCES processing_rules(id),
  matched_inputs INTEGER NOT NULL CHECK(matched_inputs>=0),
  counts TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE rule_requirements(
  id TEXT PRIMARY KEY,
  rule_id TEXT NOT NULL REFERENCES processing_rules(id),
  profile TEXT NOT NULL REFERENCES profiles(id),
  file_id TEXT NOT NULL REFERENCES files(id),
  item_id TEXT NOT NULL REFERENCES items(id),
  evaluation_id TEXT NOT NULL REFERENCES rule_evaluations(id),
  job_id TEXT REFERENCES processing_jobs(id),
  state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','waived')),
  UNIQUE(rule_id,file_id,item_id));
CREATE INDEX rule_requirements_item ON rule_requirements(item_id,profile);
