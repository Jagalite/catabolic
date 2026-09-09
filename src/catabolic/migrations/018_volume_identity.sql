CREATE TABLE binding_volumes(
 profile TEXT NOT NULL, kind TEXT NOT NULL, owner TEXT NOT NULL, volume_uuid TEXT NOT NULL,
 PRIMARY KEY(profile,kind,owner),
 FOREIGN KEY(profile,kind,owner) REFERENCES bindings(profile,kind,owner) ON DELETE CASCADE);
CREATE TABLE remount_guard(profile TEXT PRIMARY KEY REFERENCES profiles(id));
CREATE TABLE remount_repairs(
 id TEXT PRIMARY KEY, profile TEXT NOT NULL REFERENCES profiles(id), plan TEXT NOT NULL,
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
DROP TRIGGER publication_source_version;
CREATE TRIGGER publication_source_version AFTER UPDATE OF size,mtime_ns,device,inode ON observations
 WHEN NOT EXISTS(SELECT 1 FROM remount_guard WHERE profile=NEW.profile) AND NEW.status='present' AND (OLD.size!=NEW.size OR OLD.mtime_ns!=NEW.mtime_ns OR OLD.device!=NEW.device OR OLD.inode!=NEW.inode)
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
