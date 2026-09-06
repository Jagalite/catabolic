CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE profiles(id TEXT PRIMARY KEY);
CREATE TABLE locations(id TEXT PRIMARY KEY);
CREATE TABLE catalogs(id TEXT PRIMARY KEY);
CREATE TABLE bindings(
  profile TEXT NOT NULL REFERENCES profiles(id), kind TEXT NOT NULL CHECK(kind IN ('source','output')),
  owner TEXT NOT NULL, root TEXT NOT NULL, device INTEGER NOT NULL, inode INTEGER NOT NULL,
  PRIMARY KEY(profile,kind,owner), UNIQUE(profile,root));
CREATE TABLE scans(
  id TEXT PRIMARY KEY, profile TEXT NOT NULL, location TEXT NOT NULL REFERENCES locations(id),
  complete INTEGER NOT NULL, observed INTEGER NOT NULL, errors TEXT NOT NULL,
  finished_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE files(
  id TEXT PRIMARY KEY, location TEXT NOT NULL REFERENCES locations(id), path TEXT NOT NULL,
  UNIQUE(location,path));
CREATE TABLE observations(
  profile TEXT NOT NULL REFERENCES profiles(id), file_id TEXT NOT NULL REFERENCES files(id),
  size INTEGER NOT NULL, mtime_ns INTEGER NOT NULL, device INTEGER NOT NULL, inode INTEGER NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('present','missing')), scan_id TEXT NOT NULL REFERENCES scans(id),
  PRIMARY KEY(profile,file_id));
CREATE TABLE items(id TEXT PRIMARY KEY, kind TEXT NOT NULL, metadata TEXT NOT NULL);
CREATE TABLE identities(
  namespace TEXT NOT NULL, value TEXT NOT NULL, item_id TEXT NOT NULL REFERENCES items(id),
  PRIMARY KEY(namespace,value));
CREATE TABLE mappings(
  id TEXT PRIMARY KEY, catalog TEXT NOT NULL REFERENCES catalogs(id),
  file_id TEXT NOT NULL REFERENCES files(id), item_id TEXT NOT NULL REFERENCES items(id),
  path TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 1);
CREATE UNIQUE INDEX active_destinations ON mappings(catalog,path) WHERE active=1;
CREATE TABLE owned_links(
  profile TEXT NOT NULL, catalog TEXT NOT NULL, path TEXT NOT NULL, target TEXT NOT NULL,
  PRIMARY KEY(profile,catalog,path));
CREATE TABLE journal(
  id TEXT PRIMARY KEY, profile TEXT NOT NULL, catalog TEXT NOT NULL,
  path TEXT NOT NULL, kind TEXT NOT NULL, target TEXT, previous TEXT,
  UNIQUE(profile,catalog,path));
