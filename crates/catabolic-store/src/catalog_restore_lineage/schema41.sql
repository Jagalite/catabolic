-- Candidate41 only. Historical origin is NOT a cleanup witness or a permission
-- to execute/replay an old command. Only the isolated exact restore owner may
-- install the transient row-matching function; ordinary connections fail closed.
CREATE TABLE catalog_restore_lineages (
 restore_id TEXT NOT NULL PRIMARY KEY,
 catalog_id TEXT NOT NULL,
 from_incarnation TEXT NOT NULL,
 to_incarnation TEXT NOT NULL UNIQUE,
 parent_restore_id TEXT REFERENCES catalog_restore_lineages(restore_id),
 proof_json TEXT NOT NULL CHECK(json_valid(proof_json)),
 CHECK(from_incarnation != to_incarnation),
 CHECK(json_extract(proof_json,'$.restore_id') IS restore_id),
 CHECK(json_extract(proof_json,'$.catalog_id') IS catalog_id),
 CHECK(json_extract(proof_json,'$.from_incarnation') IS from_incarnation),
 CHECK(json_extract(proof_json,'$.to_incarnation') IS to_incarnation),
 CHECK(json_extract(proof_json,'$.parent_restore_id') IS parent_restore_id)
);
CREATE TRIGGER catalog_restore_lineages_insert BEFORE INSERT ON catalog_restore_lineages
 WHEN catabolic_restore_lineage_insert('origin',NEW.restore_id,NEW.proof_json) IS NOT 1
 BEGIN SELECT RAISE(ABORT,'exact native restore lineage authority required'); END;
CREATE TRIGGER catalog_restore_lineages_update BEFORE UPDATE ON catalog_restore_lineages
 BEGIN SELECT RAISE(ABORT,'native restore lineage is immutable'); END;
CREATE TRIGGER catalog_restore_lineages_delete BEFORE DELETE ON catalog_restore_lineages
 BEGIN SELECT RAISE(ABORT,'native restore lineage is immutable'); END;
CREATE TABLE catalog_restore_worker_history (
 restore_id TEXT NOT NULL REFERENCES catalog_restore_lineages(restore_id),
 owner_kind TEXT NOT NULL CHECK(owner_kind IN ('processing','observation')),
 owner_id TEXT NOT NULL,
 origin_incarnation TEXT NOT NULL,
 tuple_sha256 TEXT NOT NULL CHECK(length(tuple_sha256)=64 AND tuple_sha256 NOT GLOB '*[^0-9a-f]*'),
 proof_json TEXT NOT NULL CHECK(json_valid(proof_json)),
 PRIMARY KEY(restore_id,owner_kind,owner_id),
 CHECK(json_extract(proof_json,'$.restore_id') IS restore_id),
 CHECK(json_extract(proof_json,'$.owner_kind') IS owner_kind),
 CHECK(json_extract(proof_json,'$.owner_id') IS owner_id),
 CHECK(json_extract(proof_json,'$.origin_incarnation') IS origin_incarnation),
 CHECK(json_extract(proof_json,'$.tuple_sha256') IS tuple_sha256)
);
CREATE TRIGGER catalog_restore_worker_history_insert BEFORE INSERT ON catalog_restore_worker_history
 WHEN catabolic_restore_lineage_insert('worker',NEW.restore_id||':'||NEW.owner_kind||':'||NEW.owner_id,NEW.proof_json) IS NOT 1
 BEGIN SELECT RAISE(ABORT,'exact native restore historical owner required'); END;
CREATE TRIGGER catalog_restore_worker_history_update BEFORE UPDATE ON catalog_restore_worker_history
 BEGIN SELECT RAISE(ABORT,'native historical worker evidence is immutable'); END;
CREATE TRIGGER catalog_restore_worker_history_delete BEFORE DELETE ON catalog_restore_worker_history
 BEGIN SELECT RAISE(ABORT,'native historical worker evidence is immutable'); END;
