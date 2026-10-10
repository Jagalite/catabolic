CREATE TABLE item_files(
  id TEXT PRIMARY KEY,
  file_id TEXT NOT NULL REFERENCES files(id),
  item_id TEXT NOT NULL REFERENCES items(id),
  role TEXT NOT NULL,
  part INTEGER CHECK(part IS NULL OR part > 0),
  metadata TEXT NOT NULL DEFAULT '{}',
  origin TEXT NOT NULL CHECK(origin IN ('explicit','mapping','migration')),
  active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
  UNIQUE(file_id,item_id,role));
CREATE INDEX item_files_by_file ON item_files(file_id,active,item_id);
CREATE INDEX item_files_by_item ON item_files(item_id,active,role,part);
CREATE UNIQUE INDEX item_file_parts ON item_files(item_id,role,part)
  WHERE active=1 AND part IS NOT NULL;

-- Keep identification evidence from disabled decisions as well as active ones.
-- Existing mappings and all their IDs, destinations, and active flags stay intact.
INSERT INTO item_files(id,file_id,item_id,role,origin)
  SELECT 'legacy:' || min(id),file_id,item_id,'primary','migration'
  FROM mappings GROUP BY file_id,item_id;

CREATE TABLE item_relationships(
  id TEXT PRIMARY KEY,
  source_id TEXT NOT NULL REFERENCES items(id),
  target_id TEXT NOT NULL REFERENCES items(id),
  kind TEXT NOT NULL,
  position INTEGER CHECK(position IS NULL OR position > 0),
  metadata TEXT NOT NULL DEFAULT '{}',
  active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
  CHECK(source_id != target_id),
  UNIQUE(source_id,target_id,kind));
CREATE INDEX relationships_from ON item_relationships(source_id,active,kind);
CREATE INDEX relationships_to ON item_relationships(target_id,active,kind,position);
CREATE UNIQUE INDEX relationship_positions ON item_relationships(target_id,kind,position)
  WHERE active=1 AND position IS NOT NULL;
