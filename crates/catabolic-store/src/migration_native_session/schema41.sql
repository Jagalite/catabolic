-- Historical40-to41 migration commitment, not bootstrap/restore authority.
-- Only the retained migration owner installs the exact-row connection function.
CREATE TABLE catalog_native_transition_40_41 (
 id INTEGER PRIMARY KEY CHECK(id=1),
 catalog_id TEXT NOT NULL CHECK(length(CAST(catalog_id AS BLOB)) BETWEEN 1 AND 256),
 source_incarnation TEXT NOT NULL CHECK(length(source_incarnation)=32 AND source_incarnation NOT GLOB '*[^0-9a-f]*'),
 source_generation INTEGER NOT NULL CHECK(typeof(source_generation)='integer' AND source_generation>=0),
 approved_plan_sha256 TEXT NOT NULL CHECK(length(approved_plan_sha256)=64 AND approved_plan_sha256 NOT GLOB '*[^0-9a-f]*'),
 source_full_ledger_sha256 TEXT NOT NULL CHECK(length(source_full_ledger_sha256)=64 AND source_full_ledger_sha256 NOT GLOB '*[^0-9a-f]*'),
 source_tables_sha256 TEXT NOT NULL CHECK(length(source_tables_sha256)=64 AND source_tables_sha256 NOT GLOB '*[^0-9a-f]*'),
 verified_backup_image_sha256 TEXT NOT NULL CHECK(length(verified_backup_image_sha256)=64 AND verified_backup_image_sha256 NOT GLOB '*[^0-9a-f]*'),
 migration41_sql_sha256 TEXT NOT NULL CHECK(length(migration41_sql_sha256)=64 AND migration41_sql_sha256 NOT GLOB '*[^0-9a-f]*'),
 closed41_reference_sha256 TEXT NOT NULL CHECK(length(closed41_reference_sha256)=64 AND closed41_reference_sha256 NOT GLOB '*[^0-9a-f]*'),
 migrated_generation INTEGER NOT NULL CHECK(typeof(migrated_generation)='integer' AND migrated_generation>=source_generation)
);
CREATE TRIGGER catalog_native_transition_40_41_insert BEFORE INSERT ON catalog_native_transition_40_41
 WHEN catabolic_native_transition_40_41_insert(json_array(NEW.catalog_id,NEW.source_incarnation,NEW.source_generation,NEW.approved_plan_sha256,NEW.source_full_ledger_sha256,NEW.source_tables_sha256,NEW.verified_backup_image_sha256,NEW.migration41_sql_sha256,NEW.closed41_reference_sha256,NEW.migrated_generation)) IS NOT 1
 BEGIN SELECT RAISE(ABORT,'exact native40-to41 migration owner required'); END;
CREATE TRIGGER catalog_native_transition_40_41_update BEFORE UPDATE ON catalog_native_transition_40_41
 BEGIN SELECT RAISE(ABORT,'native40-to41 transition is immutable'); END;
CREATE TRIGGER catalog_native_transition_40_41_delete BEFORE DELETE ON catalog_native_transition_40_41
 BEGIN SELECT RAISE(ABORT,'native40-to41 transition is retained'); END;
