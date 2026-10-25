//! Strict current library producer, schema, sources, history and projection.
//! Returns the observed item head only. It cannot establish an Activate output
//! baseline; callers needing that must compare separately authenticated evidence.
use super::*;

pub(super) fn validate_current_in(
    store: &Store,
    subject: &str,
    item: &crate::catalog_import::ImportItem,
    metadata: &crate::catalog_import_typed::ImportedMetadata,
    activation: &str,
    policies: &Value,
    admitted: i64,
    budget: &mut impl StructuralReadBudget,
) -> Result<AuthenticatedImportedLibrary> {
    require(
        item.source_id == subject
            && item.kind == "custom:motion_library"
            && item.legacy_metadata == json!({})
            && metadata.contract_version == 2
            && metadata.system_fields == ["motion_library"]
            && metadata.policies == *policies
            && metadata.contributions.len() == 1,
        "library producer shape differs",
    )?;
    require(
        *policies
            == json!([
                {"field":"title","value_type":"json","nullable":false,"normalization":"exact"},
                {"field":"motion_library","value_type":"json","nullable":false,"normalization":"exact"}
            ]),
        "library field policy differs",
    )?;
    let contribution = &metadata.contributions[0];
    require(
        contribution.namespace == "motion.structure"
            && contribution.record_id == "source_library"
            && contribution.kind == crate::catalog_import_typed::ImportedSourceKind::Scan,
        "library contribution owner differs",
    )?;
    let fields = &contribution.fields;
    let definition = fields
        .get("motion_library")
        .ok_or_else(|| fail("library definition absent"))?;
    let id = definition
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| fail("library ID absent"))?;
    require(
        subject.strip_prefix("motion.library:") == Some(id)
            && definition["contract_version"] == 1
            && definition["catalog_revision"].as_i64() == Some(contribution.source_revision)
            && fields.as_object().is_some_and(|o| o.len() == 2)
            && fields["title"] == definition["name"]
            && contribution.evidence["source_table"] == "catalog_libraries"
            && contribution.evidence["source_row"]["id"] == id
            && contribution.evidence["source_row"]["revision"] == definition["catalog_revision"],
        "library definition producer differs",
    )?;
    require(
        definition.as_object().is_some_and(|o| o.len() == 7)
            && definition["name"].is_string()
            && matches!(
                definition["kind"].as_str(),
                Some("movies" | "television" | "music" | "photos" | "personal_video" | "mixed")
            )
            && contribution.source_revision >= 1
            && !id.is_empty()
            && contribution.evidence["source_row"]
                .as_object()
                .is_some_and(|o| o.len() == 5)
            && definition["language"].is_string()
            && definition["name"] == contribution.evidence["source_row"]["name"]
            && definition["kind"] == contribution.evidence["source_row"]["kind"]
            && definition["language"] == contribution.evidence["source_row"]["language"]
            && contribution
                .evidence
                .as_object()
                .is_some_and(|o| o.len() == 3)
            && contribution.evidence["meaning"]
                == "exact definition; source_ids do not grant access or native query authority",
        "library definition shape differs",
    )?;
    let ids = definition["source_ids"]
        .as_array()
        .ok_or_else(|| fail("library source IDs missing"))?;
    let mut previous: Option<&str> = None;
    for id in ids {
        let id = id
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| fail("invalid library source ID"))?;
        charge(budget, id.len())?;
        require(
            previous.is_none_or(|p| p < id),
            "library source IDs are not strictly ordered",
        )?;
        previous = Some(id);
    }
    let snapshot = crate::catalog_metadata_store::read_refresh_metadata(store, subject)
        .map_err(|e| fail(&e.to_string()))?;
    require(
        snapshot.schema.contract_version() == 2
            && snapshot
                .schema
                .system_fields()
                .iter()
                .map(String::as_str)
                .eq(["motion_library"])
            && snapshot.documents.len() == 2,
        "current schema or contributors differ",
    )?;
    let expected_evidence = json!({"import_operation_id":activation,"source_revision":contribution.source_revision,"source_evidence":contribution.evidence});
    for document in &snapshot.documents {
        require(document.revision() == 1, "updated library is unsupported")?;
        let legacy = document.key().namespace() == crate::catalog_metadata_store::LEGACY_NAMESPACE;
        if legacy {
            require(
                document.key().record_id() == crate::catalog_metadata_store::LEGACY_RECORD_ID
                    && document.fields().is_empty()
                    && document.evidence()
                        == &json!({"origin":"unknown","import":"items.metadata"}),
                "legacy library baseline differs",
            )?;
        } else {
            require(
                document.key().namespace() == "motion.structure"
                    && document.key().record_id() == "source_library"
                    && document.kind() == catabolic_core::catalog_metadata::SourceKind::Scan
                    && serde_json::to_value(document.fields())
                        .map_err(|_| fail("fields encode"))?
                        == *fields
                    && document.evidence() == &expected_evidence,
                "current library source differs",
            )?;
        }
    }
    let mut schema_statement = prepare(
        store,
        "SELECT schema_fingerprint,contract_version,policies_json,system_fields_json,prior_fingerprint,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us FROM main.catalog_metadata_schemas WHERE item_id=?",
        budget,
    )?;
    let mut schema_rows = schema_statement.query([subject])?;
    let schema_row = schema_rows.next()?.ok_or_else(|| fail("schema absent"))?;
    row_charge(schema_row, budget)?;
    require(
        text(schema_row, 0, budget)? == snapshot.schema.fingerprint()
            && schema_row.get::<_, i64>(1)? == 2
            && value(schema_row, 2, budget)? == *policies
            && value(schema_row, 3, budget)? == json!(["motion_library"])
            && matches!(schema_row.get_ref(4)?, ValueRef::Null)
            && text(schema_row, 5, budget)? == store.database_id
            && text(schema_row, 6, budget)? == HOST
            && text(schema_row, 7, budget)? == activation
            && schema_row.get::<_, i64>(8)? == admitted,
        "schema initialization lineage differs",
    )?;
    require(
        schema_rows.next()?.is_none(),
        "schema evolution is unsupported",
    )?;
    let resolved =
        catabolic_core::catalog_metadata::resolve(&snapshot.schema, subject, &snapshot.documents)
            .map_err(|_| fail("library resolution refused"))?;
    let mut projection_statement = prepare(
        store,
        "SELECT resolution_json FROM main.catalog_metadata_projection WHERE item_id=?",
        budget,
    )?;
    let mut projection_rows = projection_statement.query([subject])?;
    let projection_row = projection_rows
        .next()?
        .ok_or_else(|| fail("projection absent"))?;
    row_charge(projection_row, budget)?;
    let actual_resolution = value(projection_row, 0, budget)?;
    require(
        actual_resolution
            == serde_json::to_value(&resolved).map_err(|_| fail("resolution encoding failed"))?,
        "full library projection differs",
    )?;
    charge(budget, 3 * 32)?;
    let counts: (i64,i64,i64) = prepare(store,"SELECT (SELECT count(*) FROM main.catalog_metadata_history WHERE item_id=?1),(SELECT count(*) FROM main.catalog_metadata_schemas WHERE item_id=?1),(SELECT count(*) FROM main.catalog_metadata_schema_heads WHERE item_id=?1)",budget)?.query_row([subject], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    require(counts == (2, 1, 1), "extra library lineage is unsupported")?;
    let mut history = prepare(
        store,
        "SELECT namespace,record_id,revision,source_kind,schema_fingerprint,fields_json,evidence_json,raw_mutation_json,command_catalog_id,command_host_namespace,command_operation_id,admitted_at_us FROM main.catalog_metadata_history WHERE item_id=? ORDER BY namespace,record_id",
        budget,
    )?;
    let mut history_rows = history.query([subject])?;
    while let Some(row) = history_rows.next()? {
        row_charge(row, budget)?;
        let namespace = text(row, 0, budget)?;
        let legacy = namespace == crate::catalog_metadata_store::LEGACY_NAMESPACE;
        require(
            (legacy || namespace == "motion.structure")
                && text(row, 1, budget)?
                    == if legacy {
                        crate::catalog_metadata_store::LEGACY_RECORD_ID
                    } else {
                        "source_library"
                    }
                && row.get::<_, i64>(2)? == 1
                && text(row, 3, budget)? == if legacy { "legacy_import" } else { "scan" }
                && text(row, 4, budget)? == snapshot.schema.fingerprint()
                && text(row, 8, budget)? == store.database_id
                && text(row, 9, budget)? == HOST
                && text(row, 10, budget)? == activation
                && row.get::<_, i64>(11)? == admitted,
            "history identity differs",
        )?;
        require(
            value(row, 5, budget)? == if legacy { json!({}) } else { fields.clone() }
                && value(row, 6, budget)?
                    == if legacy {
                        json!({"origin":"unknown","import":"items.metadata"})
                    } else {
                        expected_evidence.clone()
                    },
            "history contents differ",
        )?;
        let expected_raw = if legacy {
            json!({"contract_version":2,"subject":subject,"policies":policies,"system_fields":["motion_library"]})
        } else {
            json!({"operation":"metadata_import_source_v2","subject":subject,"namespace":"motion.structure","record_id":"source_library","fields":fields,"evidence":expected_evidence})
        };
        require(
            value(row, 7, budget)? == expected_raw,
            "history mutation differs",
        )?;
    }
    let mut current_statement = prepare(
        store,
        "SELECT i.kind,h.revision FROM main.items i JOIN main.catalog_entity_revisions h ON h.entity_kind='items' AND h.entity_key=json_array(i.id) AND h.deleted=0 WHERE i.id=?",
        budget,
    )?;
    let mut current_rows = current_statement.query([subject])?;
    let row = current_rows
        .next()?
        .ok_or_else(|| fail("current library item absent"))?;
    row_charge(row, budget)?;
    let current = (text(row, 0, budget)?, row.get::<_, i64>(1)?);
    // Finish comparisons while this row is borrowed. Only scalar facts survive
    // the cardinality sentinel; no owned copy or proof escapes before it.
    let matches = current.0 == "custom:motion_library" && current.1 > 0;
    let item_revision = current.1;
    require(
        current_rows.next()?.is_none(),
        "duplicate current library item head",
    )?;
    require(matches, "current library item head is invalid")?;
    store
        .catalog_operation_budget()?
        .check()
        .map_err(|_| fail("terminal operation budget refused"))?;
    Ok(AuthenticatedImportedLibrary {
        definition: definition.clone(),
        item_revision,
    })
}
