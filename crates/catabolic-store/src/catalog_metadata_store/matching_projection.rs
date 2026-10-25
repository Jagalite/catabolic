//! Exact managed match-state projection for the private candidate41 Unmatch
//! compound owner. A protected-field grant is not identity or Pin authority.
//! The caller owns that proof, the deferred barrier and the sole outer receipt.
use super::*;
use crate::catalog_nfo_attribution::{ProofBudget, matching::UnmatchProjectionPermit};
use std::collections::BTreeMap;

const FIELD: &str = "motion_catalog";

/// Owner-created only: no Deserialize, public fields or caller-selected effects.
#[derive(Debug)]
pub(crate) struct UnmatchProjectionPlan {
    subject: String,
    outer_digest: String,
    raw_request: String,
    item_revision: i64,
    before: RefreshMetadataSnapshot,
    effective_before: Value,
    changed: Vec<ContributionDocument>,
    resolution: MetadataResolution,
    digest: String,
}
impl UnmatchProjectionPlan {
    pub(crate) fn subject(&self) -> &str {
        &self.subject
    }
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
    pub(crate) fn before(&self) -> &RefreshMetadataSnapshot {
        &self.before
    }
    pub(crate) fn item_revision(&self) -> i64 {
        self.item_revision
    }
    pub(crate) fn effective_before(&self) -> &Value {
        &self.effective_before
    }
    pub(crate) fn effective_after(&self) -> &Value {
        match &self.resolution.fields[FIELD] {
            FieldResolution::Resolved { value, .. } => value,
            _ => unreachable!("owner verified exact resolved marker"),
        }
    }
    pub(crate) fn changed(&self) -> &[ContributionDocument] {
        &self.changed
    }
    pub(crate) fn resolution(&self) -> &MetadataResolution {
        &self.resolution
    }
}
fn denied(message: &str) -> MetadataStoreError {
    fail(
        MetadataStoreErrorCode::AuthorityDenied,
        format!("unmatch projection: {message}"),
    )
}
fn budget_error(error: impl std::fmt::Display) -> MetadataStoreError {
    fail(
        MetadataStoreErrorCode::StoreFailure,
        format!("unmatch projection budget: {error}"),
    )
}
fn scope(store: &crate::Store, outer: &CommandEnvelope, original_generation: bool) -> Result<()> {
    if store.schema_version != 41
        || !store.writable
        || store.db.is_autocommit()
        || outer.key().catalog_id() != store.database_id
        || Some(outer.admission_incarnation()) != store.catalog_incarnation.as_deref()
    {
        return Err(denied("exact native41 retained writer required"));
    }
    let exact:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_state WHERE id=1 AND catalog_id=? AND incarnation=? AND (?=0 OR generation=?))",params![outer.key().catalog_id(),outer.admission_incarnation(),original_generation,outer.expected_generation()],|r|r.get(0))?;
    if !exact {
        return Err(denied("current catalog/incarnation/generation differs"));
    }
    Ok(())
}
fn marker<'a>(value: &'a Value, subject: &str) -> Result<&'a serde_json::Map<String, Value>> {
    let object = value
        .as_object()
        .ok_or_else(|| corrupt("Unmatch marker is not an object"))?;
    if object.get("contract_version") != Some(&json!(1))
        || object.get("item_id").and_then(Value::as_str) != Some(subject)
        || !matches!(
            object.get("match_state").and_then(Value::as_str),
            Some("unmatched" | "ambiguous" | "matched" | "manual")
        )
        || !object.get("external_ids").is_some_and(Value::is_array)
    {
        return Err(corrupt(
            "Unmatch marker contract/subject/state/identities differ",
        ));
    }
    Ok(object)
}
fn grant_matches(
    grant: &MetadataAuthority,
    outer: &CommandEnvelope,
    subject: &str,
    document: &ContributionDocument,
) -> bool {
    grant.host_namespace == outer.key().host_namespace()
        && grant.subject == subject
        && matches!(&grant.access,MetadataAccess::SourceV2 {namespace,record_id,kind,system_fields}
            if namespace==document.key().namespace() && record_id==document.key().record_id()
                && *kind==document.kind() && system_fields.len()==1 && system_fields.contains(FIELD))
}
// Changed documents are a unique ordered subsequence of the complete captured
// snapshot. Merge by borrowed key in linear work, never a quadratic source scan.
fn merged<'a>(
    before: &'a [ContributionDocument],
    changed: &'a [ContributionDocument],
) -> impl Iterator<Item = &'a ContributionDocument> {
    let mut changes = changed.iter().peekable();
    before.iter().map(move |document| {
        if changes
            .peek()
            .is_some_and(|next| next.key() == document.key())
        {
            changes.next().expect("peeked change")
        } else {
            document
        }
    })
}
fn visible_value(field: &FieldResolution) -> Option<&Value> {
    match field {
        FieldResolution::Resolved { value, .. } => Some(value),
        _ => None,
    }
}

pub(crate) fn plan_unmatch_projection_in(
    store: &crate::Store,
    outer: &CommandEnvelope,
    subject: &str,
    grants: &[MetadataAuthority],
) -> Result<UnmatchProjectionPlan> {
    scope(store, outer, true)?;
    let mut budget = ProofBudget::new(store.catalog_operation_budget().map_err(budget_error)?);
    budget
        .serialized(
            &(
                subject,
                outer.key(),
                outer.admission_incarnation(),
                outer.digest().as_str(),
            ),
            64,
        )
        .map_err(budget_error)?;
    ContributionKey::new(subject, "motion.structure", "source_projection")?;
    if grants.len() > MAX_METADATA_DOCUMENTS
        || outer.canonical_payload().len() > MAX_RAW_MUTATION_BYTES
    {
        return Err(MetadataError::SnapshotBudget.into());
    }
    // Admit the borrowed capability strings before traversing/comparing them.
    // Only exact SourceV2 grants are consumed; no wildcard or prefix authority.
    let mut by_source = BTreeMap::new();
    for grant in grants {
        let MetadataAccess::SourceV2 {
            namespace,
            record_id,
            kind,
            system_fields,
        } = &grant.access
        else {
            return Err(denied("exact SourceV2 contributor grant required"));
        };
        budget
            .serialized(
                &(
                    &grant.host_namespace,
                    &grant.subject,
                    namespace,
                    record_id,
                    kind,
                    system_fields,
                ),
                64,
            )
            .map_err(budget_error)?;
        if by_source
            .insert((namespace.as_str(), record_id.as_str()), grant)
            .is_some()
        {
            return Err(denied("duplicate structural source grant"));
        }
    }
    let before = read_refresh_metadata(store, subject)?;
    if !before.schema.system_fields().contains(FIELD) {
        return Err(denied("already-classified managed marker required"));
    }
    let item_revision:i64=store.db.query_row("SELECT revision FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key=json_array(?) AND deleted=0",[subject],|r|r.get(0))?;
    if item_revision < 1 {
        return Err(corrupt("Unmatch item head absent"));
    }
    // This admission pays for the actual resolver output, not another retained
    // source clone. The original complete snapshot is moved into the plan.
    budget
        .serialized(&before.documents, 64)
        .map_err(budget_error)?;
    let mut old = resolve(&before.schema, subject, &before.documents)?;
    let effective_before = match old.fields.remove(FIELD) {
        Some(FieldResolution::Resolved { value, .. }) => value,
        _ => return Err(corrupt("Unmatch effective structural marker unresolved")),
    };
    let effective = marker(&effective_before, subject)?;
    let mut owners = 0usize;
    let mut changed = Vec::new();
    for document in before
        .documents
        .iter()
        .filter(|d| d.fields().contains_key(FIELD))
    {
        owners += 1;
        if document.kind() == SourceKind::LegacyImport || document.key().namespace() == "motion.nfo"
        {
            return Err(denied(
                "legacy/NFO structural contributor needs a separate compound proof",
            ));
        }
        if !by_source
            .get(&(document.key().namespace(), document.key().record_id()))
            .is_some_and(|grant| grant_matches(grant, outer, subject, document))
        {
            return Err(denied("exact unique structural contributor grant required"));
        }
        let current = &document.fields()[FIELD];
        let object = marker(current, subject)?;
        if object.get("external_ids") != effective.get("external_ids") {
            return Err(denied(
                "shadowed contributor identities differ from effective ownership",
            ));
        }
        if object.get("match_state").and_then(Value::as_str) == Some("unmatched") {
            continue;
        }
        budget.serialized(current, 64).map_err(budget_error)?;
        budget.serialized(&"unmatched", 2).map_err(budget_error)?;
        let mut next = current.clone();
        next["match_state"] = json!("unmatched");
        #[derive(Serialize)]
        struct Set<'a> {
            op: &'static str,
            value: &'a Value,
        }
        #[derive(Serialize)]
        struct Edits<'a> {
            motion_catalog: Set<'a>,
        }
        let edits = Edits {
            motion_catalog: Set {
                op: "set",
                value: &next,
            },
        };
        budget.serialized(&edits, 64).map_err(budget_error)?;
        budget
            .canonical_serialized(document.evidence(), 64)
            .map_err(budget_error)?;
        // Admit the retained key/fields/evidence before both the patch key
        // copy and plan_update's source clone.
        budget.serialized(document, 64).map_err(budget_error)?;
        let patch = ContributionPatch::from_json_with_system_fields(
            &before.schema,
            document.key().clone(),
            document.kind(),
            document.revision(),
            &serde_json::to_string(&edits)?,
            &encode(document.evidence()),
        )?;
        let next = plan_update(&before.schema, Some(document), &patch)?;
        if next.evidence() != document.evidence()
            || next.fields().len() != document.fields().len()
            || document
                .fields()
                .iter()
                .any(|(field, value)| field != FIELD && next.fields().get(field) != Some(value))
        {
            return Err(corrupt("Unmatch patch changed unrelated source content"));
        }
        changed.push(next);
    }
    if owners == 0 || grants.len() != owners {
        return Err(denied(
            "grants do not exactly cover all structural contributors",
        ));
    }
    let mut proposed = Vec::new();
    for next in merged(&before.documents, &changed) {
        budget.serialized(next, 64).map_err(budget_error)?;
        proposed.push(next.clone());
    }
    let resolution = resolve(&before.schema, subject, &proposed)?;
    let after = match resolution.fields.get(FIELD) {
        Some(FieldResolution::Resolved { value, .. }) => value,
        _ => return Err(corrupt("Unmatch next structural marker unresolved")),
    };
    let after = marker(after, subject)?;
    if after.len() != effective.len()
        || after.get("match_state").and_then(Value::as_str) != Some("unmatched")
        || effective
            .iter()
            .any(|(key, value)| key != "match_state" && after.get(key) != Some(value))
        || old.fields.iter().any(|(field, value)| {
            resolution.fields.get(field).map(visible_value) != Some(visible_value(value))
        })
    {
        return Err(corrupt(
            "Unmatch resolution changed unrelated effective metadata",
        ));
    }
    // Stream exact original outer bytes before retaining them for immutable
    // source history. There is no synthetic independently replayable command.
    budget
        .raw_json(outer.canonical_payload(), 64)
        .map_err(budget_error)?;
    let raw_request = outer.canonical_payload().to_owned();
    let digest = budget
        .serialized(
            &(
                "catabolic.unmatch.projection.v1",
                outer.digest().as_str(),
                subject,
                item_revision,
                before.schema.fingerprint(),
                &before.documents,
                &changed,
                &resolution,
            ),
            64,
        )
        .map_err(budget_error)?;
    Ok(UnmatchProjectionPlan {
        subject: subject.to_owned(),
        outer_digest: outer.digest().as_str().to_owned(),
        raw_request,
        item_revision,
        before,
        effective_before,
        changed,
        resolution,
        digest,
    })
}

pub(crate) fn apply_unmatch_projection_in(
    store: &crate::Store,
    outer: &CommandEnvelope,
    plan: &UnmatchProjectionPlan,
    permit: &UnmatchProjectionPermit,
    admitted_at_us: i64,
) -> Result<()> {
    scope(store, outer, false)?;
    if admitted_at_us < 0
        || plan.outer_digest != outer.digest().as_str()
        || plan.raw_request != outer.canonical_payload()
    {
        return Err(denied(
            "outer identity/raw history differs from prepared plan",
        ));
    }
    permit
        .authorize_projection_in(store, outer, plan)
        .map_err(|error| denied(&error.to_string()))?;
    // Pins may legitimately advance global generation first. Only the caller's
    // typed permit authorizes that corridor; metadata/schema still match exactly.
    let exact_item: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key=json_array(?) AND deleted=0 AND revision=?)",
        params![plan.subject(), plan.item_revision], |row| row.get(0),
    )?;
    if !exact_item {
        return Err(denied("item head changed outside Pin-only corridor"));
    }
    let actual = read_refresh_metadata(store, plan.subject())?;
    if actual.schema != plan.before.schema || actual.documents != plan.before.documents {
        return Err(denied("complete metadata predecessor changed"));
    }
    let mut budget = ProofBudget::new(store.catalog_operation_budget().map_err(budget_error)?);
    for document in &plan.changed {
        budget
            .serialized(document.fields(), 64)
            .map_err(budget_error)?;
        budget
            .canonical_serialized(document.evidence(), 64)
            .map_err(budget_error)?;
        budget
            .raw_json(&plan.raw_request, 64)
            .map_err(budget_error)?;
        write_revision(
            &store.db,
            document,
            &plan.raw_request,
            outer,
            admitted_at_us,
        )?;
    }
    if !plan.changed.is_empty() {
        budget
            .serialized(&plan.resolution, 64)
            .map_err(budget_error)?;
        // Existing writer constructs its effective object and canonical bytes.
        // The full resolution bounds that object's keys/values; canonical
        // expansion is reserved before calling the unchanged writer helper.
        budget
            .canonical_serialized(&plan.resolution, 64)
            .map_err(budget_error)?;
        write_projection(&store.db, &plan.resolution)?;
        // Reuse the complete reader to verify exact source/schema and actual
        // effective item/projection values. It admits current rows before decode.
        let terminal = read_refresh_metadata(store, plan.subject())?;
        if terminal.schema != plan.before.schema
            || terminal.documents.len() != plan.before.documents.len()
            || terminal
                .documents
                .iter()
                .zip(merged(&plan.before.documents, &plan.changed))
                .any(|(actual, expected)| actual != expected)
        {
            return Err(corrupt(
                "Unmatch terminal source/schema differs from exact plan",
            ));
        }
        // The reader above validates the effective value. Check the other cache,
        // resolution_json, against its exact owner-produced wire as well.
        budget
            .serialized(&plan.resolution, 64)
            .map_err(budget_error)?;
        let expected = serde_json::to_string(&plan.resolution)?;
        let mut query = store
            .db
            .prepare("SELECT resolution_json FROM catalog_metadata_projection WHERE item_id=?")?;
        let mut rows = query.query([plan.subject()])?;
        let row = rows
            .next()?
            .ok_or_else(|| corrupt("Unmatch terminal projection absent"))?;
        let rusqlite::types::ValueRef::Text(bytes) = row.get_ref(0)? else {
            return Err(corrupt("Unmatch terminal projection is not text"));
        };
        if bytes.len() > MAX_METADATA_SNAPSHOT_BYTES {
            return Err(MetadataError::SnapshotBudget.into());
        }
        let raw =
            std::str::from_utf8(bytes).map_err(|_| corrupt("invalid terminal projection UTF-8"))?;
        budget.raw_json(raw, 64).map_err(budget_error)?;
        if raw != expected {
            return Err(corrupt("Unmatch terminal resolution bytes differ"));
        }
    }
    // No receipt, schema change, Pin/tag/source-attribution effect or barrier
    // release. Caller verifies the terminal NFO closure before releasing it.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use catabolic_core::catalog_command::CommandKey;

    fn document(namespace: &str, revision: i64) -> ContributionDocument {
        let schema = FieldSchema::new_v2(
            vec![],
            vec![(
                FIELD.into(),
                FieldPolicy {
                    value_type: ValueType::Json,
                    nullable: false,
                    normalization: Normalization::Exact,
                },
            )],
        )
        .unwrap();
        ContributionDocument::from_json_with_system_fields(
            &schema,
            ContributionKey::new("work", namespace, "source_projection").unwrap(),
            SourceKind::Scan,
            revision,
            &json!({"motion_catalog": {
                "contract_version":1,"item_id":"work","match_state":"manual",
                "external_ids":[],"retained":{"opaque":true}
            }})
            .to_string(),
            r#"{"owner":"test evidence"}"#,
        )
        .unwrap()
    }

    #[test]
    fn unmatch_marker_accepts_exact_states_and_refuses_malformed_identity_shape() {
        let original = document("motion.structure", 1);
        let value = &original.fields()[FIELD];
        for state in ["unmatched", "ambiguous", "matched", "manual"] {
            let mut current = value.clone();
            current["match_state"] = json!(state);
            let parsed = marker(&current, "work").unwrap();
            assert_eq!(parsed["retained"], value["retained"]);
        }
        for (field, invalid) in [
            ("contract_version", json!(2)),
            ("item_id", json!("another-work")),
            ("match_state", json!("accepted")),
            ("match_state", Value::Null),
            ("external_ids", json!({})),
        ] {
            let mut current = value.clone();
            current[field] = invalid;
            assert!(marker(&current, "work").is_err(), "{field}");
        }
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove("match_state");
        assert!(marker(&missing, "work").is_err());
        assert!(marker(&json!([]), "work").is_err());
    }

    #[test]
    fn unmatch_grant_requires_exact_host_subject_source_kind_and_marker_only() {
        let outer = CommandEnvelope::new(
            1,
            CommandKey::new("catalog", "host", "unmatch").unwrap(),
            "incarnation",
            1,
            r#"{"operation":"nfo_unmatch"}"#,
        )
        .unwrap();
        let document = document("motion.structure", 1);
        let grant = MetadataAuthority {
            host_namespace: "host".into(),
            subject: "work".into(),
            access: MetadataAccess::SourceV2 {
                namespace: "motion.structure".into(),
                record_id: "source_projection".into(),
                kind: SourceKind::Scan,
                system_fields: [FIELD.into()].into(),
            },
        };
        assert!(grant_matches(&grant, &outer, "work", &document));
        for case in 0..8 {
            let mut wrong = grant.clone();
            match case {
                0 => wrong.host_namespace = "other-host".into(),
                1 => wrong.subject = "other-work".into(),
                2 => {
                    wrong.access = MetadataAccess::Source {
                        namespace: "motion.structure".into(),
                        record_id: "source_projection".into(),
                        kind: SourceKind::Scan,
                    }
                }
                _ => {
                    let MetadataAccess::SourceV2 {
                        namespace,
                        record_id,
                        kind,
                        system_fields,
                    } = &mut wrong.access
                    else {
                        unreachable!()
                    };
                    match case {
                        3 => *namespace = "motion.other".into(),
                        4 => *record_id = "another-record".into(),
                        5 => *kind = SourceKind::Local,
                        6 => {
                            system_fields.insert("motion_artwork".into());
                        }
                        _ => system_fields.clear(),
                    }
                }
            }
            assert!(
                !grant_matches(&wrong, &outer, "work", &document),
                "case {case}"
            );
        }
    }

    #[test]
    fn unmatch_borrowed_merge_preserves_complete_order_and_unchanged_sources() {
        let before = vec![
            document("motion.a", 1),
            document("motion.b", 3),
            document("motion.c", 7),
        ];
        assert!(merged(&before, &[]).eq(before.iter()));
        let changed = vec![document("motion.a", 2), document("motion.c", 8)];
        let result: Vec<_> = merged(&before, &changed).collect();
        assert_eq!(result, vec![&changed[0], &before[1], &changed[1]]);
        assert!(std::ptr::eq(result[1], &before[1]));
        assert_eq!(result[0].evidence(), before[0].evidence());
        assert_eq!(result[2].fields(), before[2].fields());
    }
}
