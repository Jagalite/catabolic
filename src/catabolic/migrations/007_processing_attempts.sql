CREATE TABLE processing_attempts(
  job_id TEXT NOT NULL REFERENCES processing_jobs(id),
  attempt INTEGER NOT NULL CHECK(attempt > 0),
  state TEXT NOT NULL, error TEXT,
  retryable INTEGER NOT NULL CHECK(retryable IN (0,1)),
  retry_after REAL NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY(job_id,attempt));
CREATE INDEX processing_attempts_due ON processing_attempts(retryable,retry_after,job_id);
CREATE TABLE processing_retry_queue(
  job_id TEXT PRIMARY KEY REFERENCES processing_jobs(id),
  profile TEXT NOT NULL REFERENCES profiles(id),
  attempt INTEGER NOT NULL, retry_after REAL NOT NULL);
CREATE INDEX processing_retry_due ON processing_retry_queue(profile,retry_after,job_id);
