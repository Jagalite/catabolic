CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY,
    filename TEXT NOT NULL UNIQUE,
    checksum TEXT NOT NULL CHECK(length(checksum) = 64),
    origin TEXT NOT NULL CHECK(origin IN ('initialized', 'baseline', 'migrated')),
    applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
