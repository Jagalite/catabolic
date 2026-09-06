CREATE TABLE proposals(
  id TEXT PRIMARY KEY, profile TEXT NOT NULL REFERENCES profiles(id),
  file_id TEXT NOT NULL REFERENCES files(id), kind TEXT NOT NULL,
  payload TEXT NOT NULL, evidence TEXT NOT NULL, source TEXT NOT NULL,
  snapshot TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending'
    CHECK(state IN ('pending','accepted','rejected')),
  result TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE INDEX proposals_page ON proposals(profile,state,id);
CREATE TABLE decision_events(
  id INTEGER PRIMARY KEY, proposal_id TEXT NOT NULL REFERENCES proposals(id),
  action TEXT NOT NULL, actor TEXT NOT NULL, before_value TEXT NOT NULL,
  after_value TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE expected_sets(
  id TEXT PRIMARY KEY, collection_id TEXT NOT NULL REFERENCES items(id),
  source TEXT NOT NULL, edition TEXT NOT NULL, ordering TEXT NOT NULL,
  complete INTEGER NOT NULL CHECK(complete IN (0,1)), members TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE INDEX expected_collection ON expected_sets(collection_id,created_at,id);
CREATE TABLE copy_policies(
  catalog TEXT PRIMARY KEY REFERENCES catalogs(id), definition TEXT NOT NULL);
CREATE TABLE processing_jobs(
  id TEXT PRIMARY KEY, profile TEXT NOT NULL REFERENCES profiles(id),
  file_id TEXT NOT NULL REFERENCES files(id), operation TEXT NOT NULL,
  location TEXT NOT NULL REFERENCES locations(id),
  snapshot TEXT NOT NULL, options TEXT NOT NULL, cache_key TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'queued', attempts INTEGER NOT NULL DEFAULT 0,
  result TEXT, error TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  finished_at TEXT);
CREATE INDEX jobs_pending ON processing_jobs(profile,state,id);
CREATE INDEX jobs_cache ON processing_jobs(profile,file_id,operation,cache_key,state);
CREATE TABLE file_facts(
  profile TEXT NOT NULL REFERENCES profiles(id), file_id TEXT NOT NULL REFERENCES files(id),
  operation TEXT NOT NULL, job_id TEXT NOT NULL REFERENCES processing_jobs(id),
  snapshot TEXT NOT NULL, data TEXT NOT NULL, status TEXT NOT NULL,
  PRIMARY KEY(profile,file_id,operation));
CREATE TABLE content_baselines(
  profile TEXT NOT NULL REFERENCES profiles(id), file_id TEXT NOT NULL REFERENCES files(id),
  algorithm TEXT NOT NULL, digest TEXT NOT NULL, size INTEGER NOT NULL,
  job_id TEXT NOT NULL REFERENCES processing_jobs(id),
  PRIMARY KEY(profile,file_id,algorithm));
CREATE INDEX content_duplicates ON content_baselines(algorithm,size,digest);
CREATE TABLE text_segments(
  id INTEGER PRIMARY KEY, profile TEXT NOT NULL REFERENCES profiles(id),
  file_id TEXT NOT NULL REFERENCES files(id), job_id TEXT NOT NULL REFERENCES processing_jobs(id),
  ordinal INTEGER NOT NULL, locator TEXT NOT NULL, text TEXT NOT NULL,
  UNIQUE(profile,file_id,ordinal));
CREATE TABLE refresh_targets(
  profile TEXT NOT NULL REFERENCES profiles(id), catalog TEXT NOT NULL REFERENCES catalogs(id),
  application TEXT NOT NULL, endpoint TEXT NOT NULL, credential_env TEXT NOT NULL,
  PRIMARY KEY(profile,catalog));
CREATE TABLE refresh_events(
  id INTEGER PRIMARY KEY, profile TEXT NOT NULL REFERENCES profiles(id),
  catalog TEXT NOT NULL REFERENCES catalogs(id), target TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'queued', attempts INTEGER NOT NULL DEFAULT 0,
  error TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, finished_at TEXT);
CREATE INDEX refresh_pending ON refresh_events(profile,state,id);
CREATE TABLE text_terms(
  term TEXT NOT NULL, segment_id INTEGER NOT NULL REFERENCES text_segments(id) ON DELETE CASCADE,
  PRIMARY KEY(term,segment_id));
CREATE TABLE refresh_dirty(
  profile TEXT NOT NULL REFERENCES profiles(id), catalog TEXT NOT NULL REFERENCES catalogs(id),
  target TEXT NOT NULL, PRIMARY KEY(profile,catalog));
CREATE TABLE watch_revisions(
  profile TEXT NOT NULL REFERENCES profiles(id), file_id TEXT NOT NULL REFERENCES files(id),
  revision TEXT NOT NULL, stable_since REAL NOT NULL, PRIMARY KEY(profile,file_id));
CREATE INDEX jobs_source_queue ON processing_jobs(profile,state,location,created_at,id);
