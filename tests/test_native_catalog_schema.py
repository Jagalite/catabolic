#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""37 preserved schema regressions; not Rust-owner/release qualification.

Provenance: tests/parity/native-catalog-schema/source-provenance.json.
Independent schema review remains incomplete; these tests do not replace it.
"""

import hashlib
import json
import sqlite3
import unittest
import uuid
from pathlib import Path

from scripts.native_catalog_schema import (
    EVIDENCE_PATH,
    SIGNATURE_PATH,
    SQL_PATH,
    legacy_paths,
)

REPO = Path(__file__).resolve().parents[1]
ROOT = REPO / EVIDENCE_PATH
SQL = (REPO / SQL_PATH).read_text(encoding="utf-8")
MAX = 9223372036854775807
CID = "00000000-0000-0000-0000-000000000001"


def script(db, sql):
    # executescript implicitly commits; split with SQLite for transaction tests.
    statement = ""
    for line in sql.splitlines(True):
        statement += line
        if sqlite3.complete_statement(statement):
            db.execute(statement)
            statement = ""
    assert not statement.strip()


def legacy(populated=True):
    db = sqlite3.connect(":memory:", isolation_level=None)
    db.create_function(
        "catabolic_catalog_id_v1", 1, lambda s: str(uuid.UUID(s)), deterministic=True
    )
    db.create_function("catabolic_query_v1", 2, lambda s, p: s)
    db.create_function(
        "catabolic_sha256_v1", 1, lambda s: hashlib.sha256(s.encode()).hexdigest()
    )
    for p in legacy_paths(REPO):
        script(db, p.read_text())
    db.execute("PRAGMA foreign_keys=ON")
    db.execute("INSERT INTO meta VALUES (?,?)", ("database_id", CID))
    if populated:
        db.execute("INSERT INTO profiles VALUES ('p')")
        db.execute("INSERT INTO locations VALUES ('l')")
        for x in range(1, 5):
            db.execute("INSERT INTO files VALUES (?,?,?)", (f"f{x}", "l", f"path{x}"))
        for x in ["edition", "va", "vb", "free"]:
            db.execute(
                "INSERT INTO items VALUES (?,?,?)", (x, "test", "raw metadata " + x)
            )
        for a, f, i in [("a1", "f1", "va"), ("a2", "f2", "vb"), ("au", "f4", "free")]:
            db.execute(
                "INSERT INTO item_files(id,file_id,item_id,role,part,metadata,origin) VALUES (?,?,?,?,?,?,?)",
                (a, f, i, "primary", 1, "{raw retained", "explicit"),
            )
    return db


def native_settings(db):
    db.execute("PRAGMA recursive_triggers=ON")
    db.execute("PRAGMA read_uncommitted=OFF")
    assert db.execute("PRAGMA foreign_keys").fetchone() == (1,)
    assert db.execute("PRAGMA recursive_triggers").fetchone() == (1,)
    assert db.execute("PRAGMA read_uncommitted").fetchone() == (0,)


def upgraded(populated=True):
    db = legacy(populated)
    db.execute("BEGIN IMMEDIATE")
    script(db, SQL)
    native_settings(db)
    db.execute("COMMIT")
    return db


def snapshot(db, table_names=None):
    table_names = table_names or [
        r[0]
        for r in db.execute(
            "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        )
    ]
    return {
        t: sorted(list(db.execute('SELECT * FROM "' + t + '"')), key=repr)
        for t in table_names
    }


def generation(db):
    return db.execute("SELECT generation FROM catalog_state WHERE id=1").fetchone()[0]


def incarnation(db):
    return db.execute("SELECT incarnation FROM catalog_state WHERE id=1").fetchone()[0]


def permit(db, kind, id):
    db.execute(
        "INSERT INTO catalog_mutation_permits(scope_kind,scope_id,incarnation) VALUES (?,?,?)",
        (kind, id, incarnation(db)),
    )


def receipt(db, op="op1", expected=0):
    db.execute(
        "INSERT INTO catalog_command_receipts VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (
            CID,
            "host",
            op,
            1,
            1,
            "a" * 64,
            incarnation(db),
            expected,
            generation(db),
            10,
            20,
            "full",
            '{"original":true}',
            None,
        ),
    )


def typed(db, op="op1", source=(0, 100), state="ready"):
    # SQL-shape fixture only: production must capture/recompute real revisions.
    for kind, id in [
        ("context", "ctx"),
        ("timeline", "timeline"),
        ("version", "version"),
    ]:
        permit(db, kind, id)
    cmd = (CID, "host", op)
    db.execute(
        "INSERT INTO catalog_segmentation_contexts VALUES ('ctx','host','editorial-context',1)"
    )
    db.execute(
        "INSERT INTO catalog_segmentation_revisions VALUES (?,?,?,?,?,?)",
        ("ctx", 1, 10, *cmd),
    )
    db.execute("INSERT INTO catalog_timelines VALUES ('timeline','edition',1)")
    db.execute(
        "INSERT INTO catalog_timeline_revisions VALUES (?,?,?,?,?,?,?,?,?)",
        ("timeline", 1, "provisional", 0, 100, 10, *cmd),
    )
    db.execute("INSERT INTO catalog_versions VALUES ('version','va','ctx',1)")
    db.execute(
        "INSERT INTO catalog_version_revisions VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
        ("version", 1, "timeline", 1, state, 0, 100, None, None, 10, *cmd),
    )
    db.execute(
        "INSERT INTO catalog_exact_file_revisions VALUES ('p','f1','rev1',1,'{}',10)"
    )
    db.execute("INSERT INTO catalog_bindings VALUES ('binding','version','a1')")
    db.execute(
        "INSERT INTO catalog_binding_revisions VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (
            "binding",
            1,
            "p",
            "f1",
            "rev1",
            1,
            "slice",
            source[0],
            source[1],
            0,
            source[1] - source[0],
            None,
            10,
            *cmd,
        ),
    )
    db.execute("INSERT INTO catalog_version_bindings VALUES ('version',1,'binding',1)")
    db.execute(
        "INSERT INTO catalog_segmentation_sets VALUES ('set','ctx','p','f1','rev1')"
    )
    db.execute("INSERT INTO catalog_segment_members VALUES ('ctx',1,'set','binding',1)")


def typed_commit(db):
    db.execute("BEGIN IMMEDIATE")
    typed(db)
    db.execute("DELETE FROM catalog_mutation_permits")
    receipt(db)
    db.execute("COMMIT")


class NativeCatalogSchema(unittest.TestCase):
    def test_additive_preservation_populated_legacy_rows_and_ddl(self):
        db = legacy()
        before = snapshot(db)
        ddl = list(
            db.execute("SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL")
        )
        db.execute("BEGIN")
        script(db, SQL)
        native_settings(db)
        db.execute("COMMIT")
        self.assertEqual(before, snapshot(db, list(before)))
        actual = {
            (t, n): s
            for t, n, s in db.execute(
                "SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL"
            )
        }
        self.assertTrue(all((actual[t, n] == s for t, n, s in ddl)))
        self.assertEqual(generation(db), 0)
        self.assertEqual(
            db.execute("SELECT catalog_id FROM catalog_state").fetchone(), (CID,)
        )
        self.assertEqual(db.execute("PRAGMA integrity_check").fetchone(), ("ok",))
        self.assertFalse(list(db.execute("PRAGMA foreign_key_check")))
        self.assertEqual(
            db.execute("SELECT count(*) FROM catalog_legacy_associations").fetchone(),
            (3,),
        )
        self.assertEqual(
            db.execute(
                "SELECT count(*) FROM catalog_legacy_association_classification WHERE classification='unclassified'"
            ).fetchone(),
            (3,),
        )
        self.assertEqual(
            db.execute(
                "SELECT metadata_raw,provenance FROM catalog_legacy_item_metadata WHERE item_id='va'"
            ).fetchone(),
            ("raw metadata va", "legacy_import"),
        )
        for table in [
            "catalog_timelines",
            "catalog_versions",
            "catalog_bindings",
            "catalog_exact_file_revisions",
            "catalog_identity_evidence",
        ]:
            self.assertEqual(
                db.execute("SELECT count(*) FROM " + table).fetchone(), (0,)
            )

    def test_interrupted_migration_rolls_back_all_ddl_and_rows(self):
        db = legacy()
        before = snapshot(db)
        ddl = list(db.execute("SELECT type,name,sql FROM sqlite_schema"))
        db.execute("BEGIN")
        script(db, SQL)
        db.execute("ROLLBACK")
        self.assertEqual(before, snapshot(db))
        self.assertEqual(
            ddl, list(db.execute("SELECT type,name,sql FROM sqlite_schema"))
        )

    def test_signed_shape_matches_and_legacy_resources_unchanged(self):
        db = upgraded(False)
        actual = [
            [t, n, " ".join(s.split())]
            for t, n, s in db.execute(
                "SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY type,name"
            )
        ]
        self.assertEqual(actual, json.loads((REPO / SIGNATURE_PATH).read_text()))
        manifest = json.loads((ROOT / "preparation-manifest.json").read_text())
        for p, digest in manifest["legacy_resource_sha256"].items():
            self.assertEqual(
                hashlib.sha256((REPO / p).read_bytes()).hexdigest(), digest
            )

    def test_every_mutable_trigger_compiles_in_all_three_forms(self):
        # Zero selected rows compile every trigger body without invented fixtures.
        db = upgraded()
        for row in json.loads(
            (ROOT / "generation-revision-inventory.json").read_text()
        ):
            if row["generation"] == "excluded":
                continue
            table = row["table"]
            cols = list(db.execute('PRAGMA table_info("' + table + '")'))
            column = cols[0][1]
            db.execute(f'INSERT INTO "{table}" SELECT * FROM "{table}" WHERE 0')
            db.execute(f'UPDATE "{table}" SET "{column}"="{column}" WHERE 0')
            db.execute(f'DELETE FROM "{table}" WHERE 0')

    def test_unclassified_deletion_keeps_audit_and_not_live_fk(self):
        db = upgraded()
        db.execute("DELETE FROM item_files WHERE id='au'")
        db.execute("DELETE FROM items WHERE id='free'")
        self.assertEqual(
            db.execute(
                "SELECT classification,legacy_row_present FROM catalog_legacy_association_classification WHERE association_id='au'"
            ).fetchone(),
            ("unclassified", 0),
        )
        self.assertEqual(
            db.execute(
                "SELECT metadata_raw FROM catalog_legacy_item_metadata WHERE item_id='free'"
            ).fetchone(),
            ("raw metadata free",),
        )

    def test_typed_owner_can_mutate_but_legacy_cannot(self):
        db = upgraded()
        typed_commit(db)
        with self.assertRaisesRegex(
            sqlite3.IntegrityError, "catalog_typed_owner_required"
        ):
            db.execute("UPDATE item_files SET metadata='changed' WHERE id='a1'")
        with self.assertRaisesRegex(
            sqlite3.IntegrityError, "catalog_typed_owner_required"
        ):
            db.execute(
                "INSERT INTO item_files(id,file_id,item_id,role,part,origin) VALUES ('new','f3','va','primary',2,'explicit')"
            )
        with self.assertRaisesRegex(
            sqlite3.IntegrityError, "catalog_typed_owner_required"
        ):
            db.execute("UPDATE items SET metadata='changed' WHERE id='va'")
        db.execute("BEGIN")
        permit(db, "version", "version")
        db.execute("UPDATE item_files SET metadata='changed' WHERE id='a1'")
        db.execute("DELETE FROM catalog_mutation_permits")
        db.execute("COMMIT")
        self.assertEqual(
            db.execute("SELECT metadata FROM item_files WHERE id='a1'").fetchone(),
            ("changed",),
        )

    def test_permit_cannot_commit_and_rollback_restores(self):
        db = upgraded()
        typed_commit(db)
        before = snapshot(db)
        db.execute("BEGIN")
        permit(db, "version", "version")
        db.execute("UPDATE item_files SET metadata='changed' WHERE id='a1'")
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute("COMMIT")
        self.assertTrue(db.in_transaction)
        db.execute("ROLLBACK")
        self.assertEqual(before, snapshot(db))

    def test_permit_savepoint_and_stale_incarnation(self):
        db = upgraded()
        db.execute("BEGIN")
        db.execute("SAVEPOINT s")
        permit(db, "version", "new")
        db.execute("ROLLBACK TO s")
        db.execute("RELEASE s")
        db.execute("COMMIT")
        self.assertEqual(
            db.execute("SELECT count(*) FROM catalog_mutation_permits").fetchone(), (0,)
        )
        with self.assertRaisesRegex(
            sqlite3.IntegrityError, "catalog_stale_incarnation"
        ):
            db.execute(
                "INSERT INTO catalog_mutation_permits VALUES ('version','x','stale',1)"
            )

    def test_barrier_cannot_be_satisfied(self):
        db = upgraded()
        with self.assertRaisesRegex(
            sqlite3.IntegrityError, "barrier_must_remain_empty"
        ):
            db.execute("INSERT INTO catalog_commit_barrier VALUES (1)")

    def test_wrong_version_permit_does_not_unlock_claim(self):
        db = upgraded()
        typed_commit(db)
        db.execute("BEGIN")
        permit(db, "version", "other")
        with self.assertRaisesRegex(
            sqlite3.IntegrityError, "catalog_typed_owner_required"
        ):
            db.execute("UPDATE item_files SET active=0 WHERE id='a1'")
        db.execute("ROLLBACK")

    def test_typed_delete_retained_fk_even_with_permit(self):
        db = upgraded()
        typed_commit(db)
        db.execute("BEGIN")
        permit(db, "version", "version")
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute("DELETE FROM item_files WHERE id='a1'")
        db.execute("ROLLBACK")

    def test_claim_insert_replace_cannot_escape_guard(self):
        db = upgraded()
        typed_commit(db)
        with self.assertRaisesRegex(
            sqlite3.IntegrityError, "catalog_typed_owner_required"
        ):
            db.execute(
                "INSERT OR REPLACE INTO item_files(id,file_id,item_id,role,part,origin) VALUES ('a1','f1','free','primary',2,'explicit')"
            )

    def test_update_replace_new_claimed_owner_is_guarded(self):
        db = upgraded()
        typed_commit(db)
        before = snapshot(db)
        with self.assertRaisesRegex(
            sqlite3.IntegrityError, "catalog_typed_owner_required"
        ):
            db.execute("UPDATE OR REPLACE items SET id='va' WHERE id='free'")
        self.assertEqual(before, snapshot(db))

    def test_overflow_explicit_abort_overrides_ignore_fail_and_replace(self):
        for policy in ["IGNORE", "FAIL", "REPLACE"]:
            for sql in [
                f"INSERT OR {policy} INTO items VALUES ('overflow','test','{{}}')",
                f"UPDATE OR {policy} items SET metadata='changed' WHERE id='free'",
            ]:
                db = upgraded()
                db.execute("UPDATE catalog_state SET generation=?", (MAX,))
                before = snapshot(db)
                with self.assertRaisesRegex(
                    sqlite3.IntegrityError, "catalog_generation_exhausted"
                ):
                    db.execute(sql)
                self.assertEqual(before, snapshot(db))

    def test_entity_overflow_explicit_abort_overrides_conflict_policy(self):
        for policy in ["IGNORE", "FAIL", "REPLACE"]:
            db = upgraded()
            guard = db.execute(
                "SELECT sql FROM sqlite_schema WHERE name='catalog_entity_revisions_advance'"
            ).fetchone()[0]
            db.execute("DROP TRIGGER catalog_entity_revisions_advance")
            db.execute(
                "UPDATE catalog_entity_revisions SET revision=? WHERE entity_kind=? AND entity_key=?",
                (MAX, "items", '["free"]'),
            )
            db.execute(guard)
            before = snapshot(db)
            with self.assertRaisesRegex(
                sqlite3.IntegrityError, "catalog_entity_revision_conflict"
            ):
                db.execute(
                    f"UPDATE OR {policy} items SET metadata='changed' WHERE id='free'"
                )
            self.assertEqual(before, snapshot(db))

    def test_meta_uuid_canonicalization_preserves_original_spelling(self):
        for raw in [
            "ABCDEFAB-1234-5678-9012-ABCDEFABCDEF",
            "abcdefab123456789012abcdefabcdef",
            "{abcdefab-1234-5678-9012-abcdefabcdef}",
            "urn:uuid:abcdefab-1234-5678-9012-abcdefabcdef",
        ]:
            db = legacy()
            db.execute("UPDATE meta SET value=? WHERE key='database_id'", (raw,))
            before = snapshot(db)
            db.execute("BEGIN")
            script(db, SQL)
            native_settings(db)
            db.execute("COMMIT")
            self.assertEqual(snapshot(db, list(before)), before)
            self.assertEqual(
                db.execute("SELECT catalog_id FROM catalog_state").fetchone(),
                ("abcdefab-1234-5678-9012-abcdefabcdef",),
            )

    def test_generation_and_revision_noop_delete_reinsert_aba(self):
        db = upgraded()
        g = generation(db)
        db.execute("UPDATE items SET metadata=metadata WHERE id='free'")
        self.assertEqual(generation(db), g)
        db.execute("UPDATE items SET metadata='new' WHERE id='free'")
        self.assertGreater(generation(db), g)
        self.assertEqual(
            db.execute(
                "SELECT revision FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key='[\"free\"]'"
            ).fetchone(),
            (2,),
        )
        db.execute("DELETE FROM item_files WHERE id='au'")
        db.execute("DELETE FROM items WHERE id='free'")
        self.assertEqual(
            db.execute(
                "SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key='[\"free\"]'"
            ).fetchone(),
            (3, 1),
        )
        db.execute("INSERT INTO items VALUES ('free','test','again')")
        self.assertEqual(
            db.execute(
                "SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key='[\"free\"]'"
            ).fetchone(),
            (4, 0),
        )

    def test_native_settings_are_required_and_verified(self):
        db = upgraded()
        self.assertEqual(db.execute("PRAGMA recursive_triggers").fetchone(), (1,))
        self.assertEqual(db.execute("PRAGMA foreign_keys").fetchone(), (1,))
        self.assertEqual(db.execute("PRAGMA read_uncommitted").fetchone(), (0,))

    def test_insert_replace_natural_key_collision_tombstones_removed_identity(self):
        db = upgraded()
        db.execute(
            "INSERT OR REPLACE INTO item_files(id,file_id,item_id,role,part,origin) VALUES ('au2','f4','free','primary',1,'explicit')"
        )
        self.assertEqual(
            db.execute(
                "SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind='item_files' AND entity_key='[\"au\"]'"
            ).fetchone(),
            (2, 1),
        )
        self.assertEqual(
            db.execute(
                "SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind='item_files' AND entity_key='[\"au2\"]'"
            ).fetchone(),
            (1, 0),
        )

    def test_update_replace_natural_key_collision_tombstones_removed_identity(self):
        db = upgraded()
        db.execute(
            "INSERT INTO item_files(id,file_id,item_id,role,part,origin) VALUES ('au2','f3','free','primary',2,'explicit')"
        )
        db.execute(
            "UPDATE OR REPLACE item_files SET file_id='f4',part=1 WHERE id='au2'"
        )
        self.assertEqual(
            db.execute(
                "SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind='item_files' AND entity_key='[\"au\"]'"
            ).fetchone(),
            (2, 1),
        )
        self.assertEqual(
            db.execute(
                "SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind='item_files' AND entity_key='[\"au2\"]'"
            ).fetchone(),
            (2, 0),
        )

    def test_recursive_existing_job_request_event_notification_chain(self):
        db = upgraded()
        db.execute(
            "INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key) VALUES ('job','p','f1','probe','l','{}','{}','cache')"
        )
        db.execute("INSERT INTO api_principals(id,profile) VALUES ('principal','p')")
        db.execute(
            "INSERT INTO api_tokens(id,principal,digest,grants,expires) VALUES ('token','principal','digest','[]',100)"
        )
        db.execute(
            "INSERT INTO api_requests(id,profile,principal,token_id,idempotency_key,body,job_id,reserved_bytes,reservation_device) VALUES ('request','p','principal','token','key','{}','job',100,1)"
        )
        db.execute("INSERT INTO api_reservations VALUES ('job',1,100)")
        db.execute(
            "INSERT INTO notification_destinations(profile,id,credential_env,subscriptions,tags,min_severity) VALUES ('p','dest','ENV','[\"job_completed\"]','[]','info')"
        )
        before = generation(db)
        db.execute("UPDATE processing_jobs SET state='complete' WHERE id='job'")
        self.assertEqual(
            db.execute(
                "SELECT state,reserved_bytes FROM api_requests WHERE id='request'"
            ).fetchone(),
            ("ready", 0),
        )
        self.assertEqual(
            db.execute("SELECT count(*) FROM api_reservations").fetchone(), (0,)
        )
        self.assertEqual(
            db.execute(
                "SELECT count(*) FROM api_events WHERE resource_type='job' AND resource_id='job' AND state='complete'"
            ).fetchone(),
            (1,),
        )
        self.assertEqual(
            db.execute(
                "SELECT count(*) FROM api_events WHERE resource_type='request' AND resource_id='request' AND state='ready'"
            ).fetchone(),
            (1,),
        )
        self.assertEqual(
            db.execute(
                "SELECT count(*) FROM consumer_events WHERE event='job_completed' AND subject='job'"
            ).fetchone(),
            (1,),
        )
        self.assertEqual(
            db.execute("SELECT count(*) FROM notification_deliveries").fetchone(), (1,)
        )
        self.assertGreater(generation(db), before)
        self.assertFalse(list(db.execute("PRAGMA foreign_key_check")))
        db.execute("UPDATE processing_jobs SET state='complete' WHERE id='job'")
        self.assertEqual(
            db.execute(
                "SELECT count(*) FROM consumer_events WHERE event='job_completed' AND subject='job'"
            ).fetchone(),
            (1,),
        )

    def test_primary_key_change_retains_old_revision_tombstone(self):
        db = upgraded()
        db.execute("INSERT INTO profiles VALUES ('before')")
        db.execute("UPDATE profiles SET id='after' WHERE id='before'")
        self.assertEqual(
            db.execute(
                "SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind='profiles' AND entity_key='[\"before\"]'"
            ).fetchone(),
            (2, 1),
        )
        self.assertEqual(
            db.execute(
                "SELECT revision,deleted FROM catalog_entity_revisions WHERE entity_kind='profiles' AND entity_key='[\"after\"]'"
            ).fetchone(),
            (1, 0),
        )

    def test_generation_overflow_aborts_original_mutation(self):
        db = upgraded()
        db.execute("UPDATE catalog_state SET generation=?", (MAX,))
        before = snapshot(db)
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute("INSERT INTO items VALUES ('overflow','test','{}')")
        self.assertEqual(before, snapshot(db))
        self.assertEqual(generation(db), MAX)

    def test_entity_revision_overflow_aborts_original_mutation(self):
        db = upgraded()
        db.execute(
            "INSERT INTO catalog_entity_revisions VALUES (?,?,?,?)",
            ("items", '["overflow"]', MAX, 1),
        )
        before = snapshot(db)
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute("INSERT INTO items VALUES ('overflow','test','{}')")
        self.assertEqual(before, snapshot(db))

    def test_receipt_final_generation_and_permanent_tombstone(self):
        db = upgraded()
        g = generation(db)
        receipt(db)
        self.assertEqual(generation(db), g)
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute("DELETE FROM catalog_command_receipts")
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute("UPDATE catalog_command_receipts SET result_json='{}'")
        db.execute(
            "UPDATE catalog_command_receipts SET result_state='expired',result_json=NULL,expired_at_us=20"
        )
        self.assertEqual(generation(db), g)
        with self.assertRaises(sqlite3.IntegrityError):
            receipt(db)
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute(
                "UPDATE catalog_command_receipts SET result_state='full',result_json='{}',expired_at_us=NULL"
            )
        self.assertEqual(
            db.execute(
                "SELECT payload_digest,result_state,result_json FROM catalog_command_receipts"
            ).fetchone(),
            ("a" * 64, "expired", None),
        )

    def test_receipt_hold_and_retention_prevent_compaction(self):
        db = upgraded()
        receipt(db)
        db.execute(
            "INSERT INTO catalog_receipt_holds VALUES (?,?,?,?,?,?,?)",
            (CID, "host", "op1", "migration", "h1", 10, None),
        )
        g = generation(db)
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute(
                "UPDATE catalog_command_receipts SET result_state='expired',result_json=NULL,expired_at_us=99"
            )
        db.execute("UPDATE catalog_receipt_holds SET released_at_us=15")
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute(
                "UPDATE catalog_command_receipts SET result_state='expired',result_json=NULL,expired_at_us=19"
            )
        db.execute(
            "UPDATE catalog_command_receipts SET result_state='expired',result_json=NULL,expired_at_us=20"
        )
        self.assertEqual(generation(db), g)
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute(
                "INSERT INTO catalog_receipt_holds VALUES (?,?,?,?,?,?,?)",
                (CID, "host", "op1", "migration", "h2", 30, None),
            )

    def test_receipt_generation_must_match_committed_snapshot(self):
        db = upgraded()
        db.execute("INSERT INTO items VALUES ('extra','test','{}')")
        with self.assertRaisesRegex(
            sqlite3.IntegrityError, "catalog_receipt_invalid_commit"
        ):
            db.execute(
                "INSERT INTO catalog_command_receipts VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    CID,
                    "host",
                    "op1",
                    1,
                    1,
                    "a" * 64,
                    incarnation(db),
                    0,
                    0,
                    10,
                    20,
                    "full",
                    "{}",
                    None,
                ),
            )

    def test_history_and_original_mappings_are_immutable(self):
        db = upgraded()
        typed_commit(db)
        for t in [
            "catalog_legacy_associations",
            "catalog_legacy_item_metadata",
            "catalog_exact_file_revisions",
            "catalog_version_revisions",
            "catalog_binding_revisions",
            "catalog_version_bindings",
            "catalog_segment_members",
        ]:
            with self.assertRaisesRegex(
                sqlite3.IntegrityError, "catalog_history_immutable"
            ):
                db.execute("DELETE FROM " + t)

    def test_history_without_receipt_is_uncommittable(self):
        db = upgraded()
        db.execute("BEGIN")
        typed(db)
        db.execute("DELETE FROM catalog_mutation_permits")
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute("COMMIT")
        db.execute("ROLLBACK")
        self.assertEqual(
            db.execute("SELECT count(*) FROM catalog_versions").fetchone(), (0,)
        )

    def test_sealed_version_membership_cannot_be_extended(self):
        db = upgraded()
        typed_commit(db)
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute(
                "INSERT INTO catalog_version_bindings VALUES ('version',1,'binding',1)"
            )

    def test_segmentation_exact_revision_unique_context(self):
        db = upgraded()
        typed_commit(db)
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute(
                "INSERT INTO catalog_segmentation_sets VALUES ('renamed','ctx','p','f1','rev1')"
            )

    def test_segmentation_overlap_and_nonoverlap(self):
        db = upgraded()
        db.execute("BEGIN")
        typed(db)
        permit(db, "version", "other-version")
        db.execute("UPDATE item_files SET file_id='f1' WHERE id='a2'")
        db.execute("INSERT INTO catalog_versions VALUES ('other-version','vb','ctx',1)")
        db.execute(
            "INSERT INTO catalog_version_revisions VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                "other-version",
                1,
                "timeline",
                1,
                "ready",
                0,
                100,
                None,
                None,
                10,
                CID,
                "host",
                "op1",
            ),
        )
        db.execute(
            "INSERT INTO catalog_bindings VALUES ('other-binding','other-version','a2')"
        )
        db.execute(
            "INSERT INTO catalog_binding_revisions VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                "other-binding",
                1,
                "p",
                "f1",
                "rev1",
                1,
                "slice",
                50,
                150,
                0,
                100,
                None,
                10,
                CID,
                "host",
                "op1",
            ),
        )
        with self.assertRaisesRegex(sqlite3.IntegrityError, "overlapping_coverage"):
            db.execute(
                "INSERT INTO catalog_segment_members VALUES ('ctx',1,'set','other-binding',1)"
            )
        db.execute(
            "INSERT INTO catalog_binding_revisions VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                "other-binding",
                2,
                "p",
                "f1",
                "rev1",
                1,
                "slice",
                100,
                200,
                0,
                100,
                None,
                10,
                CID,
                "host",
                "op1",
            ),
        )
        db.execute(
            "INSERT INTO catalog_segment_members VALUES ('ctx',1,'set','other-binding',2)"
        )
        db.execute("ROLLBACK")

    def test_integer_slice_and_duration_checks(self):
        for source, timeline, part in [
            ((0, 0), (0, 0), 1),
            ((-1, 10), (0, 11), 1),
            ((0, 100), (0, 101), 1),
            ((0, 100), (0, 100), 0),
            ((0, 100), (0, 100), 4294967296),
            ((0, 100.5), (0, 100.5), 1),
        ]:
            db = upgraded()
            db.execute("BEGIN")
            typed(db)
            with self.assertRaises(sqlite3.IntegrityError):
                db.execute(
                    "INSERT INTO catalog_binding_revisions VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                    (
                        "binding",
                        2,
                        "p",
                        "f1",
                        "rev1",
                        part,
                        "slice",
                        *source,
                        *timeline,
                        None,
                        10,
                        CID,
                        "host",
                        "op1",
                    ),
                )
            db.execute("ROLLBACK")

    def test_insert_replace_cannot_rewrite_permanent_receipt_or_history(self):
        db = upgraded()
        typed_commit(db)
        for t in [
            "catalog_command_receipts",
            "catalog_legacy_associations",
            "catalog_legacy_item_metadata",
            "catalog_exact_file_revisions",
            "catalog_version_revisions",
            "catalog_binding_revisions",
            "catalog_version_bindings",
            "catalog_segment_members",
        ]:
            with self.assertRaises(sqlite3.IntegrityError):
                db.execute("INSERT OR REPLACE INTO " + t + " SELECT * FROM " + t)

    def test_state_incarnation_is_canonical_128_bit_hex(self):
        db = upgraded()
        db.execute("BEGIN")
        permit(db, "restore", CID)
        for value in ["x", "A" * 32, "a" * 31, "a" * 33, "g" * 32]:
            with self.assertRaises(sqlite3.IntegrityError):
                db.execute("UPDATE catalog_state SET incarnation=?", (value,))
        db.execute("ROLLBACK")

    def test_live_binding_view_exposes_slice_and_owner(self):
        db = upgraded()
        typed_commit(db)
        self.assertEqual(
            db.execute(
                "SELECT association_id,source_kind,source_start_us,source_end_us,version_id FROM catalog_active_binding_revisions"
            ).fetchone(),
            ("a1", "slice", 0, 100, "version"),
        )

    def test_incarnation_requires_restore_scope_and_open_does_not_rotate(self):
        db = upgraded()
        old = incarnation(db)
        with self.assertRaisesRegex(
            sqlite3.IntegrityError, "catalog_state_immutable_or_stale"
        ):
            db.execute(
                "UPDATE catalog_state SET incarnation='bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'"
            )
        self.assertEqual(incarnation(db), old)
        db.execute("BEGIN")
        permit(db, "restore", CID)
        db.execute(
            "UPDATE catalog_state SET incarnation='bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'"
        )
        db.execute("DELETE FROM catalog_mutation_permits")
        db.execute("COMMIT")
        self.assertEqual(incarnation(db), "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")


if __name__ == "__main__":
    unittest.main(verbosity=2)
