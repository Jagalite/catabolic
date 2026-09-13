CREATE TABLE source_observation_policies (
 profile TEXT NOT NULL, source TEXT NOT NULL, revision INTEGER NOT NULL,
 policy TEXT NOT NULL, PRIMARY KEY(profile,source)
);
CREATE TABLE source_observation_policy_history (
 profile TEXT NOT NULL, source TEXT NOT NULL, revision INTEGER NOT NULL,
 policy TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(profile,source,revision)
);
ALTER TABLE observation_jobs ADD COLUMN execution TEXT NOT NULL DEFAULT '{}';
ALTER TABLE observation_jobs ADD COLUMN parent_job TEXT REFERENCES observation_jobs(id);
ALTER TABLE observation_jobs ADD COLUMN claim_token TEXT;
CREATE TABLE observation_scopes (
 job_id TEXT NOT NULL REFERENCES observation_jobs(id), path TEXT NOT NULL,
 state TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '{}', updated_at REAL NOT NULL,
 PRIMARY KEY(job_id,path)
);
CREATE TABLE observation_batches (
 job_id TEXT NOT NULL REFERENCES observation_jobs(id), sequence INTEGER NOT NULL,
 observed INTEGER NOT NULL, bytes INTEGER NOT NULL, committed_at REAL NOT NULL,
 PRIMARY KEY(job_id,sequence)
);
CREATE TABLE observation_seen (
 job_id TEXT NOT NULL REFERENCES observation_jobs(id), file_id TEXT NOT NULL,
 PRIMARY KEY(job_id,file_id)
);
CREATE INDEX observation_continuations ON observation_jobs(parent_job);
