CREATE TABLE execution_claims(
 job_id TEXT PRIMARY KEY REFERENCES processing_jobs(id),
 generation INTEGER NOT NULL, token TEXT NOT NULL, worker_pid INTEGER NOT NULL,
 attempt INTEGER NOT NULL, lease_until REAL NOT NULL,
 snapshot TEXT NOT NULL, options TEXT NOT NULL
);
CREATE TABLE api_events(
 id INTEGER PRIMARY KEY AUTOINCREMENT, profile TEXT NOT NULL,
 resource_type TEXT NOT NULL, resource_id TEXT NOT NULL,
 state TEXT NOT NULL, created_at REAL NOT NULL DEFAULT (unixepoch())
);
CREATE TRIGGER api_job_events AFTER UPDATE OF state ON processing_jobs
WHEN NEW.state != OLD.state BEGIN
 INSERT INTO api_events(profile,resource_type,resource_id,state)
 VALUES(NEW.profile,'job',NEW.id,NEW.state);
END;
