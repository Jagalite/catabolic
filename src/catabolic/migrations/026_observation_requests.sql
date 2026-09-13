CREATE TABLE observation_requests (
 id TEXT PRIMARY KEY, profile TEXT NOT NULL, source TEXT NOT NULL,
 requester TEXT, guarantees TEXT NOT NULL, job_id TEXT NOT NULL REFERENCES observation_jobs(id),
 state TEXT NOT NULL DEFAULT 'active' CHECK(state IN ('active','cancelled')),
 created_at REAL NOT NULL,
 UNIQUE(profile,source,requester)
);
CREATE INDEX observation_request_job ON observation_requests(job_id);

ALTER TABLE observation_jobs ADD COLUMN guarded INTEGER NOT NULL DEFAULT 0;
