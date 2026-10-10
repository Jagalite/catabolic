CREATE TABLE api_playback_jobs(
 id TEXT PRIMARY KEY, profile TEXT NOT NULL REFERENCES profiles(id), cache_key TEXT NOT NULL,
 body TEXT NOT NULL, snapshot TEXT NOT NULL, recipe TEXT NOT NULL, tools TEXT NOT NULL,
 destination TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'queued'
 CHECK(state IN ('queued','running','ready','failed','cancelled','stale')),
 directory_device INTEGER, directory_inode INTEGER,
 reserved_bytes INTEGER NOT NULL, reservation_device INTEGER NOT NULL,
 segment_count INTEGER NOT NULL DEFAULT 0, available_seconds REAL NOT NULL DEFAULT 0,
 error TEXT, created_at REAL NOT NULL, updated_at REAL NOT NULL
);
CREATE UNIQUE INDEX api_playback_cache ON api_playback_jobs(profile,cache_key)
 WHERE state IN ('queued','running','ready');
CREATE TABLE api_playback_sessions(
 id TEXT PRIMARY KEY, profile TEXT NOT NULL REFERENCES profiles(id),
 principal TEXT NOT NULL REFERENCES api_principals(id), token_id TEXT NOT NULL REFERENCES api_tokens(id),
 idempotency_key TEXT NOT NULL, body TEXT NOT NULL,
 job_id TEXT REFERENCES api_playback_jobs(id), result TEXT,
 cancelled INTEGER NOT NULL DEFAULT 0, created_at REAL NOT NULL, expires REAL NOT NULL,
 UNIQUE(principal,idempotency_key)
);
CREATE INDEX api_playback_pending ON api_playback_jobs(profile,state,created_at);
CREATE INDEX api_playback_session_jobs ON api_playback_sessions(job_id,cancelled,expires);
