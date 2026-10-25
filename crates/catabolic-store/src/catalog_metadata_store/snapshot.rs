//! Complete, bounded source snapshots for independently authorized host adapters.
//! Empty and shadowed documents remain visible; no import evidence is consulted
//! as a current value, tag assertion, or revision authority.
use super::*;

impl From<crate::Error> for MetadataStoreError {
    fn from(error: crate::Error) -> Self {
        Self {
            code: MetadataStoreErrorCode::StoreFailure,
            error,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceSnapshot {
    pub namespace: String,
    pub record_id: String,
    pub revision: i64,
    pub fields: std::collections::BTreeMap<String, Value>,
    pub tags: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ContributionSnapshot {
    pub subject: String,
    pub generation: i64,
    pub schema_fingerprint: Option<String>,
    pub metadata: Value,
    pub sources: Vec<SourceSnapshot>,
    pub excluded_tags: Vec<String>,
}

pub fn read_contributions(
    store: &crate::Store,
    authority: &MetadataReadAuthority,
) -> Result<ContributionSnapshot> {
    if store.schema_version < 34 || store.catalog_incarnation.is_none() {
        return Err(fail(
            MetadataStoreErrorCode::UnsupportedContract,
            "native metadata snapshot requires native34",
        ));
    }
    store
        .transaction_available()
        .map_err(|error| MetadataStoreError {
            code: MetadataStoreErrorCode::StoreFailure,
            error,
        })?;
    let db = &store.db;
    if !db.is_autocommit() {
        return Err(fail(
            MetadataStoreErrorCode::AmbientReadTransaction,
            "metadata snapshot requires fresh admission",
        ));
    }
    let snapshot = store.transaction_detailed::<_, MetadataStoreError>(|db| {
        let budget = store.catalog_operation_budget()?;
        let charge = |bytes: usize, values: usize| {
            budget.charge(crate::operation_budget::BudgetCharge { bytes, values, work: bytes / 64 + values + 1, ..Default::default() })
                .map_err(|_| fail(MetadataStoreErrorCode::StoreFailure, "metadata snapshot operation budget exhausted"))
        };
        check_read_admission(db, authority)?;
        let raw: Option<String> = db
            .query_row(
                "SELECT metadata FROM items WHERE id=? AND length(CAST(metadata AS BLOB))<=?",
                params![authority.subject, MAX_METADATA_SNAPSHOT_BYTES as i64],
                |r| r.get(0),
            )
            .optional()?;
        let raw = raw.ok_or_else(|| {
            fail(
                MetadataStoreErrorCode::SubjectMissing,
                "missing or oversized metadata subject",
            )
        })?;
        charge(raw.len(), 1)?;
        if db
            .prepare("SELECT 1 FROM catalog_logical_retirements WHERE item_id=?")?
            .exists([&authority.subject])?
        {
            return Err(fail(
                MetadataStoreErrorCode::RetiredSubject,
                "retired metadata subject",
            ));
        }
        let fingerprint = schema_registry::current_fingerprint(db, &authority.subject)?;
        let mut sources = Vec::new();
        if fingerprint.is_some() {
            schema_registry::charge_retained_schemas(db,&budget,&authority.subject)?;
            let (source_bytes, count): (i64, i64) = db.query_row("SELECT coalesce(sum(length(CAST(fields_json AS BLOB))+length(CAST(evidence_json AS BLOB))),0),count(*) FROM catalog_metadata_sources WHERE item_id=?", [&authority.subject], |r| Ok((r.get(0)?,r.get(1)?)))?;
            let schema = read_schema(db, &authority.subject)?;
            let fields: usize = if schema.contract_version() == 1 {
                usize::try_from(count).map_err(|_| corrupt("negative source count"))? * catabolic_core::catalog_metadata::MAX_METADATA_FIELDS
            } else {
                let n: i64 = db.query_row("SELECT count(*) FROM catalog_metadata_sources s,json_each(s.fields_json) WHERE s.item_id=?", [&authority.subject], |r|r.get(0))?;
                usize::try_from(n).map_err(|_| corrupt("negative field count"))? + schema.fields().len()
            };
            charge(usize::try_from(source_bytes).map_err(|_| corrupt("negative snapshot size"))?, fields)?;
            let documents = read_documents(db, &schema, &authority.subject)?;
            // Exercise the same resolver and its complete-input bounds even
            // though this snapshot exposes attributed inputs, not only winners.
            resolve(&schema, &authority.subject, &documents)?;
            for document in documents {
                let mut query = db.prepare("SELECT t.name FROM item_tags a JOIN tags t ON t.id=a.tag_id WHERE a.item_id=? AND a.source=? AND a.active=1 ORDER BY t.name LIMIT 101")?;
                let tags = query
                    .query_map(
                        params![authority.subject, document.key().namespace()],
                        |r| r.get::<_, String>(0),
                    )?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                if tags.len() > 100 || tags.iter().any(|s| s.len() > 100) {
                    return Err(fail(
                        MetadataStoreErrorCode::InvalidInput,
                        "source tag snapshot exceeds adapter contract",
                    ));
                }
                charge(tags.iter().map(String::len).sum(), tags.len())?;
                sources.push(SourceSnapshot {
                    namespace: document.key().namespace().into(),
                    record_id: document.key().record_id().into(),
                    revision: document.revision(),
                    fields: document.fields().clone(),
                    tags,
                });
            }
        }
        let mut query = db.prepare("SELECT t.name FROM catalog_item_tag_suppressions s JOIN catalog_tag_suppression_revisions h ON h.subject_type=s.subject_type AND h.subject_id=s.item_id AND h.tag_id=s.tag_id AND h.revision=s.revision JOIN tags t ON t.id=s.tag_id WHERE s.item_id=? AND h.suppressed=1 ORDER BY t.name LIMIT 101")?;
        let excluded_tags = query
            .query_map([&authority.subject], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if excluded_tags.len() > 100 || excluded_tags.iter().any(|s| s.len() > 100) {
            return Err(fail(
                MetadataStoreErrorCode::InvalidInput,
                "tag suppression snapshot exceeds adapter contract",
            ));
        }
        charge(excluded_tags.iter().map(String::len).sum(), excluded_tags.len())?;
        check_read_admission(db, authority)?;
        let snapshot = ContributionSnapshot {
            subject: authority.subject.clone(),
            generation: authority.expected_generation,
            schema_fingerprint: fingerprint,
            metadata: decode_json(&raw).map_err(|_| corrupt("invalid native item metadata"))?,
            sources,
            excluded_tags,
        };
        if serde_json::to_vec(&snapshot)?.len() > MAX_METADATA_SNAPSHOT_BYTES {
            return Err(MetadataError::SnapshotBudget.into());
        }
        Ok(snapshot)
    })?;
    check_read_admission(&store.db, authority)?;
    Ok(snapshot)
}
