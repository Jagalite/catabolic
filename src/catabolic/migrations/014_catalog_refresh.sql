CREATE TABLE catalog_refresh_settings(
  profile TEXT NOT NULL REFERENCES profiles(id),
  catalog TEXT NOT NULL REFERENCES catalogs(id),
  enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
  max_removals INTEGER NOT NULL CHECK(max_removals>=0),
  PRIMARY KEY(profile,catalog));
CREATE TABLE catalog_refresh_queue(
  profile TEXT NOT NULL,
  catalog TEXT NOT NULL,
  generation INTEGER NOT NULL DEFAULT 1,
  attempts INTEGER NOT NULL DEFAULT 0,
  next_attempt INTEGER NOT NULL DEFAULT 0,
  error TEXT,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY(profile,catalog),
  FOREIGN KEY(profile,catalog) REFERENCES catalog_refresh_settings(profile,catalog));
