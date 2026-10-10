ALTER TABLE source_identity_policies ADD COLUMN settings TEXT;
ALTER TABLE source_identity_policies ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
CREATE TABLE source_policy_history(
 id INTEGER PRIMARY KEY, profile TEXT NOT NULL REFERENCES profiles(id),
 location TEXT NOT NULL REFERENCES locations(id), revision INTEGER NOT NULL,
 settings TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 UNIQUE(profile,location,revision));
INSERT INTO source_policy_history(profile,location,revision,settings)
 SELECT profile,location,revision,json_object('preset',identity_policy,'migration_baseline',1)
 FROM source_identity_policies;
