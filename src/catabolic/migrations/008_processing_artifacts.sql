CREATE TABLE processing_recipes(
  id TEXT PRIMARY KEY, name TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0),
  preset TEXT NOT NULL, definition TEXT NOT NULL, digest TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(name,revision), UNIQUE(name,digest));
CREATE TABLE generated_locations(
  profile TEXT NOT NULL REFERENCES profiles(id),
  location TEXT NOT NULL REFERENCES locations(id),
  PRIMARY KEY(profile,location));
ALTER TABLE processing_jobs ADD COLUMN recipe_id TEXT REFERENCES processing_recipes(id);
ALTER TABLE processing_attempts ADD COLUMN started_at TEXT;
ALTER TABLE processing_attempts ADD COLUMN finished_at TEXT;
ALTER TABLE processing_attempts ADD COLUMN result TEXT;
CREATE TABLE processing_artifacts(
  id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL REFERENCES processing_jobs(id),
  attempt INTEGER NOT NULL CHECK(attempt>0),
  profile TEXT NOT NULL REFERENCES profiles(id),
  location TEXT NOT NULL REFERENCES locations(id),
  path TEXT NOT NULL, temporary_path TEXT NOT NULL, binding TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('planned','writing','validating','publishing','ready','failed','interrupted')),
  file_id TEXT UNIQUE REFERENCES files(id), item_id TEXT NOT NULL REFERENCES items(id),
  role TEXT NOT NULL, size INTEGER, sha256 TEXT, validation TEXT,
  device INTEGER, inode INTEGER, error TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY(job_id,attempt) REFERENCES processing_attempts(job_id,attempt),
  FOREIGN KEY(profile,location) REFERENCES generated_locations(profile,location),
  UNIQUE(job_id,attempt), UNIQUE(location,path), UNIQUE(location,temporary_path),
  CHECK(state!='ready' OR (file_id IS NOT NULL AND sha256 IS NOT NULL AND size>0)));
CREATE INDEX artifacts_page ON processing_artifacts(profile,state,id);
CREATE INDEX artifacts_job ON processing_artifacts(job_id,attempt);
CREATE INDEX generated_files ON processing_artifacts(location,file_id,state);
