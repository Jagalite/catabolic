//! Versioned schema lookup. Original subjects/history remain immutable; current
//! documents are revalidated in memory under a compatible append-only union.
use super::*;
use std::collections::BTreeSet;

pub(super) fn installed(db: &Connection) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='catalog_metadata_schemas')", [], |r| r.get(0))?)
}

pub(crate) fn current_fingerprint(db: &Connection, subject: &str) -> Result<Option<String>> {
    let table = if installed(db)? {
        "catalog_metadata_schema_heads"
    } else {
        "catalog_metadata_subjects"
    };
    Ok(db
        .query_row(
            &format!("SELECT schema_fingerprint FROM {table} WHERE item_id=?"),
            [subject],
            |r| r.get(0),
        )
        .optional()?)
}

pub(crate) fn read_registered(
    db: &Connection,
    subject: &str,
    fingerprint: &str,
) -> Result<FieldSchema> {
    if !installed(db)? {
        let (stored, raw): (String,String) = db.query_row("SELECT schema_fingerprint,policies_json FROM catalog_metadata_subjects WHERE item_id=?", [subject], |r| Ok((r.get(0)?,r.get(1)?)))?;
        if stored != fingerprint {
            return Err(corrupt("missing retained schema fingerprint"));
        }
        let policies: Vec<PolicyField> = serde_json::from_str(&raw)?;
        let schema = field_schema(&policies)?;
        if schema.fingerprint() != fingerprint {
            return Err(corrupt("retained metadata schema fingerprint mismatch"));
        }
        return Ok(schema);
    }
    let (version, raw, system): (i64,String,String) = db.query_row("SELECT contract_version,policies_json,system_fields_json FROM catalog_metadata_schemas WHERE item_id=? AND schema_fingerprint=? AND length(CAST(policies_json AS BLOB))<=?", params![subject,fingerprint,MAX_METADATA_SNAPSHOT_BYTES as i64], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    registered_from_parts(version, fingerprint, &raw, &system)
}

/// Decode the exact retained representation without requiring another owned raw
/// copy. This parser establishes shape/fingerprint only, never ancestry authority.
pub(super) fn registered_from_parts(
    version: i64,
    fingerprint: &str,
    raw: &str,
    system: &str,
) -> Result<FieldSchema> {
    let policies: Vec<PolicyField> = serde_json::from_str(raw)?;
    let systems: BTreeSet<String> = serde_json::from_str(system)?;
    registered_from_policies(version, fingerprint, policies, systems)
}

/// Shared constructor after a caller has admitted and decoded borrowed parts.
/// Keep policy conversion and fingerprint semantics identical to read_registered.
pub(super) fn registered_from_policies(
    version: i64,
    fingerprint: &str,
    policies: Vec<PolicyField>,
    systems: BTreeSet<String>,
) -> Result<FieldSchema> {
    let schema = match version {
        1 if systems.is_empty() => field_schema(&policies)?,
        2 => {
            let mut external = Vec::new();
            let mut internal = Vec::new();
            for policy in policies {
                let field = policy.field.clone();
                let value = convert_policy(&policy);
                if systems.contains(&field) {
                    internal.push((field, value));
                } else {
                    external.push((field, value));
                }
            }
            if internal.len() != systems.len() {
                return Err(corrupt("registered system policy absent"));
            }
            FieldSchema::new_v2(external, internal)?
        }
        _ => return Err(corrupt("unknown registered schema contract")),
    };
    if schema.fingerprint() != fingerprint {
        return Err(corrupt("registered metadata schema fingerprint mismatch"));
    }
    Ok(schema)
}

pub(super) fn convert_policy(field: &PolicyField) -> FieldPolicy {
    FieldPolicy {
        value_type: match field.value_type {
            PolicyValueType::String => ValueType::String,
            PolicyValueType::Boolean => ValueType::Boolean,
            PolicyValueType::Integer => ValueType::Integer,
            PolicyValueType::Json => ValueType::Json,
        },
        nullable: field.nullable,
        normalization: match field.normalization {
            PolicyNormalization::Exact => Normalization::Exact,
            PolicyNormalization::TrimString => Normalization::TrimString,
            PolicyNormalization::TrimAsciiLowercase => Normalization::TrimAsciiLowercase,
        },
    }
}

pub(super) fn policies(schema: &FieldSchema) -> Vec<PolicyField> {
    schema
        .fields()
        .iter()
        .map(|(field, p)| PolicyField {
            field: field.clone(),
            nullable: p.nullable,
            value_type: match p.value_type {
                ValueType::String => PolicyValueType::String,
                ValueType::Boolean => PolicyValueType::Boolean,
                ValueType::Integer => PolicyValueType::Integer,
                ValueType::Json => PolicyValueType::Json,
            },
            normalization: match p.normalization {
                Normalization::Exact => PolicyNormalization::Exact,
                Normalization::TrimString => PolicyNormalization::TrimString,
                Normalization::TrimAsciiLowercase => PolicyNormalization::TrimAsciiLowercase,
            },
        })
        .collect()
}

/// Validate the current registered policy under the caller's retained native
/// snapshot. This is descriptive shape evidence, never mutation authority.
pub fn registered_public_projection_fields(
    db: &Connection,
    subject: &str,
) -> Result<Option<BTreeSet<String>>> {
    if !installed(db)? {
        return Ok(None);
    }
    let Some(fingerprint) = current_fingerprint(db, subject)? else {
        return Ok(None);
    };
    let schema = read_registered(db, subject, &fingerprint)?;
    let (projection_fingerprint, effective, item): (String, String, String) = db.query_row("SELECT p.schema_fingerprint,p.effective_json,i.metadata FROM catalog_metadata_projection p JOIN items i ON i.id=p.item_id WHERE p.item_id=? AND length(CAST(p.effective_json AS BLOB))<=? AND length(CAST(i.metadata AS BLOB))<=?", params![subject,MAX_METADATA_SNAPSHOT_BYTES as i64,MAX_METADATA_SNAPSHOT_BYTES as i64], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    if projection_fingerprint != fingerprint
        || serde_json::from_str::<Value>(&effective)? != serde_json::from_str::<Value>(&item)?
    {
        return Err(corrupt("registered projection diverges from current item"));
    }

    Ok(Some(
        schema
            .fields()
            .keys()
            .filter(|field| {
                !field.starts_with("motion_") && !schema.system_fields().contains(*field)
            })
            .cloned()
            .collect(),
    ))
}

pub(super) fn charge_retained_schemas(
    db: &Connection,
    budget: &crate::operation_budget::OperationBudget,
    subject: &str,
) -> Result<()> {
    if !installed(db)? {
        return Ok(());
    }
    let (bytes,fields,count):(i64,i64,i64)=db.query_row("SELECT coalesce(sum(length(CAST(s.policies_json AS BLOB))+length(CAST(s.system_fields_json AS BLOB))),0),coalesce(sum(json_array_length(s.policies_json)),0),(SELECT count(*) FROM catalog_metadata_sources WHERE item_id=?1) FROM catalog_metadata_schemas s WHERE s.item_id=?1 AND s.schema_fingerprint IN (SELECT schema_fingerprint FROM catalog_metadata_sources WHERE item_id=?1 UNION SELECT schema_fingerprint FROM catalog_metadata_schema_heads WHERE item_id=?1)",[subject],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let current_fields:i64=db.query_row("SELECT json_array_length(s.policies_json) FROM catalog_metadata_schema_heads h JOIN catalog_metadata_schemas s ON s.item_id=h.item_id AND s.schema_fingerprint=h.schema_fingerprint WHERE h.item_id=?",[subject],|r|r.get(0)).optional()?.unwrap_or(0);
    budget
        .charge(crate::operation_budget::BudgetCharge {
            bytes: bytes as usize,
            values: fields as usize,
            work: bytes as usize / 64
                + fields as usize
                + (count as usize).saturating_mul(current_fields as usize)
                + 1,
            ..Default::default()
        })
        .map_err(|_| {
            fail(
                MetadataStoreErrorCode::StoreFailure,
                "retained schema parse/compatibility budget exhausted",
            )
        })
}
