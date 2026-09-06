-- Tags are catalog-wide; item and file assignments have real foreign keys.
CREATE TABLE tags(
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  description TEXT NOT NULL DEFAULT '');
-- Canonical names and aliases share one collision domain.
CREATE TABLE tag_names(
  name TEXT PRIMARY KEY,
  tag_id TEXT NOT NULL REFERENCES tags(id));
CREATE INDEX tag_names_by_tag ON tag_names(tag_id,name);
CREATE TABLE tag_parents(
  child_id TEXT NOT NULL REFERENCES tags(id),
  parent_id TEXT NOT NULL REFERENCES tags(id),
  CHECK(child_id != parent_id),
  PRIMARY KEY(child_id,parent_id));
CREATE INDEX tag_children ON tag_parents(parent_id,child_id);
CREATE TABLE item_tags(
  id TEXT PRIMARY KEY,
  item_id TEXT NOT NULL REFERENCES items(id),
  tag_id TEXT NOT NULL REFERENCES tags(id),
  source TEXT NOT NULL,
  confidence REAL CHECK(confidence IS NULL OR (confidence >= 0 AND confidence <= 1)),
  note TEXT NOT NULL DEFAULT '',
  active INTEGER NOT NULL CHECK(active IN (0,1)),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(item_id,tag_id,source));
CREATE INDEX item_tags_lookup ON item_tags(tag_id,active,item_id);
CREATE TABLE file_tags(
  id TEXT PRIMARY KEY,
  file_id TEXT NOT NULL REFERENCES files(id),
  tag_id TEXT NOT NULL REFERENCES tags(id),
  source TEXT NOT NULL,
  confidence REAL CHECK(confidence IS NULL OR (confidence >= 0 AND confidence <= 1)),
  note TEXT NOT NULL DEFAULT '',
  active INTEGER NOT NULL CHECK(active IN (0,1)),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(file_id,tag_id,source));
CREATE INDEX file_tags_lookup ON file_tags(tag_id,active,file_id);
