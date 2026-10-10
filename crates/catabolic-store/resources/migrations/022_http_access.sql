CREATE TABLE api_principals(id TEXT PRIMARY KEY, profile TEXT NOT NULL REFERENCES profiles(id), enabled INTEGER NOT NULL DEFAULT 1);
CREATE TABLE api_grants(id TEXT PRIMARY KEY, principal TEXT NOT NULL REFERENCES api_principals(id), definition TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1, enabled INTEGER NOT NULL DEFAULT 1);
CREATE TABLE api_tokens(id TEXT PRIMARY KEY, principal TEXT NOT NULL REFERENCES api_principals(id), digest TEXT NOT NULL, grants TEXT NOT NULL, expires REAL NOT NULL, revoked INTEGER NOT NULL DEFAULT 0);
CREATE TABLE api_sources(profile TEXT NOT NULL, location TEXT NOT NULL REFERENCES locations(id), PRIMARY KEY(profile,location));
CREATE TABLE api_tickets(id TEXT PRIMARY KEY, digest TEXT NOT NULL, token_id TEXT NOT NULL REFERENCES api_tokens(id), file_id TEXT NOT NULL REFERENCES files(id), revision TEXT NOT NULL, expires REAL NOT NULL, revoked INTEGER NOT NULL DEFAULT 0, remaining_bytes INTEGER NOT NULL);
CREATE TABLE api_operations(id TEXT PRIMARY KEY, profile TEXT NOT NULL, recipe_id TEXT NOT NULL REFERENCES processing_recipes(id), location TEXT NOT NULL REFERENCES locations(id), enabled INTEGER NOT NULL DEFAULT 1);
CREATE TABLE api_requests(id TEXT PRIMARY KEY, profile TEXT NOT NULL, principal TEXT NOT NULL REFERENCES api_principals(id), token_id TEXT NOT NULL REFERENCES api_tokens(id), idempotency_key TEXT NOT NULL, body TEXT NOT NULL, job_id TEXT NOT NULL REFERENCES processing_jobs(id), state TEXT NOT NULL DEFAULT 'queued', reserved_bytes INTEGER NOT NULL, reservation_device INTEGER NOT NULL, created_at REAL NOT NULL DEFAULT (unixepoch()), UNIQUE(principal,idempotency_key));
CREATE TABLE api_audit(id INTEGER PRIMARY KEY AUTOINCREMENT, principal TEXT, action TEXT NOT NULL, resource TEXT, created_at REAL NOT NULL DEFAULT (unixepoch()));
CREATE TABLE api_snapshots(id TEXT PRIMARY KEY, principal TEXT NOT NULL, scope TEXT NOT NULL, ids TEXT NOT NULL, expires REAL NOT NULL);
CREATE TRIGGER api_request_events AFTER UPDATE OF state ON api_requests WHEN NEW.state!=OLD.state BEGIN
 INSERT INTO api_events(profile,resource_type,resource_id,state) VALUES(NEW.profile,'request',NEW.id,NEW.state);
END;
CREATE TRIGGER api_request_created AFTER INSERT ON api_requests BEGIN
 INSERT INTO api_events(profile,resource_type,resource_id,state) VALUES(NEW.profile,'request',NEW.id,NEW.state);
END;
CREATE TRIGGER api_job_request_states AFTER UPDATE OF state ON processing_jobs BEGIN
 UPDATE api_requests SET state=CASE NEW.state WHEN 'complete' THEN 'ready' WHEN 'changed' THEN 'stale' WHEN 'timeout' THEN 'failed' ELSE NEW.state END WHERE job_id=NEW.id AND state NOT IN ('cancelled');
 UPDATE api_requests SET reserved_bytes=0 WHERE job_id=NEW.id AND NEW.state IN ('complete','failed','timeout','changed','cancelled');
END;
CREATE TABLE api_worker_status(profile TEXT PRIMARY KEY, worker_pid INTEGER NOT NULL, capabilities TEXT NOT NULL, updated_at REAL NOT NULL);
CREATE TABLE api_reservations(job_id TEXT PRIMARY KEY REFERENCES processing_jobs(id), device INTEGER NOT NULL, bytes INTEGER NOT NULL);
CREATE TRIGGER api_release_reservations AFTER UPDATE OF state ON processing_jobs WHEN NEW.state IN ('complete','failed','timeout','changed','cancelled') BEGIN
 DELETE FROM api_reservations WHERE job_id=NEW.id;
END;

CREATE TABLE api_private_files(path TEXT PRIMARY KEY, device INTEGER NOT NULL, inode INTEGER NOT NULL);

CREATE INDEX api_requests_work ON api_requests(profile,state,created_at,id);
CREATE INDEX api_requests_job ON api_requests(job_id,principal);
CREATE INDEX api_snapshots_expiry ON api_snapshots(expires);
CREATE INDEX api_tickets_expiry ON api_tickets(expires);
CREATE TRIGGER api_artifact_validating AFTER UPDATE OF state ON processing_artifacts WHEN NEW.state='validating' BEGIN
 UPDATE api_requests SET state='validating' WHERE job_id=NEW.job_id AND state='running';
END;
