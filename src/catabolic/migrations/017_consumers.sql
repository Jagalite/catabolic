CREATE TABLE consumer_connections(
 profile TEXT NOT NULL REFERENCES profiles(id), id TEXT NOT NULL,
 application TEXT NOT NULL CHECK(application IN ('plex','jellyfin')),
 endpoint TEXT NOT NULL, credential_env TEXT NOT NULL, server_id TEXT NOT NULL,
 evidence TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1,
 PRIMARY KEY(profile,id));
CREATE TABLE consumer_bindings(
 profile TEXT NOT NULL, id TEXT NOT NULL, connection_id TEXT NOT NULL,
 catalog TEXT NOT NULL REFERENCES catalogs(id), subtree TEXT NOT NULL,
 remote_root TEXT NOT NULL, local_binding TEXT NOT NULL, library TEXT NOT NULL,
 group_id TEXT NOT NULL, origin TEXT NOT NULL,
 enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1)),
 automatic INTEGER NOT NULL DEFAULT 0 CHECK(automatic IN (0,1)),
 debounce INTEGER NOT NULL DEFAULT 0 CHECK(debounce BETWEEN 0 AND 3600),
 revision INTEGER NOT NULL DEFAULT 1,
 generation INTEGER NOT NULL DEFAULT 0, verified INTEGER NOT NULL DEFAULT 0,
 acknowledged INTEGER NOT NULL DEFAULT 0, due_at REAL NOT NULL DEFAULT 0,
 verified_at TEXT, error TEXT, indexing TEXT,
 PRIMARY KEY(profile,id),
 FOREIGN KEY(profile,connection_id) REFERENCES consumer_connections(profile,id));
CREATE INDEX consumer_binding_scope ON consumer_bindings(profile,catalog,enabled);
CREATE INDEX consumer_binding_group ON consumer_bindings(profile,group_id,enabled);
CREATE TABLE consumer_deliveries(
 profile TEXT NOT NULL REFERENCES profiles(id), id TEXT NOT NULL,
 state TEXT NOT NULL DEFAULT 'pending', attempts INTEGER NOT NULL DEFAULT 0,
 due_at REAL NOT NULL DEFAULT 0, lease_token TEXT, lease_until REAL,
 error TEXT, accepted_at TEXT, PRIMARY KEY(profile,id));
CREATE TABLE consumer_attempts(
 id INTEGER PRIMARY KEY, profile TEXT NOT NULL, group_id TEXT NOT NULL,
 token TEXT NOT NULL, snapshot TEXT NOT NULL, state TEXT NOT NULL,
 started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, finished_at TEXT, error TEXT,
 FOREIGN KEY(profile,group_id) REFERENCES consumer_deliveries(profile,id));
CREATE TABLE consumer_creations(
 profile TEXT NOT NULL, id TEXT NOT NULL, connection_id TEXT NOT NULL,
 connection_revision INTEGER NOT NULL, spec TEXT NOT NULL, before_ids TEXT NOT NULL,
 state TEXT NOT NULL, library TEXT, error TEXT,
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(profile,id),
 FOREIGN KEY(profile,connection_id) REFERENCES consumer_connections(profile,id));
CREATE TABLE notification_destinations(
 profile TEXT NOT NULL REFERENCES profiles(id), id TEXT NOT NULL,
 credential_env TEXT NOT NULL, subscriptions TEXT NOT NULL, tags TEXT NOT NULL,
 min_severity TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1, revision INTEGER NOT NULL DEFAULT 1,
 PRIMARY KEY(profile,id));
CREATE TABLE consumer_events(
 id TEXT PRIMARY KEY, profile TEXT NOT NULL REFERENCES profiles(id),
 event TEXT NOT NULL, severity TEXT NOT NULL, subject TEXT NOT NULL,
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE notification_deliveries(
 event_id TEXT NOT NULL REFERENCES consumer_events(id), profile TEXT NOT NULL,
 destination_id TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending',
 attempts INTEGER NOT NULL DEFAULT 0, due_at REAL NOT NULL DEFAULT 0,
 lease_token TEXT, lease_until REAL, error TEXT,
 PRIMARY KEY(event_id,profile,destination_id),
 FOREIGN KEY(profile,destination_id) REFERENCES notification_destinations(profile,id));
ALTER TABLE refresh_events ADD COLUMN lease_token TEXT;
ALTER TABLE refresh_events ADD COLUMN lease_until REAL;

CREATE TABLE publication_generations(
 profile TEXT NOT NULL REFERENCES profiles(id), catalog TEXT NOT NULL REFERENCES catalogs(id),
 local_binding TEXT NOT NULL, generation INTEGER NOT NULL DEFAULT 0,
 verified INTEGER NOT NULL DEFAULT 0, verified_at TEXT, last_checked REAL NOT NULL DEFAULT 0,
 PRIMARY KEY(profile,catalog));

CREATE INDEX publication_mapping_source ON mappings(file_id,catalog,path) WHERE active=1;
CREATE VIEW publication_source_paths AS
 SELECT m.catalog,m.path,m.file_id,b.profile,
 json_object('profile',b.profile,'kind',b.kind,'owner',b.owner,'root',b.root,'device',b.device,'inode',b.inode) AS local_binding
 FROM mappings m JOIN bindings b ON b.kind='output' AND b.owner=m.catalog
 WHERE m.active=1 AND (
 EXISTS(SELECT 1 FROM owned_links o WHERE o.profile=b.profile AND o.catalog=m.catalog AND o.path=m.path)
 OR EXISTS(SELECT 1 FROM owned_hardlinks o WHERE o.profile=b.profile AND o.catalog=m.catalog AND o.path=m.path));

-- A rescanned source can change the bytes visible through an unchanged link.
-- Observation versions are durable input; workers still verify local publication.
CREATE TRIGGER publication_source_version AFTER UPDATE OF size,mtime_ns,device,inode ON observations
 WHEN NEW.status='present' AND (OLD.size!=NEW.size OR OLD.mtime_ns!=NEW.mtime_ns OR OLD.device!=NEW.device OR OLD.inode!=NEW.inode)
 BEGIN
 INSERT INTO publication_generations(profile,catalog,local_binding,generation)
 SELECT DISTINCT profile,catalog,local_binding,1 FROM publication_source_paths
 WHERE profile=NEW.profile AND file_id=NEW.file_id
 ON CONFLICT(profile,catalog) DO UPDATE SET generation=publication_generations.generation+1,local_binding=excluded.local_binding;
 UPDATE consumer_bindings SET generation=generation+1,due_at=CAST(strftime('%s','now') AS REAL)+debounce,indexing=NULL
 WHERE profile=NEW.profile AND enabled=1 AND EXISTS(
 SELECT 1 FROM publication_source_paths p WHERE p.profile=NEW.profile AND p.file_id=NEW.file_id AND p.catalog=consumer_bindings.catalog
 AND (consumer_bindings.subtree='' OR p.path=consumer_bindings.subtree OR substr(p.path,1,length(consumer_bindings.subtree)+1)=consumer_bindings.subtree||'/'));
 END;
