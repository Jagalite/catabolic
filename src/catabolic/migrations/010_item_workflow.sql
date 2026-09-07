ALTER TABLE processing_artifacts ADD COLUMN publication_snapshot TEXT;
CREATE TABLE item_workflow(
  item_id TEXT PRIMARY KEY REFERENCES items(id),
  status TEXT NOT NULL CHECK(status IN ('pending','in_progress','complete','deferred','ignored')),
  revision INTEGER NOT NULL CHECK(revision>0),
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_by TEXT NOT NULL);
CREATE INDEX item_workflow_status ON item_workflow(status,item_id);
CREATE TABLE item_requirements(
  id TEXT PRIMARY KEY,
  item_id TEXT NOT NULL REFERENCES items(id),
  profile TEXT NOT NULL REFERENCES profiles(id),
  kind TEXT NOT NULL CHECK(kind IN ('review','file','job','artifact','proposal')),
  label TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('pending','complete','waived')),
  file_id TEXT REFERENCES files(id),
  job_id TEXT REFERENCES processing_jobs(id),
  artifact_id TEXT REFERENCES processing_artifacts(id),
  proposal_id TEXT REFERENCES proposals(id),
  CHECK((kind='review' AND file_id IS NULL AND job_id IS NULL AND artifact_id IS NULL AND proposal_id IS NULL) OR
        (kind='file' AND file_id IS NOT NULL AND job_id IS NULL AND artifact_id IS NULL AND proposal_id IS NULL) OR
        (kind='job' AND file_id IS NULL AND job_id IS NOT NULL AND artifact_id IS NULL AND proposal_id IS NULL) OR
        (kind='artifact' AND file_id IS NULL AND job_id IS NULL AND artifact_id IS NOT NULL AND proposal_id IS NULL) OR
        (kind='proposal' AND file_id IS NULL AND job_id IS NULL AND artifact_id IS NULL AND proposal_id IS NOT NULL)),
  CHECK(kind='review' OR state!='complete'));
CREATE INDEX item_requirements_item ON item_requirements(item_id,id);
CREATE TABLE item_worklog(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  item_id TEXT NOT NULL REFERENCES items(id),
  profile TEXT NOT NULL REFERENCES profiles(id),
  revision INTEGER NOT NULL CHECK(revision>0),
  kind TEXT NOT NULL CHECK(kind IN ('note','decision','progress','status','requirement')),
  body TEXT NOT NULL,
  actor TEXT NOT NULL,
  data TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(item_id,revision));
CREATE INDEX item_worklog_page ON item_worklog(item_id,id);
