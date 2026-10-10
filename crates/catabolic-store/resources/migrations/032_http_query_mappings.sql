CREATE TABLE http_operations(
 id TEXT PRIMARY KEY, profile TEXT NOT NULL REFERENCES profiles(id),
 name TEXT NOT NULL, revision INTEGER NOT NULL, definition TEXT NOT NULL,
 digest TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
 principal TEXT REFERENCES api_principals(id), token_id TEXT REFERENCES api_tokens(id),
 UNIQUE(profile,name,digest)
);
CREATE TABLE http_mapping_definitions(
 profile TEXT NOT NULL REFERENCES profiles(id), id TEXT NOT NULL,
 definition TEXT NOT NULL, digest TEXT NOT NULL,
 PRIMARY KEY(profile,id)
);
CREATE TABLE http_mapping_ledger(
 profile TEXT NOT NULL, mapping_id TEXT NOT NULL, key TEXT NOT NULL,
 request_digest TEXT NOT NULL, event_id TEXT NOT NULL REFERENCES consumer_events(id),
 PRIMARY KEY(profile,mapping_id,key),
 FOREIGN KEY(profile,mapping_id) REFERENCES http_mapping_definitions(profile,id)
);
CREATE TABLE http_mapping_deliveries(
 event_id TEXT PRIMARY KEY REFERENCES consumer_events(id),
 operation_id TEXT NOT NULL REFERENCES http_operations(id),
 mapping_id TEXT NOT NULL, key TEXT NOT NULL, plan_id TEXT NOT NULL,
 request TEXT NOT NULL, token_id TEXT REFERENCES api_tokens(id)
);
