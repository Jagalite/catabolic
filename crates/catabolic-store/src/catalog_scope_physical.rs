//! Stored whole-file observations/facts, separately authorized from item metadata
//! and content access. Nothing here opens a media path or selects a playback plan.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileFactField {
    Size,
    MtimeNs,
    Status,
    RecordedRevision,
    HashDigest,
    VerifyDigest,
    ProbeFormatName,
    ProbeDuration,
    ProbeSize,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct TrustedFileFactsRead {
    pub(super) by_role: BTreeMap<String, BTreeSet<FileFactField>>,
}
impl TrustedFileFactsRead {
    /// Deliberately grants the selected WHOLE physical file facts, even when an
    /// admitted item covers only a slice or shares the file with hidden items.
    /// This never grants content, paths, interval effects or episode measurements.
    pub fn whole_file(roles: Vec<String>, fields: Vec<FileFactField>) -> Result<Self> {
        if roles.is_empty() || roles.len() > 32 || fields.len() > 9 {
            return Err(fail(ScopeErrorCode::InvalidGrant));
        }
        let fields: BTreeSet<_> = fields.into_iter().collect();
        let mut by_role = BTreeMap::new();
        for role in roles {
            crate::curation::validate_file_read_role(&role)
                .map_err(|_| fail(ScopeErrorCode::InvalidGrant))?;
            by_role.insert(role, fields.clone());
        }
        Ok(Self { by_role })
    }
}
pub(super) fn charge_raw(bytes: i64) -> Result<()> {
    if !(0..=MAX_DEFINITION_BYTES as i64).contains(&bytes) {
        return Err(fail(ScopeErrorCode::ByteBudget));
    }
    shared_charge(BudgetCharge {
        items: 1,
        bytes: bytes as usize,
        values: 1,
        work: bytes as usize / 64 + 1,
    })
}
/// Before the existing revision owner loads any recorded path/policy strings.
/// Those values are used only internally to check exact recorded identity.
pub(super) fn recorded_revision(store: &Store, profile: &str, file: &str) -> Result<String> {
    let bytes: i64=store.db.query_row("SELECT length(CAST(f.id AS BLOB))+length(CAST(f.location AS BLOB))+length(CAST(f.path AS BLOB))+coalesce(length(CAST(b.root AS BLOB)),0)+coalesce(length(CAST(p.settings AS BLOB)),0)+coalesce(length(CAST(p.identity_policy AS BLOB)),0)+coalesce(length(CAST(v.volume_uuid AS BLOB)),0)+coalesce(length(CAST(o.status AS BLOB)),0) FROM main.files f LEFT JOIN main.bindings b ON b.profile=? AND b.kind='source' AND b.owner=f.location LEFT JOIN main.source_identity_policies p ON p.profile=? AND p.location=f.location LEFT JOIN main.binding_volumes v ON v.profile=? AND v.kind='source' AND v.owner=f.location LEFT JOIN main.observations o ON o.profile=? AND o.file_id=f.id WHERE f.id=?",params![profile,profile,profile,profile,file],|r|r.get(0))?;
    charge_raw(bytes)?;
    Ok(crate::components::revision(&crate::components::occurrence(
        &store.db, profile, file,
    )?))
}
fn processing_fact(
    store: &Store,
    profile: &str,
    file: &str,
    operation: &str,
) -> Result<Option<Value>> {
    let bytes:Option<i64>=store.db.query_row(&crate::catalog_source_succession::current::view(store.schema_version, "SELECT length(CAST(snapshot AS BLOB))+length(CAST(data AS BLOB))+length(CAST(job_id AS BLOB))+length(CAST(status AS BLOB))+length(CAST(operation AS BLOB)) FROM main.file_facts WHERE profile=? AND file_id=? AND operation=?"),params![profile,file,operation],|r|r.get(0)).optional()?;
    let Some(bytes) = bytes else { return Ok(None) };
    charge_raw(bytes)?;
    let current = recorded_revision(store, profile, file)?;
    let Some(fact) = crate::public_reads::fact(store, profile, file, operation)? else {
        return Ok(None);
    };
    if !fact["snapshot"].is_object() || !fact["data"].is_object() {
        return Err(fail(ScopeErrorCode::Catalog));
    }
    let revision = crate::components::revision(&fact["snapshot"]);
    Ok(Some(
        json!({"status":fact["status"],"recorded_revision":revision,"exact_revision_current":fact["status"]=="complete" && revision==current,"data":fact["data"]}),
    ))
}
fn scalar(value: &Value) -> Result<Value> {
    if value.is_array() || value.is_object() {
        return Err(fail(ScopeErrorCode::Catalog));
    }
    Ok(value.clone())
}
pub(super) fn file_facts(
    store: &Store,
    cap: &Capability,
    item: &str,
    mask: &TrustedFileFactsRead,
    started: Instant,
    budget: ReadBudget,
) -> Result<Value> {
    let roles = encode(&json!(mask.by_role.keys().collect::<Vec<_>>()));
    shared_charge(BudgetCharge {
        bytes: roles.len(),
        values: 1,
        work: 1,
        ..Default::default()
    })?;
    let mut statement=store.db.prepare("SELECT CASE WHEN length(CAST(id AS BLOB))<=1024 THEN id ELSE '' END,CASE WHEN length(CAST(file_id AS BLOB))<=1024 THEN file_id ELSE '' END,role,part,length(CAST(id AS BLOB))+length(CAST(file_id AS BLOB))+length(CAST(role AS BLOB)) FROM main.item_files WHERE item_id=? AND active=1 AND role IN (SELECT value FROM json_each(?)) ORDER BY id COLLATE BINARY")?;
    let mut rows = statement.query(params![item, roles])?;
    let mut files = Vec::new();
    let mut output_bytes = 0usize;
    while let Some(row) = rows.next()? {
        remaining(started, budget)?;
        if files.len() == budget.maximum_ids {
            return Err(fail(ScopeErrorCode::IdBudget));
        }
        charge_raw(row.get(4)?)?;
        let association: String = row.get(0)?;
        let file: String = row.get(1)?;
        let role: String = row.get(2)?;
        let part: Option<i64> = row.get(3)?;
        if association.is_empty() || file.is_empty() {
            return Err(fail(ScopeErrorCode::ByteBudget));
        }
        let fields = mask
            .by_role
            .get(&role)
            .ok_or_else(|| fail(ScopeErrorCode::WrongAuthority))?;
        let mut observed = serde_json::Map::new();
        for field in fields {
            let (key, column) = match field {
                FileFactField::Size => ("size", "size"),
                FileFactField::MtimeNs => ("mtime_ns", "mtime_ns"),
                FileFactField::Status => ("status", "status"),
                _ => continue,
            };
            let value = if *field == FileFactField::Status {
                let bytes:Option<i64>=store.db.query_row("SELECT length(CAST(status AS BLOB)) FROM main.observations WHERE profile=? AND file_id=?",params![cap.profile,file],|r|r.get(0)).optional()?;
                if let Some(bytes) = bytes {
                    charge_raw(bytes)?;
                }
                let value: Option<String> = store
                    .db
                    .query_row(
                        "SELECT status FROM main.observations WHERE profile=? AND file_id=?",
                        params![cap.profile, file],
                        |r| r.get(0),
                    )
                    .optional()?;
                json!(value.unwrap_or_else(|| "unknown".into()))
            } else {
                let value: Option<Option<i64>> = store
                    .db
                    .query_row(
                        &format!(
                            "SELECT {column} FROM main.observations WHERE profile=? AND file_id=?"
                        ),
                        params![cap.profile, file],
                        |r| r.get(0),
                    )
                    .optional()?;
                json!(value.flatten())
            };
            observed.insert(key.into(), value);
        }
        if fields.contains(&FileFactField::RecordedRevision) {
            observed.insert(
                "recorded_revision".into(),
                json!(recorded_revision(store, &cap.profile, &file)?),
            );
        }
        let mut processing = serde_json::Map::new();
        for (operation, selected) in [
            (
                "hash",
                vec![(FileFactField::HashDigest, "digest", vec!["digest"])],
            ),
            (
                "verify",
                vec![(FileFactField::VerifyDigest, "digest", vec!["digest"])],
            ),
            (
                "probe",
                vec![
                    (
                        FileFactField::ProbeFormatName,
                        "format_name",
                        vec!["format", "format_name"],
                    ),
                    (
                        FileFactField::ProbeDuration,
                        "duration",
                        vec!["format", "duration"],
                    ),
                    (FileFactField::ProbeSize, "size", vec!["format", "size"]),
                ],
            ),
        ] {
            let selected = selected
                .into_iter()
                .filter(|(field, _, _)| fields.contains(field))
                .collect::<Vec<_>>();
            if selected.is_empty() {
                continue;
            }
            if let Some(mut fact) = processing_fact(store, &cap.profile, &file, operation)? {
                let mut data = serde_json::Map::new();
                for (_, key, path) in selected {
                    let value = path.iter().fold(&fact["data"], |value, key| &value[*key]);
                    data.insert(key.into(), scalar(value)?);
                }
                fact["data"] = Value::Object(data);
                if !fields.contains(&FileFactField::RecordedRevision) {
                    fact.as_object_mut().unwrap().remove("recorded_revision");
                }
                processing.insert(operation.into(), fact);
            }
        }
        let output = json!({"association_id":association,"item_id":item,"file_id":file,"role":role,"part":part,
            "physical_scope":"whole_file_recorded","observed":observed,"processing":processing});
        let bytes = encode(&output).len();
        output_bytes = output_bytes.saturating_add(bytes);
        if output_bytes > MAX_BYTES {
            return Err(fail(ScopeErrorCode::ByteBudget));
        }
        shared_charge(BudgetCharge {
            bytes,
            values: 1,
            work: bytes / 64 + 1,
            ..Default::default()
        })?;
        files.push(output);
    }
    Ok(
        json!({"contract_version":1,"item_id":item,"physical_scope":"whole_file_recorded","scoped_total":files.len(),"files":files}),
    )
}
