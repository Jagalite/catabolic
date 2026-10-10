CREATE TABLE observation_sources (
 profile TEXT NOT NULL, source TEXT NOT NULL, dirty_generation INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(profile,source)
);
CREATE TABLE observation_jobs (
 id TEXT PRIMARY KEY, profile TEXT NOT NULL, source TEXT NOT NULL,
 compatibility TEXT NOT NULL, exclusions TEXT NOT NULL,
 dirty_generation INTEGER NOT NULL, generation INTEGER NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('queued','running','complete','unavailable','failed','stale')),
 requested_at REAL NOT NULL, started_at REAL, completed_at REAL,
 lease_until REAL NOT NULL DEFAULT 0, worker_pid INTEGER, scan_id TEXT, report TEXT,
 UNIQUE(profile,source,generation)
);
CREATE INDEX observation_reuse ON observation_jobs(profile,source,compatibility,state,started_at);
CREATE TABLE watcher_definitions (
 id TEXT PRIMARY KEY, profile TEXT NOT NULL, name TEXT NOT NULL, revision INTEGER NOT NULL,
 definition TEXT NOT NULL, digest TEXT NOT NULL, created_at REAL NOT NULL,
 UNIQUE(profile,name,revision), UNIQUE(profile,name,digest)
);
CREATE TABLE watchers (
 profile TEXT NOT NULL, name TEXT NOT NULL, definition_id TEXT NOT NULL REFERENCES watcher_definitions(id),
 enabled INTEGER NOT NULL DEFAULT 0, pending_generation INTEGER NOT NULL DEFAULT 0,
 completed_generation INTEGER NOT NULL DEFAULT 0, requested_generation INTEGER NOT NULL DEFAULT 0, next_due REAL NOT NULL DEFAULT 0,
 retry_at REAL NOT NULL DEFAULT 0, active_run TEXT, lease_until REAL NOT NULL DEFAULT 0,
 baseline TEXT, pending_reaction TEXT, last_run TEXT, event_first REAL, event_last REAL,
 PRIMARY KEY(profile,name)
);
CREATE TABLE watcher_owners (
 profile TEXT NOT NULL, catalog TEXT NOT NULL, watcher TEXT NOT NULL,
 PRIMARY KEY(profile,catalog), FOREIGN KEY(profile,watcher) REFERENCES watchers(profile,name)
);
CREATE TABLE watcher_runs (
 id TEXT PRIMARY KEY, profile TEXT NOT NULL, watcher TEXT NOT NULL,
 definition_id TEXT NOT NULL, generation INTEGER NOT NULL, trigger TEXT NOT NULL,
 scheduled_at REAL NOT NULL, started_at REAL NOT NULL, completed_at REAL,
 state TEXT NOT NULL, worker_pid INTEGER, result TEXT, error TEXT
);
CREATE INDEX watcher_run_history ON watcher_runs(profile,watcher,started_at);
