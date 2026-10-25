-- Unregistered additive candidate41 owner. Permanent typed effects survive
-- ordinary command-result compaction; a receipt commitment alone is not effect
-- evidence. The retained owner validates complete child counts/digests before
-- committing. No generic binding/observation write capability is introduced.
-- A permit may exist only inside the receipt owner's transaction. The absent
-- deferred commit barrier prevents committing any retained write capability.
CREATE TABLE catalog_source_succession_permits (
 profile TEXT NOT NULL,
 source_id TEXT NOT NULL,
 succession_revision INTEGER NOT NULL CHECK(typeof(succession_revision)='integer' AND succession_revision>0),
 incarnation TEXT NOT NULL,
 barrier_id INTEGER NOT NULL DEFAULT 1 CHECK(barrier_id=1),
 PRIMARY KEY(profile,source_id,succession_revision),
 FOREIGN KEY(barrier_id) REFERENCES catalog_commit_barrier(id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER catalog_source_succession_permit_insert BEFORE INSERT ON catalog_source_succession_permits
 WHEN NEW.incarnation IS NOT (SELECT incarnation FROM catalog_state WHERE id=1)
 OR catalog_source_succession_permit_allowed(NEW.profile,NEW.source_id,NEW.succession_revision,NEW.incarnation,NEW.barrier_id) IS NOT 1
 BEGIN SELECT RAISE(ABORT,'source succession permit requires exact retained owner'); END;
CREATE TRIGGER catalog_source_succession_permit_no_update BEFORE UPDATE ON catalog_source_succession_permits
 BEGIN SELECT RAISE(ABORT,'source succession permit immutable'); END;
CREATE TABLE catalog_source_successions (
 profile TEXT NOT NULL,
 source_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0),
 predecessor_digest TEXT NOT NULL CHECK(length(predecessor_digest)=64 AND predecessor_digest NOT GLOB '*[^0-9a-f]*'),
 record_digest TEXT NOT NULL UNIQUE CHECK(length(record_digest)=64 AND record_digest NOT GLOB '*[^0-9a-f]*'),
 catalog_id TEXT NOT NULL,
 incarnation TEXT NOT NULL,
 expected_generation INTEGER NOT NULL CHECK(typeof(expected_generation)='integer' AND expected_generation>=0),
 committed_generation INTEGER NOT NULL CHECK(typeof(committed_generation)='integer' AND committed_generation>=expected_generation),
 command_host_namespace TEXT NOT NULL,
 command_operation_id TEXT NOT NULL,
 payload_digest TEXT NOT NULL CHECK(length(payload_digest)=64 AND payload_digest NOT GLOB '*[^0-9a-f]*'),
 admission_digest TEXT NOT NULL CHECK(length(admission_digest)=64 AND admission_digest NOT GLOB '*[^0-9a-f]*'),
 old_binding_json TEXT NOT NULL CHECK(length(CAST(old_binding_json AS BLOB))<=65536 AND json_valid(old_binding_json) AND json_type(old_binding_json)='object'),
 new_binding_json TEXT NOT NULL CHECK(length(CAST(new_binding_json AS BLOB))<=65536 AND json_valid(new_binding_json) AND json_type(new_binding_json)='object'),
 policy_json TEXT NOT NULL CHECK(length(CAST(policy_json AS BLOB))<=65536 AND json_valid(policy_json) AND json_type(policy_json)='object'),
 old_mapping_json TEXT NOT NULL CHECK(length(CAST(old_mapping_json AS BLOB))<=65536 AND json_valid(old_mapping_json) AND json_type(old_mapping_json)='object'),
 new_mapping_json TEXT NOT NULL CHECK(length(CAST(new_mapping_json AS BLOB))<=65536 AND json_valid(new_mapping_json) AND json_type(new_mapping_json)='object'),
 closure_digest TEXT NOT NULL CHECK(length(closure_digest)=64 AND closure_digest NOT GLOB '*[^0-9a-f]*'),
 file_count INTEGER NOT NULL CHECK(typeof(file_count)='integer' AND file_count BETWEEN 0 AND 1024),
 context_count INTEGER NOT NULL CHECK(typeof(context_count)='integer' AND context_count BETWEEN 0 AND 64),
 head_count INTEGER NOT NULL CHECK(typeof(head_count)='integer' AND head_count BETWEEN 1 AND 16384),
 scope_count INTEGER NOT NULL CHECK(typeof(scope_count)='integer' AND scope_count BETWEEN 1 AND 16384),
 evidence_count INTEGER NOT NULL CHECK(typeof(evidence_count)='integer' AND evidence_count BETWEEN 0 AND 16384),
 evidence_digest TEXT NOT NULL CHECK(length(evidence_digest)=64 AND evidence_digest NOT GLOB '*[^0-9a-f]*'),
 result_digest TEXT NOT NULL CHECK(length(result_digest)=64 AND result_digest NOT GLOB '*[^0-9a-f]*'),
 result_json TEXT NOT NULL CHECK(length(CAST(result_json AS BLOB))<=8388608 AND json_valid(result_json) AND json_type(result_json)='object'),
 PRIMARY KEY(profile,source_id,revision),
 UNIQUE(catalog_id,command_host_namespace,command_operation_id),
 FOREIGN KEY(catalog_id,command_host_namespace,command_operation_id)
   REFERENCES catalog_command_receipts(catalog_id,host_namespace,operation_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE catalog_source_succession_files (
 profile TEXT NOT NULL,
 source_id TEXT NOT NULL,
 succession_revision INTEGER NOT NULL,
 file_id TEXT NOT NULL,
 relative_path TEXT NOT NULL,
 disposition TEXT NOT NULL CHECK(disposition='verified_whole_original'),
 old_scan_provenance_json TEXT NOT NULL CHECK(length(CAST(old_scan_provenance_json AS BLOB))<=65536 AND json_valid(old_scan_provenance_json) AND json_type(old_scan_provenance_json) IN ('object','null')),
 old_observation_json TEXT NOT NULL CHECK(length(CAST(old_observation_json AS BLOB))<=65536 AND json_valid(old_observation_json) AND json_type(old_observation_json)='object'),
 old_occurrence_json TEXT NOT NULL CHECK(length(CAST(old_occurrence_json AS BLOB))<=65536 AND json_valid(old_occurrence_json) AND json_type(old_occurrence_json)='object'),
 new_occurrence_json TEXT NOT NULL CHECK(length(CAST(new_occurrence_json AS BLOB))<=65536 AND json_valid(new_occurrence_json) AND json_type(new_occurrence_json)='object'),
 old_ctime_ns INTEGER NOT NULL CHECK(typeof(old_ctime_ns)='integer'),
 new_ctime_ns INTEGER NOT NULL CHECK(typeof(new_ctime_ns)='integer'),
 content_sha256 TEXT NOT NULL CHECK(length(content_sha256)=64 AND content_sha256 NOT GLOB '*[^0-9a-f]*'),
 bytes INTEGER NOT NULL CHECK(typeof(bytes)='integer' AND bytes>=0),
 PRIMARY KEY(profile,source_id,succession_revision,file_id),
 UNIQUE(profile,source_id,succession_revision,relative_path),
 FOREIGN KEY(profile,source_id,succession_revision) REFERENCES catalog_source_successions(profile,source_id,revision) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE catalog_source_succession_contexts (
 profile TEXT NOT NULL,
 source_id TEXT NOT NULL,
 succession_revision INTEGER NOT NULL,
 context_id TEXT NOT NULL,
 head_count INTEGER NOT NULL CHECK(typeof(head_count)='integer' AND head_count BETWEEN 1 AND 8192),
 old_heads_json TEXT NOT NULL CHECK(length(CAST(old_heads_json AS BLOB))<=8388608 AND json_valid(old_heads_json) AND json_type(old_heads_json)='array'),
 renewal_json TEXT NOT NULL CHECK(length(CAST(renewal_json AS BLOB))<=8388608 AND json_valid(renewal_json) AND json_type(renewal_json)='object'),
 result_json TEXT NOT NULL CHECK(length(CAST(result_json AS BLOB))<=8388608 AND json_valid(result_json) AND json_type(result_json)='object'),
 PRIMARY KEY(profile,source_id,succession_revision,context_id),
 FOREIGN KEY(profile,source_id,succession_revision) REFERENCES catalog_source_successions(profile,source_id,revision) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER catalog_source_succession_sequence BEFORE INSERT ON catalog_source_successions
 WHEN NEW.catalog_id IS NOT (SELECT catalog_id FROM catalog_state WHERE id=1)
 OR NEW.incarnation IS NOT (SELECT incarnation FROM catalog_state WHERE id=1)
 OR NEW.revision IS NOT coalesce((SELECT max(revision)+1 FROM catalog_source_successions WHERE profile=NEW.profile AND source_id=NEW.source_id),1)
 OR (NEW.revision>1 AND NOT EXISTS(SELECT 1 FROM catalog_source_successions p
     WHERE p.profile=NEW.profile AND p.source_id=NEW.source_id AND p.revision=NEW.revision-1
       AND p.record_digest=NEW.predecessor_digest AND p.new_binding_json=NEW.old_binding_json
       AND p.new_mapping_json=NEW.old_mapping_json AND p.committed_generation<=NEW.expected_generation))
 BEGIN SELECT RAISE(ABORT,'source succession predecessor differs'); END;
CREATE TRIGGER catalog_source_succession_no_update BEFORE UPDATE ON catalog_source_successions
 BEGIN SELECT RAISE(ABORT,'source succession history immutable'); END;
CREATE TRIGGER catalog_source_succession_no_delete BEFORE DELETE ON catalog_source_successions
 BEGIN SELECT RAISE(ABORT,'source succession history retained'); END;
CREATE TRIGGER catalog_source_succession_files_no_update BEFORE UPDATE ON catalog_source_succession_files
 BEGIN SELECT RAISE(ABORT,'source succession file effect immutable'); END;
CREATE TRIGGER catalog_source_succession_files_no_delete BEFORE DELETE ON catalog_source_succession_files
 BEGIN SELECT RAISE(ABORT,'source succession file effect retained'); END;
CREATE TRIGGER catalog_source_succession_contexts_no_update BEFORE UPDATE ON catalog_source_succession_contexts
 BEGIN SELECT RAISE(ABORT,'source succession context effect immutable'); END;
CREATE TRIGGER catalog_source_succession_contexts_no_delete BEFORE DELETE ON catalog_source_succession_contexts
 BEGIN SELECT RAISE(ABORT,'source succession context effect retained'); END;

-- This is a provenance-preserving transfer, not a claim that a probe job or
-- observation scan ran against the successor inode. Original job/scan records
-- and original component occurrences remain unchanged. The current evidence
-- resolver must verify this complete effect and its exact occurrence chain
-- before projecting renewed facts/headers/components/assertions as current.
-- Active assertions transfer through exact old/new component evidence; their
-- historical proposal and occurrence endpoints are never rewritten.
CREATE TABLE catalog_source_succession_evidence (
 profile TEXT NOT NULL,
 source_id TEXT NOT NULL,
 succession_revision INTEGER NOT NULL,
 file_id TEXT NOT NULL,
 evidence_kind TEXT NOT NULL CHECK(evidence_kind IN ('processing_fact','media_header','component_occurrence','component_assertion')),
 evidence_key TEXT NOT NULL CHECK(length(CAST(evidence_key AS BLOB))<=65536 AND json_valid(evidence_key) AND json_type(evidence_key)='array'),
 predecessor_digest TEXT NOT NULL CHECK(length(predecessor_digest)=64 AND predecessor_digest NOT GLOB '*[^0-9a-f]*'),
 evidence_digest TEXT NOT NULL CHECK(length(evidence_digest)=64 AND evidence_digest NOT GLOB '*[^0-9a-f]*'),
 original_json TEXT NOT NULL CHECK(length(CAST(original_json AS BLOB))<=8388608 AND json_valid(original_json) AND json_type(original_json)='object'),
 renewed_json TEXT NOT NULL CHECK(length(CAST(renewed_json AS BLOB))<=8388608 AND json_valid(renewed_json) AND json_type(renewed_json)='object'),
 PRIMARY KEY(profile,source_id,succession_revision,file_id,evidence_kind,evidence_key),
 FOREIGN KEY(profile,source_id,succession_revision,file_id)
   REFERENCES catalog_source_succession_files(profile,source_id,succession_revision,file_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER catalog_source_succession_evidence_no_update BEFORE UPDATE ON catalog_source_succession_evidence
 BEGIN SELECT RAISE(ABORT,'source succession provenance immutable'); END;
CREATE TRIGGER catalog_source_succession_evidence_no_delete BEFORE DELETE ON catalog_source_succession_evidence
 BEGIN SELECT RAISE(ABORT,'source succession provenance retained'); END;

CREATE TRIGGER catalog_source_successions_insert_owner BEFORE INSERT ON catalog_source_successions
 WHEN NOT EXISTS(SELECT 1 FROM catalog_source_succession_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation
   WHERE p.profile=NEW.profile AND p.source_id=NEW.source_id AND p.succession_revision=NEW.revision)
 BEGIN SELECT RAISE(ABORT,'source succession requires retained typed owner'); END;

CREATE TRIGGER catalog_source_succession_files_insert_owner BEFORE INSERT ON catalog_source_succession_files
 WHEN NOT EXISTS(SELECT 1 FROM catalog_source_succession_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation
   WHERE p.profile=NEW.profile AND p.source_id=NEW.source_id AND p.succession_revision=NEW.succession_revision)
 BEGIN SELECT RAISE(ABORT,'source succession requires retained typed owner'); END;

CREATE TRIGGER catalog_source_succession_contexts_insert_owner BEFORE INSERT ON catalog_source_succession_contexts
 WHEN NOT EXISTS(SELECT 1 FROM catalog_source_succession_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation
   WHERE p.profile=NEW.profile AND p.source_id=NEW.source_id AND p.succession_revision=NEW.succession_revision)
 BEGIN SELECT RAISE(ABORT,'source succession requires retained typed owner'); END;

CREATE TRIGGER catalog_source_succession_evidence_insert_owner BEFORE INSERT ON catalog_source_succession_evidence
 WHEN NOT EXISTS(SELECT 1 FROM catalog_source_succession_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation
   WHERE p.profile=NEW.profile AND p.source_id=NEW.source_id AND p.succession_revision=NEW.succession_revision)
 BEGIN SELECT RAISE(ABORT,'source succession requires retained typed owner'); END;

-- Exact permanent current-head closure, including absence (revision=0).
-- The private owner captures source policy, physical, component dependency and
-- structural association heads. Returning to byte-equal values never restores
-- an old transfer after any witnessed entity head advances.
CREATE TABLE catalog_source_succession_heads (
 profile TEXT NOT NULL,
 source_id TEXT NOT NULL,
 succession_revision INTEGER NOT NULL,
 entity_kind TEXT NOT NULL CHECK(length(CAST(entity_kind AS BLOB)) BETWEEN 1 AND 256),
 entity_key TEXT NOT NULL CHECK(length(CAST(entity_key AS BLOB))<=65536 AND json_valid(entity_key) AND json_type(entity_key)='array'),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0),
 deleted INTEGER NOT NULL CHECK(deleted IN (0,1) AND (revision>0 OR deleted=0)),
 PRIMARY KEY(profile,source_id,succession_revision,entity_kind,entity_key),
 FOREIGN KEY(profile,source_id,succession_revision) REFERENCES catalog_source_successions(profile,source_id,revision) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER catalog_source_succession_heads_no_update BEFORE UPDATE ON catalog_source_succession_heads
 BEGIN SELECT RAISE(ABORT,'source succession heads immutable'); END;
CREATE TRIGGER catalog_source_succession_heads_no_delete BEFORE DELETE ON catalog_source_succession_heads
 BEGIN SELECT RAISE(ABORT,'source succession heads retained'); END;
CREATE TRIGGER catalog_source_succession_heads_insert_owner BEFORE INSERT ON catalog_source_succession_heads
 WHEN NOT EXISTS(SELECT 1 FROM catalog_source_succession_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation
   WHERE p.profile=NEW.profile AND p.source_id=NEW.source_id AND p.succession_revision=NEW.succession_revision)
 BEGIN SELECT RAISE(ABORT,'source succession requires retained typed owner'); END;
CREATE TABLE catalog_source_succession_context_heads (
 profile TEXT NOT NULL,source_id TEXT NOT NULL,succession_revision INTEGER NOT NULL,context_id TEXT NOT NULL,
 entity_kind TEXT NOT NULL,entity_key TEXT NOT NULL,
 PRIMARY KEY(profile,source_id,succession_revision,context_id,entity_kind,entity_key),
 FOREIGN KEY(profile,source_id,succession_revision,context_id) REFERENCES catalog_source_succession_contexts(profile,source_id,succession_revision,context_id) DEFERRABLE INITIALLY DEFERRED,
 FOREIGN KEY(profile,source_id,succession_revision,entity_kind,entity_key) REFERENCES catalog_source_succession_heads(profile,source_id,succession_revision,entity_kind,entity_key) DEFERRABLE INITIALLY DEFERRED
);
CREATE TRIGGER catalog_source_succession_context_heads_no_update BEFORE UPDATE ON catalog_source_succession_context_heads
 BEGIN SELECT RAISE(ABORT,'source succession context membership immutable'); END;
CREATE TRIGGER catalog_source_succession_context_heads_no_delete BEFORE DELETE ON catalog_source_succession_context_heads
 BEGIN SELECT RAISE(ABORT,'source succession context membership retained'); END;
CREATE TRIGGER catalog_source_succession_context_heads_insert_owner BEFORE INSERT ON catalog_source_succession_context_heads
 WHEN NOT EXISTS(SELECT 1 FROM catalog_source_succession_permits p JOIN catalog_state s ON s.id=1 AND s.incarnation=p.incarnation
   WHERE p.profile=NEW.profile AND p.source_id=NEW.source_id AND p.succession_revision=NEW.succession_revision)
 BEGIN SELECT RAISE(ABORT,'source succession requires retained typed owner'); END;
CREATE VIEW catalog_source_current_head_effects AS
 SELECT s.profile,s.source_id,s.revision FROM catalog_source_successions s
 WHERE EXISTS(SELECT 1 FROM catalog_source_current_scope_effects scoped WHERE scoped.profile=s.profile AND scoped.source_id=s.source_id AND scoped.revision=s.revision)
 AND s.head_count=(SELECT count(*) FROM catalog_source_succession_heads h WHERE h.profile=s.profile AND h.source_id=s.source_id AND h.succession_revision=s.revision)
 AND s.file_count=(SELECT count(*) FROM catalog_source_succession_files f WHERE f.profile=s.profile AND f.source_id=s.source_id AND f.succession_revision=s.revision)
 AND s.context_count=(SELECT count(*) FROM catalog_source_succession_contexts c WHERE c.profile=s.profile AND c.source_id=s.source_id AND c.succession_revision=s.revision)
 AND s.evidence_count=(SELECT count(*) FROM catalog_source_succession_evidence e WHERE e.profile=s.profile AND e.source_id=s.source_id AND e.succession_revision=s.revision)
 AND NOT EXISTS(SELECT 1 FROM catalog_source_succession_contexts c WHERE c.profile=s.profile AND c.source_id=s.source_id AND c.succession_revision=s.revision AND NOT EXISTS(SELECT 1 FROM catalog_source_current_context_membership valid WHERE valid.profile=c.profile AND valid.source_id=c.source_id AND valid.succession_revision=c.succession_revision AND valid.context_id=c.context_id AND valid.current=1))
 AND NOT EXISTS(SELECT id FROM files WHERE location=s.source_id EXCEPT SELECT file_id FROM catalog_source_succession_files WHERE profile=s.profile AND source_id=s.source_id AND succession_revision=s.revision)
 AND NOT EXISTS(SELECT file_id FROM catalog_source_succession_files WHERE profile=s.profile AND source_id=s.source_id AND succession_revision=s.revision EXCEPT SELECT id FROM files WHERE location=s.source_id)
 AND NOT EXISTS(SELECT DISTINCT v.segmentation_context_id FROM catalog_live_versions v JOIN catalog_version_bindings m ON m.version_id=v.id AND m.version_revision=v.current_revision JOIN catalog_binding_revisions b ON b.binding_id=m.binding_id AND b.revision=m.binding_revision JOIN files f ON f.id=b.file_id WHERE b.profile=s.profile AND f.location=s.source_id EXCEPT SELECT context_id FROM catalog_source_succession_contexts WHERE profile=s.profile AND source_id=s.source_id AND succession_revision=s.revision)
 AND NOT EXISTS(SELECT context_id FROM catalog_source_succession_contexts WHERE profile=s.profile AND source_id=s.source_id AND succession_revision=s.revision EXCEPT SELECT DISTINCT v.segmentation_context_id FROM catalog_live_versions v JOIN catalog_version_bindings m ON m.version_id=v.id AND m.version_revision=v.current_revision JOIN catalog_binding_revisions b ON b.binding_id=m.binding_id AND b.revision=m.binding_revision JOIN files f ON f.id=b.file_id WHERE b.profile=s.profile AND f.location=s.source_id)
 AND NOT EXISTS(SELECT 1 FROM catalog_source_succession_heads h
 LEFT JOIN catalog_entity_revisions current ON current.entity_kind=h.entity_kind AND current.entity_key=h.entity_key
 WHERE h.profile=s.profile AND h.source_id=s.source_id AND h.succession_revision=s.revision
 AND (coalesce(current.revision,0)<>h.revision OR coalesce(current.deleted,0)<>h.deleted));

-- Candidate41-only current evidence. These relations never rewrite original
-- job, scan, proposal, component or fact rows. Permanent receipt provenance is
-- required even when its ordinary result body has expired. Only the latest
-- authenticated binding effect at the exact current occurrence can contribute.
CREATE VIEW catalog_source_bounded_evidence AS
 SELECT e.profile,e.source_id,e.succession_revision,e.file_id,e.evidence_kind,e.evidence_key,e.predecessor_digest,e.evidence_digest,
 CASE WHEN (SELECT coalesce(sum(catalog_source_evidence_work(256+length(CAST(x.evidence_key AS BLOB))+length(CAST(x.original_json AS BLOB))+length(CAST(x.renewed_json AS BLOB)),x.succession_revision)),0) FROM catalog_source_succession_evidence x WHERE x.profile=e.profile AND x.source_id=e.source_id AND x.succession_revision=e.succession_revision)<=8388608 THEN e.original_json ELSE catalog_source_evidence_refused() END AS original_json,
 CASE WHEN (SELECT coalesce(sum(catalog_source_evidence_work(256+length(CAST(x.evidence_key AS BLOB))+length(CAST(x.original_json AS BLOB))+length(CAST(x.renewed_json AS BLOB)),x.succession_revision)),0) FROM catalog_source_succession_evidence x WHERE x.profile=e.profile AND x.source_id=e.source_id AND x.succession_revision=e.succession_revision)<=8388608 THEN e.renewed_json ELSE catalog_source_evidence_refused() END AS renewed_json
 FROM catalog_source_succession_evidence e;
CREATE VIEW catalog_source_live_transfers AS
 SELECT e.* FROM catalog_source_bounded_evidence e
 JOIN catalog_source_successions s ON s.profile=e.profile AND s.source_id=e.source_id AND s.revision=e.succession_revision
 JOIN catalog_source_current_head_effects heads ON heads.profile=s.profile AND heads.source_id=s.source_id AND heads.revision=s.revision
 JOIN catalog_state identity ON identity.id=1 AND identity.catalog_id=s.catalog_id AND identity.incarnation=s.incarnation
 JOIN catalog_command_receipts r ON r.catalog_id=s.catalog_id AND r.host_namespace=s.command_host_namespace AND r.operation_id=s.command_operation_id
 JOIN catalog_source_succession_files f ON f.profile=e.profile AND f.source_id=e.source_id AND f.succession_revision=e.succession_revision AND f.file_id=e.file_id
 JOIN bindings b ON b.profile=e.profile AND b.kind='source' AND b.owner=e.source_id
 JOIN files registry ON registry.id=e.file_id AND registry.location=e.source_id AND registry.path=f.relative_path
 JOIN observations o ON o.profile=e.profile AND o.file_id=e.file_id
 JOIN native_source_ctimes c ON c.profile=o.profile AND c.file_id=o.file_id AND c.size=o.size AND c.mtime_ns=o.mtime_ns AND c.device=o.device AND c.inode=o.inode
 WHERE catalog_source_evidence_work(1,s.revision)=1
 AND s.revision=(SELECT max(latest.revision) FROM catalog_source_successions latest WHERE latest.profile=e.profile AND latest.source_id=e.source_id)
 AND EXISTS(SELECT 1 FROM catalog_source_succession_heads h WHERE h.profile=e.profile AND h.source_id=e.source_id AND h.succession_revision=e.succession_revision AND h.entity_kind='bindings' AND h.entity_key=json_array(e.profile,'source',e.source_id))
 AND EXISTS(SELECT 1 FROM catalog_source_succession_heads h WHERE h.profile=e.profile AND h.source_id=e.source_id AND h.succession_revision=e.succession_revision AND h.entity_kind='source_identity_policies' AND h.entity_key=json_array(e.profile,e.source_id))
 AND EXISTS(SELECT 1 FROM catalog_source_succession_heads h WHERE h.profile=e.profile AND h.source_id=e.source_id AND h.succession_revision=e.succession_revision AND h.entity_kind='source_observation_policies' AND h.entity_key=json_array(e.profile,e.source_id))
 AND EXISTS(SELECT 1 FROM catalog_source_succession_heads h WHERE h.profile=e.profile AND h.source_id=e.source_id AND h.succession_revision=e.succession_revision AND h.entity_kind='files' AND h.entity_key=json_array(e.file_id))
 AND EXISTS(SELECT 1 FROM catalog_source_succession_heads h WHERE h.profile=e.profile AND h.source_id=e.source_id AND h.succession_revision=e.succession_revision AND h.entity_kind='observations' AND h.entity_key=json_array(e.profile,e.file_id))
 AND EXISTS(SELECT 1 FROM catalog_source_succession_heads h WHERE h.profile=e.profile AND h.source_id=e.source_id AND h.succession_revision=e.succession_revision AND h.entity_kind='native_source_ctimes' AND h.entity_key=json_array(e.profile,e.file_id))
 -- Source-correlated finite chain admission. Public SQL and sealed private
 -- writers additionally enforce their VM budgets; this is not a linear-work
 -- promise for arbitrary direct Store queries.
 AND CASE WHEN
 (SELECT count(*)<=16384 AND coalesce(sum(catalog_source_evidence_work(256+length(CAST(x.evidence_key AS BLOB))+length(CAST(x.original_json AS BLOB))+length(CAST(x.renewed_json AS BLOB)),x.succession_revision)),0)<=8388608
  FROM catalog_source_succession_evidence x WHERE x.profile=e.profile AND x.source_id=e.source_id)
 AND (SELECT count(*)<=16384 AND count(*)=max(x.revision) AND min(x.revision)=1
      AND coalesce(sum(catalog_source_evidence_work(512+length(CAST(x.old_binding_json AS BLOB))+length(CAST(x.new_binding_json AS BLOB))+length(CAST(x.old_mapping_json AS BLOB))+length(CAST(x.new_mapping_json AS BLOB))+length(CAST(x.result_json AS BLOB)),x.revision)),0)<=8388608
  FROM catalog_source_successions x WHERE x.profile=e.profile AND x.source_id=e.source_id)
 THEN
 coalesce((SELECT sum(catalog_source_evidence_work(1,x.revision) * CASE WHEN (
   x.catalog_id<>s.catalog_id OR x.incarnation<>s.incarnation
   OR cr.payload_digest IS NOT x.payload_digest OR cr.admission_incarnation IS NOT x.incarnation
   OR cr.expected_generation IS NOT x.expected_generation OR cr.committed_generation IS NOT x.committed_generation
   OR NOT ((cr.result_state='expired' AND cr.result_json IS NULL) OR (cr.result_state='full' AND cr.result_json=x.result_json))
   OR (x.revision=1 AND x.predecessor_digest<>'0000000000000000000000000000000000000000000000000000000000000000')
   OR (x.revision>1 AND (prior.record_digest IS NOT x.predecessor_digest
       OR prior.new_binding_json IS NOT x.old_binding_json OR prior.new_mapping_json IS NOT x.old_mapping_json
       OR x.expected_generation<prior.committed_generation))) THEN 1 ELSE 0 END)
 FROM catalog_source_successions x
  LEFT JOIN catalog_command_receipts cr ON cr.catalog_id=x.catalog_id AND cr.host_namespace=x.command_host_namespace AND cr.operation_id=x.command_operation_id
  LEFT JOIN catalog_source_successions prior ON prior.profile=x.profile AND prior.source_id=x.source_id AND prior.revision=x.revision-1
 WHERE x.profile=e.profile AND x.source_id=e.source_id),0)=0

 AND EXISTS(WITH RECURSIVE chain(revision,predecessor_digest,evidence_digest,original_json,depth) AS (
  SELECT e.succession_revision,e.predecessor_digest,e.evidence_digest,e.original_json,1
  UNION ALL
  SELECT prior.succession_revision,prior.predecessor_digest,prior.evidence_digest,prior.original_json,chain.depth+1
  FROM chain JOIN catalog_source_succession_evidence prior
   ON prior.profile=e.profile AND prior.source_id=e.source_id AND prior.file_id=e.file_id
   AND prior.evidence_kind=e.evidence_kind AND prior.evidence_key=e.evidence_key
   AND prior.succession_revision=chain.revision-1 AND prior.evidence_digest=chain.predecessor_digest
   AND prior.original_json=chain.original_json
  WHERE catalog_source_evidence_work(256+length(CAST(prior.original_json AS BLOB)),prior.succession_revision)>0
  AND chain.depth<16384 AND chain.predecessor_digest<>'0000000000000000000000000000000000000000000000000000000000000000'
 ) SELECT 1 FROM chain JOIN catalog_source_succession_files origin
   ON origin.profile=e.profile AND origin.source_id=e.source_id AND origin.file_id=e.file_id AND origin.succession_revision=chain.revision
 WHERE chain.predecessor_digest='0000000000000000000000000000000000000000000000000000000000000000'
 AND (e.evidence_kind NOT IN ('processing_fact','media_header') OR json_extract(origin.old_observation_json,'$.observation_origin')='scan'))
 ELSE catalog_source_evidence_refused() END
 AND r.payload_digest=s.payload_digest AND r.admission_incarnation=s.incarnation
 AND r.expected_generation=s.expected_generation AND r.committed_generation=s.committed_generation
 AND ((r.result_state='expired' AND r.result_json IS NULL) OR (r.result_state='full' AND r.result_json=s.result_json))
 AND b.root=json_extract(s.new_binding_json,'$.root') AND b.device=json_extract(s.new_binding_json,'$.device') AND b.inode=json_extract(s.new_binding_json,'$.inode')
 AND o.observation_origin='succession' AND o.scan_id IS NULL
 AND o.succession_source_id=f.source_id AND o.succession_revision=f.succession_revision
 AND o.status='present' AND o.size=json_extract(f.new_occurrence_json,'$.size') AND o.mtime_ns=json_extract(f.new_occurrence_json,'$.mtime_ns')
 AND o.device=json_extract(f.new_occurrence_json,'$.device') AND o.inode=json_extract(f.new_occurrence_json,'$.inode') AND c.ctime_ns=f.new_ctime_ns;
CREATE VIEW catalog_source_transferred_facts AS
 SELECT base.profile,base.file_id,base.operation,base.job_id,
 json_extract(e.renewed_json,'$.snapshot') AS snapshot,base.data,base.status
 FROM file_facts base JOIN catalog_source_live_transfers e ON e.profile=base.profile AND e.file_id=base.file_id AND e.evidence_kind='processing_fact'
 JOIN catalog_source_succession_heads base_head ON base_head.profile=e.profile AND base_head.source_id=e.source_id AND base_head.succession_revision=e.succession_revision AND base_head.entity_kind='file_facts' AND base_head.entity_key=json_array(base.profile,base.file_id,base.operation) AND base_head.revision>0 AND base_head.deleted=0
 WHERE json_extract(e.original_json,'$.profile')=base.profile AND json_extract(e.original_json,'$.file_id')=base.file_id
 AND json_extract(e.original_json,'$.operation')=base.operation AND json_extract(e.original_json,'$.job_id')=base.job_id
 AND json_extract(e.original_json,'$.snapshot')=base.snapshot AND json_extract(e.original_json,'$.data')=base.data AND json_extract(e.original_json,'$.status')=base.status
 AND json_extract(e.renewed_json,'$.profile')=base.profile AND json_extract(e.renewed_json,'$.file_id')=base.file_id
 AND json_extract(e.renewed_json,'$.operation')=base.operation AND json_extract(e.renewed_json,'$.job_id')=base.job_id
 AND json_extract(e.renewed_json,'$.data')=base.data AND json_extract(e.renewed_json,'$.status')=base.status;
CREATE VIEW catalog_source_effective_file_facts AS
 WITH renewed AS MATERIALIZED (SELECT * FROM catalog_source_transferred_facts)
 SELECT base.* FROM file_facts base WHERE NOT EXISTS(SELECT 1 FROM renewed WHERE renewed.profile=base.profile AND renewed.file_id=base.file_id AND renewed.operation=base.operation)
 UNION ALL SELECT * FROM renewed;
CREATE VIEW catalog_source_transferred_headers AS
 SELECT base.profile,base.file_id,NULL AS scan_id,
 json_extract(e.renewed_json,'$.size') AS size,json_extract(e.renewed_json,'$.mtime_ns') AS mtime_ns,
 json_extract(e.renewed_json,'$.device') AS device,json_extract(e.renewed_json,'$.inode') AS inode,
 base.status,base.reason,base.bytes_read
 FROM media_header_checks base JOIN catalog_source_live_transfers e ON e.profile=base.profile AND e.file_id=base.file_id AND e.evidence_kind='media_header'
 JOIN catalog_source_succession_heads base_head ON base_head.profile=e.profile AND base_head.source_id=e.source_id AND base_head.succession_revision=e.succession_revision AND base_head.entity_kind='media_header_checks' AND base_head.entity_key=json_array(base.profile,base.file_id) AND base_head.revision>0 AND base_head.deleted=0
 WHERE json_extract(e.original_json,'$.profile')=base.profile AND json_extract(e.original_json,'$.file_id')=base.file_id
 AND json_extract(e.original_json,'$.scan_id')=base.scan_id
 AND json_extract(e.original_json,'$.size')=base.size AND json_extract(e.original_json,'$.mtime_ns')=base.mtime_ns
 AND json_extract(e.original_json,'$.device')=base.device AND json_extract(e.original_json,'$.inode')=base.inode
 AND json_extract(e.original_json,'$.status')=base.status AND json_extract(e.original_json,'$.reason') IS base.reason AND json_extract(e.original_json,'$.bytes_read')=base.bytes_read
 AND json_extract(e.renewed_json,'$.profile')=base.profile AND json_extract(e.renewed_json,'$.file_id')=base.file_id
 AND json_type(e.renewed_json,'$.scan_id')='null' AND json_extract(e.renewed_json,'$.status')=base.status
 AND json_extract(e.renewed_json,'$.reason') IS base.reason AND json_extract(e.renewed_json,'$.bytes_read')=base.bytes_read;
CREATE VIEW catalog_source_effective_headers AS
 WITH renewed AS MATERIALIZED (SELECT * FROM catalog_source_transferred_headers)
 SELECT base.* FROM media_header_checks base WHERE NOT EXISTS(SELECT 1 FROM renewed WHERE renewed.profile=base.profile AND renewed.file_id=base.file_id)
 UNION ALL SELECT * FROM renewed;
CREATE VIEW catalog_source_effective_assertions AS
 SELECT base.* FROM component_assertions base
 UNION ALL
 SELECT base.id,json_extract(e.renewed_json,'$.occurrence_id') AS occurrence_id,
 json_extract(e.renewed_json,'$.component_id') AS component_id,json_extract(e.renewed_json,'$.payload') AS payload,base.active
 FROM component_assertions base JOIN catalog_source_live_transfers e ON e.evidence_kind='component_assertion'
 JOIN catalog_source_succession_heads base_head ON base_head.profile=e.profile AND base_head.source_id=e.source_id AND base_head.succession_revision=e.succession_revision AND base_head.entity_kind='component_assertions' AND base_head.entity_key=json_array(base.id) AND base_head.revision>0 AND base_head.deleted=0
 WHERE base.active=1 AND json_extract(e.original_json,'$.id')=base.id
 AND json_extract(e.original_json,'$.occurrence_id')=base.occurrence_id AND json_extract(e.original_json,'$.component_id')=base.component_id
 AND json_extract(e.original_json,'$.payload')=base.payload AND json_extract(e.original_json,'$.active')=base.active
 AND json_extract(e.renewed_json,'$.id')=base.id AND json_extract(e.renewed_json,'$.active')=base.active
 AND json_extract(e.renewed_json,'$.component_id')=base.component_id
 AND json_extract(e.renewed_json,'$.occurrence_id')<>base.occurrence_id
 AND EXISTS(SELECT 1 FROM catalog_source_current_occurrence_transfers transferred WHERE transferred.profile=e.profile AND transferred.file_id=e.file_id AND transferred.occurrence_id=json_extract(e.renewed_json,'$.occurrence_id'))
 AND NOT EXISTS(
   SELECT 1 FROM (
     SELECT 'proposals' AS kind,json_array(base.id) AS key
     UNION ALL SELECT 'media_components',json_array(base.component_id)
     UNION ALL SELECT 'component_occurrences',json_array(base.occurrence_id)
     UNION ALL SELECT 'component_occurrences',json_array(json_extract(e.renewed_json,'$.occurrence_id'))
     UNION ALL SELECT 'item_files',json_array(o.association_id) FROM component_occurrences o WHERE o.id IN(base.occurrence_id,json_extract(e.renewed_json,'$.occurrence_id'))
     UNION ALL SELECT 'items',json_array(o.item_id) FROM component_occurrences o WHERE o.id IN(base.occurrence_id,json_extract(e.renewed_json,'$.occurrence_id'))
   ) required
   WHERE NOT EXISTS(SELECT 1 FROM catalog_source_succession_heads h WHERE h.profile=e.profile AND h.source_id=e.source_id AND h.succession_revision=e.succession_revision AND h.entity_kind=required.kind AND h.entity_key=required.key AND h.revision>0 AND h.deleted=0))
 AND NOT EXISTS(
   SELECT 1 FROM (
     SELECT json_extract(d.value,'$.file_id') AS file_id FROM json_each(json_extract(e.renewed_json,'$.payload'),'$.dependencies') d
     UNION SELECT json_extract(json_extract(e.renewed_json,'$.payload'),'$.compatibility.video_file_id')
   ) dependency
   WHERE dependency.file_id IS NOT NULL AND (
     NOT EXISTS(SELECT 1 FROM files f WHERE f.id=dependency.file_id)
     OR EXISTS(
       SELECT 1 FROM (
         SELECT 'files' AS kind,json_array(dependency.file_id) AS key
         UNION ALL SELECT 'observations',json_array(e.profile,dependency.file_id)
         UNION ALL SELECT 'native_source_ctimes',json_array(e.profile,dependency.file_id)
         UNION ALL SELECT 'bindings',json_array(e.profile,'source',f.location) FROM files f WHERE f.id=dependency.file_id
         UNION ALL SELECT 'source_identity_policies',json_array(e.profile,f.location) FROM files f WHERE f.id=dependency.file_id
         UNION ALL SELECT 'source_observation_policies',json_array(e.profile,f.location) FROM files f WHERE f.id=dependency.file_id
       ) required
       WHERE NOT EXISTS(SELECT 1 FROM catalog_source_succession_heads h WHERE h.profile=e.profile AND h.source_id=e.source_id AND h.succession_revision=e.succession_revision AND h.entity_kind=required.kind AND h.entity_key=required.key)
     )
   ));

-- Permanent IDs distinguish renewed occurrences from real probe executions.
-- Historical rows remain readable; currentness additionally requires the exact
-- live transfer, even after result compaction or a later head invalidation.
CREATE VIEW catalog_source_recorded_occurrence_transfers AS
 SELECT e.profile,e.file_id,json_extract(e.renewed_json,'$.id') AS occurrence_id
 FROM catalog_source_bounded_evidence e WHERE e.evidence_kind='component_occurrence';
CREATE VIEW catalog_source_current_occurrence_transfers AS
 SELECT successor.id AS occurrence_id,successor.profile,successor.file_id
 FROM catalog_source_live_transfers e
 JOIN component_occurrences original ON original.id=json_extract(e.original_json,'$.id')
 JOIN component_occurrences successor ON successor.id=json_extract(e.renewed_json,'$.id')
 JOIN catalog_source_succession_heads old_head ON old_head.profile=e.profile AND old_head.source_id=e.source_id AND old_head.succession_revision=e.succession_revision AND old_head.entity_kind='component_occurrences' AND old_head.entity_key=json_array(original.id) AND old_head.revision>0 AND old_head.deleted=0
 JOIN catalog_source_succession_heads new_head ON new_head.profile=e.profile AND new_head.source_id=e.source_id AND new_head.succession_revision=e.succession_revision AND new_head.entity_kind='component_occurrences' AND new_head.entity_key=json_array(successor.id) AND new_head.revision>0 AND new_head.deleted=0
 WHERE e.evidence_kind='component_occurrence' AND original.id<>successor.id
 AND original.profile=e.profile AND successor.profile=e.profile
 AND original.file_id=e.file_id AND successor.file_id=e.file_id
 AND original.probe_job_id IS successor.probe_job_id
 AND json_extract(e.original_json,'$.id') IS original.id
 AND json_extract(e.renewed_json,'$.id') IS successor.id
 AND json_extract(e.original_json,'$.component_id') IS original.component_id
 AND json_extract(e.renewed_json,'$.component_id') IS successor.component_id
 AND json_extract(e.original_json,'$.profile') IS original.profile
 AND json_extract(e.renewed_json,'$.profile') IS successor.profile
 AND json_extract(e.original_json,'$.file_id') IS original.file_id
 AND json_extract(e.renewed_json,'$.file_id') IS successor.file_id
 AND json_extract(e.original_json,'$.association_id') IS original.association_id
 AND json_extract(e.renewed_json,'$.association_id') IS successor.association_id
 AND json_extract(e.original_json,'$.item_id') IS original.item_id
 AND json_extract(e.renewed_json,'$.item_id') IS successor.item_id
 AND json_extract(e.original_json,'$.revision') IS original.revision
 AND json_extract(e.renewed_json,'$.revision') IS successor.revision
 AND json_extract(e.original_json,'$.snapshot') IS original.snapshot
 AND json_extract(e.renewed_json,'$.snapshot') IS successor.snapshot
 AND json_extract(e.original_json,'$.association_evidence') IS original.association_evidence
 AND json_extract(e.renewed_json,'$.association_evidence') IS successor.association_evidence
 AND json_extract(e.original_json,'$.item_evidence') IS original.item_evidence
 AND json_extract(e.renewed_json,'$.item_evidence') IS successor.item_evidence
 AND json_extract(e.original_json,'$.storage') IS original.storage
 AND json_extract(e.renewed_json,'$.storage') IS successor.storage
 AND json_extract(e.original_json,'$.locator') IS original.locator
 AND json_extract(e.renewed_json,'$.locator') IS successor.locator
 AND json_extract(e.original_json,'$.probe_job_id') IS original.probe_job_id
 AND json_extract(e.renewed_json,'$.probe_job_id') IS successor.probe_job_id
 AND json_extract(e.original_json,'$.stream_key') IS original.stream_key
 AND json_extract(e.renewed_json,'$.stream_key') IS successor.stream_key
 AND json_extract(e.original_json,'$.probe_path') IS original.probe_path
 AND json_extract(e.renewed_json,'$.probe_path') IS successor.probe_path
 AND json_extract(e.original_json,'$.created_at') IS original.created_at
 AND json_extract(e.renewed_json,'$.created_at') IS successor.created_at
 AND NOT EXISTS(SELECT 1 FROM (
   SELECT 'item_files' AS kind,json_array(successor.association_id) AS key
   UNION ALL SELECT 'items',json_array(successor.item_id)
   UNION ALL SELECT 'media_components',json_array(successor.component_id)
 ) required WHERE NOT EXISTS(SELECT 1 FROM catalog_source_succession_heads h WHERE h.profile=e.profile AND h.source_id=e.source_id AND h.succession_revision=e.succession_revision AND h.entity_kind=required.kind AND h.entity_key=required.key AND h.revision>0 AND h.deleted=0));
