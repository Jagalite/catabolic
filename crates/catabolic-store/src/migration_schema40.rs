//! Exact native40 append-only invalidation baseline proof. This does not relax
//! preservation for any other migration, table, entity kind, or existing row.
use super::*;

pub(super) fn preservation_schema40(db: &Connection, before: &Value) -> Result<()> {
    for table in [
        "catalog_metadata_schemas",
        "catalog_metadata_schema_heads",
        "catalog_metadata_tag_suppressions",
        "catalog_metadata_tag_history",
    ] {
        if before.get(table).is_some() {
            return Err(Error(
                "schema40 metadata registry must be newly introduced".into(),
            ));
        }
    }
    let mut unchanged = before.clone();
    let expected = unchanged
        .as_object_mut()
        .expect("snapshot object")
        .remove("catalog_entity_revisions")
        .ok_or_else(|| Error("schema40 requires native entity index".into()))?;
    preservation(db, &unchanged)?;
    let columns = expected["columns"]
        .as_array()
        .expect("snapshot columns")
        .iter()
        .map(|v| v.as_str().expect("column name").to_owned())
        .collect::<Vec<_>>();
    if digest_table_where(
        db,
        "catalog_entity_revisions",
        &columns,
        "WHERE entity_kind NOT IN ('catalog_metadata_schemas','catalog_metadata_schema_heads','catalog_metadata_tag_suppressions')",
    )? != expected
    {
        return Err(Error("schema40 changed existing entity index rows".into()));
    }
    let invalid:bool=db.query_row(r#"
    WITH expected(entity_kind,entity_key,revision,deleted) AS (
      SELECT 'catalog_metadata_schemas',json_array(item_id,schema_fingerprint),1,0 FROM catalog_metadata_subjects
      UNION ALL SELECT 'catalog_metadata_schema_heads',json_array(item_id),1,0 FROM catalog_metadata_subjects
      UNION ALL SELECT 'catalog_metadata_tag_suppressions',json_array(s.item_id,s.tag_id),1,0
      FROM catalog_item_tag_suppressions s
      JOIN catalog_tag_suppression_revisions h ON h.subject_type='item' AND h.subject_id=s.item_id AND h.tag_id=s.tag_id AND h.revision=s.revision
      JOIN catalog_metadata_sources m ON m.item_id=s.item_id AND m.namespace='motion.local' AND m.record_id='document'
      WHERE json_valid(h.reason) AND json_extract(h.reason,'$.import_operation_id') IS NOT NULL
      AND json_extract(h.reason,'$.import_operation_id')=json_extract(m.evidence_json,'$.import_operation_id')
      AND json_extract(h.reason,'$.source_revision')=json_extract(m.evidence_json,'$.source_revision')
    ), actual AS (
      SELECT entity_kind,entity_key,revision,deleted FROM catalog_entity_revisions
      WHERE entity_kind IN ('catalog_metadata_schemas','catalog_metadata_schema_heads','catalog_metadata_tag_suppressions')
    ) SELECT EXISTS(SELECT * FROM actual EXCEPT SELECT * FROM expected)
      OR EXISTS(SELECT * FROM expected EXCEPT SELECT * FROM actual)
      OR EXISTS(SELECT item_id,schema_fingerprint,contract_version,policies_json,system_fields_json,prior_fingerprint,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us FROM catalog_metadata_schemas
        EXCEPT SELECT item_id,schema_fingerprint,contract_version,policies_json,'[]',NULL,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us FROM catalog_metadata_subjects)
      OR EXISTS(SELECT item_id,schema_fingerprint,contract_version,policies_json,'[]',NULL,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us FROM catalog_metadata_subjects
        EXCEPT SELECT item_id,schema_fingerprint,contract_version,policies_json,system_fields_json,prior_fingerprint,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us FROM catalog_metadata_schemas)
      OR EXISTS(SELECT item_id,schema_fingerprint FROM catalog_metadata_schema_heads EXCEPT SELECT item_id,schema_fingerprint FROM catalog_metadata_subjects)
      OR EXISTS(SELECT item_id,schema_fingerprint FROM catalog_metadata_subjects EXCEPT SELECT item_id,schema_fingerprint FROM catalog_metadata_schema_heads)
      OR EXISTS(SELECT 1 FROM catalog_metadata_tag_history)
      OR EXISTS(SELECT item_id,tag_id,namespace,revision FROM catalog_metadata_tag_suppressions EXCEPT
        SELECT s.item_id,s.tag_id,'motion.local',s.revision FROM catalog_item_tag_suppressions s
        JOIN catalog_tag_suppression_revisions h ON h.subject_type='item' AND h.subject_id=s.item_id AND h.tag_id=s.tag_id AND h.revision=s.revision
        JOIN catalog_metadata_sources m ON m.item_id=s.item_id AND m.namespace='motion.local' AND m.record_id='document'
        WHERE json_valid(h.reason) AND json_extract(h.reason,'$.import_operation_id') IS NOT NULL
        AND json_extract(h.reason,'$.import_operation_id')=json_extract(m.evidence_json,'$.import_operation_id')
        AND json_extract(h.reason,'$.source_revision')=json_extract(m.evidence_json,'$.source_revision'))
      OR EXISTS(SELECT s.item_id,s.tag_id,'motion.local',s.revision FROM catalog_item_tag_suppressions s
        JOIN catalog_tag_suppression_revisions h ON h.subject_type='item' AND h.subject_id=s.item_id AND h.tag_id=s.tag_id AND h.revision=s.revision
        JOIN catalog_metadata_sources m ON m.item_id=s.item_id AND m.namespace='motion.local' AND m.record_id='document'
        WHERE json_valid(h.reason) AND json_extract(h.reason,'$.import_operation_id') IS NOT NULL
        AND json_extract(h.reason,'$.import_operation_id')=json_extract(m.evidence_json,'$.import_operation_id')
        AND json_extract(h.reason,'$.source_revision')=json_extract(m.evidence_json,'$.source_revision')
        EXCEPT SELECT item_id,tag_id,namespace,revision FROM catalog_metadata_tag_suppressions)
    "#,[],|r|r.get(0))?;
    if invalid {
        return Err(Error(
            "schema40 registry/entity baseline is not exact".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Connection, Value) {
        use crate::{
            Store,
            catalog_commands::{self, CatalogAuthority, ReceiptPolicy, TrustedCommandAuthority},
            catalog_metadata_store::{MetadataAccess, MetadataAuthority},
        };
        use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("schema40.db");
        initialize_native_at(&path, 39).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        crate::curation::item_put(
            &store,
            &json!({"id":"item","kind":"video","metadata":{"title":"retained"}}),
        )
        .unwrap();
        let generation: i64 = store
            .db
            .query_row("SELECT generation FROM catalog_state", [], |r| r.get(0))
            .unwrap();
        let payload = json!({"operation":"metadata","mutation":{"contract_version":1,"change":{"action":"initialize","subject":"item","policies":[{"field":"title","value_type":"string","nullable":false,"normalization":"exact"}]}}});
        let command = CommandEnvelope::new(
            1,
            CommandKey::new(&store.database_id, "fixture", "initialize").unwrap(),
            store.catalog_incarnation.as_deref().unwrap(),
            generation,
            &payload.to_string(),
        )
        .unwrap();
        let grant = MetadataAuthority {
            host_namespace: "fixture".into(),
            subject: "item".into(),
            access: MetadataAccess::Initialize,
        };
        catalog_commands::execute_with_authority(
            &store,
            &command,
            CatalogAuthority::new(TrustedCommandAuthority {
                host_namespace: "fixture",
                receipt_visibility: ReceiptVisibility::FullResult,
                allow_new_commands: true,
                identity_authority: None,
            })
            .with_metadata(&grant),
            ReceiptPolicy {
                retention: std::time::Duration::from_secs(3600),
                max_receipts: 100,
                max_logical_bytes: 1024 * 1024,
                max_hold_records: 100,
            },
        )
        .unwrap();
        drop(store);
        let db = connect(&path, true).unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        let before = snapshot(&db).unwrap();
        execute(&db, &steps(40).unwrap()[39]).unwrap();
        preservation_schema40(&db, &before).unwrap();
        (root, db, before)
    }
    #[test]
    fn schema40_preservation_rejects_extra_missing_changed_entity_rows_and_registry() {
        for sql in [
            "INSERT INTO catalog_entity_revisions VALUES('catalog_metadata_schema_heads','[\"phantom\"]',1,0)",
            "UPDATE catalog_entity_revisions SET revision=revision+1 WHERE entity_kind='items'",
            "UPDATE catalog_entity_revisions SET revision=2 WHERE entity_kind='catalog_metadata_schemas'",
            "DROP TRIGGER catalog_entity_revisions_no_delete; DELETE FROM catalog_entity_revisions WHERE entity_kind='catalog_metadata_schema_heads'",
            "DROP TRIGGER catalog_metadata_schema_update; UPDATE catalog_metadata_schemas SET system_fields_json='[\"spoof\"]'",
            "DROP TRIGGER catalog_metadata_schema_head_delete; DELETE FROM catalog_metadata_schema_heads",
            "INSERT INTO catalog_entity_revisions VALUES('unexpected-kind','[\"phantom\"]',1,0)",
        ] {
            let (_root, db, before) = fixture();
            db.execute_batch(sql).unwrap();
            assert!(preservation_schema40(&db, &before).is_err(), "{sql}");
        }
    }
}
