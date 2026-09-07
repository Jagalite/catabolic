CREATE TABLE processors(
  id TEXT NOT NULL,
  profile TEXT NOT NULL REFERENCES profiles(id),
  definition TEXT NOT NULL,
  PRIMARY KEY(profile,id));
CREATE TABLE processor_jobs(
  id TEXT PRIMARY KEY,
  profile TEXT NOT NULL REFERENCES profiles(id),
  processor TEXT NOT NULL,
  request TEXT NOT NULL,
  digest TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'queued' CHECK(state IN ('queued','leased','submitted','complete','failed','cancelled')),
  generation INTEGER NOT NULL DEFAULT 0,
  worker TEXT,
  lease_token TEXT,
  lease_until INTEGER,
  receipt_id TEXT REFERENCES external_receipts(id),
  error TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(profile,processor,digest),
  FOREIGN KEY(profile,processor) REFERENCES processors(profile,id));
CREATE INDEX processor_jobs_claim ON processor_jobs(profile,processor,state,lease_until);
CREATE TABLE processor_attempts(
  job_id TEXT NOT NULL REFERENCES processor_jobs(id),
  generation INTEGER NOT NULL,
  worker TEXT NOT NULL,
  state TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  finished_at INTEGER,
  PRIMARY KEY(job_id,generation));
