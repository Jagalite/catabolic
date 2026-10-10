-- Keep the established symlink tables and their values intact.
CREATE TABLE catalog_link_modes(
  catalog TEXT PRIMARY KEY REFERENCES catalogs(id),
  mode TEXT NOT NULL CHECK(mode IN ('symlink','hardlink')));
CREATE TABLE owned_hardlinks(
  profile TEXT NOT NULL REFERENCES profiles(id),
  catalog TEXT NOT NULL REFERENCES catalogs(id),
  path TEXT NOT NULL,
  target TEXT NOT NULL,
  PRIMARY KEY(profile,catalog,path));
CREATE TABLE retained_hardlinks(
  id TEXT PRIMARY KEY,
  profile TEXT NOT NULL REFERENCES profiles(id),
  catalog TEXT NOT NULL REFERENCES catalogs(id),
  path TEXT NOT NULL,
  original_path TEXT NOT NULL,
  target TEXT NOT NULL,
  retained_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(profile,catalog,path));
CREATE INDEX retained_hardlinks_by_catalog ON retained_hardlinks(profile,catalog,id);
