//! Field-only protected NFO identity projection for the private refresh owner.
//! A namespace or marker never grants ownership. All contributing marker sources
//! need independently prepared exact grants, including shadowed sources.
use super::*;
use std::collections::BTreeSet;

pub(crate) struct ProjectionGrants<'a> {
    pub sources: &'a [MetadataAuthority],
    pub legacy_reconciliation: Option<&'a MetadataAuthority>,
}

/// Classification is itself an authority-requiring policy change, even when
/// the resolved marker is unchanged. Prove every contributing source, including
/// shadowed contributors, before evolving the historical schema.
pub(super) fn require_classification(
    documents: &[ContributionDocument],
    command: &CommandEnvelope,
    subject: &str,
    grants: &ProjectionGrants<'_>,
) -> Result<()> {
    let mut owners = 0usize;
    for document in documents
        .iter()
        .filter(|d| d.fields().contains_key("motion_catalog"))
    {
        owners += 1;
        if document.kind() == SourceKind::LegacyImport {
            let grant = grants.legacy_reconciliation.ok_or_else(|| {
                fail(
                    MetadataStoreErrorCode::AuthorityDenied,
                    "classification requires exact legacy reconciliation grant",
                )
            })?;
            if grant.host_namespace != command.key().host_namespace()
                || grant.subject != subject
                || !matches!(grant.access, MetadataAccess::ReconcileLegacy)
                || document.key().namespace() != LEGACY_NAMESPACE
                || document.key().record_id() != LEGACY_RECORD_ID
            {
                return Err(fail(
                    MetadataStoreErrorCode::AuthorityDenied,
                    "classification legacy owner differs",
                ));
            }
            let target = ContributionKey::new(subject, "motion.structure", "source_projection")?;
            require_source(command, subject, &target, SourceKind::Scan, grants)?;
        } else {
            require_source(command, subject, document.key(), document.kind(), grants)?;
        }
    }
    if owners == 0 {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "classification has no independently owned structural contributor",
        ));
    }
    Ok(())
}

/// Plan against the complete current source set, not only the resolved winner.
/// The caller retains the batch barrier and persists these revisions with the
/// SAME outer receipt as source/presence/index publication. No effects here.
pub(super) fn plan(
    schema: &FieldSchema,
    documents: &[ContributionDocument],
    command: &CommandEnvelope,
    subject: &str,
    prior_document_identity: Option<&str>,
    pin: Option<&str>,
    next_document_identity: Option<&str>,
    grants: &ProjectionGrants<'_>,
) -> Result<Vec<ContributionDocument>> {
    const FIELD: &str = "motion_catalog";
    if pin.is_some() && next_document_identity != pin {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "refresh cannot overwrite independently pinned external identity",
        ));
    }
    if !schema.system_fields().contains(FIELD) {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "protected structural field lacks explicit schema classification",
        ));
    }
    let old = resolve(schema, subject, documents)?;
    let FieldResolution::Resolved {
        value: effective, ..
    } = old
        .fields
        .get(FIELD)
        .ok_or_else(|| corrupt("structural projection absent"))?
    else {
        return Err(corrupt("structural projection unresolved"));
    };
    let desired_effective = changed_marker(
        effective,
        subject,
        prior_document_identity,
        pin,
        next_document_identity,
    )?;
    let owners: Vec<_> = documents
        .iter()
        .filter(|d| d.fields().contains_key(FIELD))
        .collect();
    if owners.is_empty() {
        return Err(corrupt("structural source absent"));
    }
    let mut changed = Vec::new();
    let mut legacy = None;
    for document in owners {
        let before = &document.fields()[FIELD];
        let after = changed_marker(
            before,
            subject,
            prior_document_identity,
            pin,
            next_document_identity,
        )?;
        if before == &after {
            continue;
        }
        if document.kind() == SourceKind::LegacyImport {
            if legacy.is_some() {
                return Err(corrupt("multiple retained structural legacy owners"));
            }
            let grant = grants.legacy_reconciliation.ok_or_else(|| {
                fail(
                    MetadataStoreErrorCode::AuthorityDenied,
                    "exact retained structural reconciliation grant required",
                )
            })?;
            if grant.host_namespace != command.key().host_namespace()
                || grant.subject != subject
                || !matches!(grant.access, MetadataAccess::ReconcileLegacy)
                || document.key().namespace() != LEGACY_NAMESPACE
                || document.key().record_id() != LEGACY_RECORD_ID
            {
                return Err(fail(
                    MetadataStoreErrorCode::AuthorityDenied,
                    "retained structural reconciliation grant differs",
                ));
            }
            changed.push(plan_legacy_reconciliation(
                schema,
                document,
                document.revision(),
                &[FIELD.into()],
            )?);
            legacy = Some(after);
        } else {
            require_source(command, subject, document.key(), document.kind(), grants)?;
            changed.push(patch(
                schema,
                document.key().clone(),
                document.kind(),
                Some(document),
                after,
                document.evidence(),
            )?);
        }
    }
    if let Some(value) = legacy {
        // Removing a retained legacy field is explicit reconciliation, never
        // relabeling its provenance. Its exact original history remains. The
        // replacement structural source has its own independently scoped grant.
        let key = ContributionKey::new(subject, "motion.structure", "source_projection")?;
        require_source(command, subject, &key, SourceKind::Scan, grants)?;
        let current = documents.iter().find(|d| d.key() == &key);
        if changed.iter().any(|d| d.key() == &key) {
            // Two independently different structural projections must not be
            // silently collapsed into one source or one revision.
            return Err(corrupt(
                "legacy and existing structural replacement overlap",
            ));
        }
        if current.is_some_and(|d| d.fields().contains_key(FIELD)) {
            return Err(corrupt(
                "legacy structural reconciliation has competing target",
            ));
        }
        let evidence = current.map(|d| d.evidence().clone()).unwrap_or_else(||
            json!({"owner":"motion.structure","basis":"exact retained structural reconciliation"}));
        changed.push(patch(
            schema,
            key,
            SourceKind::Scan,
            current,
            value,
            &evidence,
        )?);
    }
    let mut proposed = documents.to_vec();
    for next in &changed {
        proposed.retain(|d| d.key() != next.key());
        proposed.push(next.clone());
    }
    let resolved = resolve(schema, subject, &proposed)?;
    match resolved.fields.get(FIELD) {
        Some(FieldResolution::Resolved { value, .. }) if *value == desired_effective => {}
        _ => {
            return Err(corrupt(
                "changed structural sources do not yield exact effective projection",
            ));
        }
    }
    Ok(changed)
}

fn require_source(
    command: &CommandEnvelope,
    subject: &str,
    key: &ContributionKey,
    kind: SourceKind,
    grants: &ProjectionGrants<'_>,
) -> Result<()> {
    let expected: BTreeSet<String> = ["motion_catalog".to_owned()].into();
    let count = grants
        .sources
        .iter()
        .filter(|g| {
            g.host_namespace == command.key().host_namespace()
                && g.subject == subject
                && matches!(&g.access, MetadataAccess::SourceV2 {
                namespace, record_id, kind: granted_kind, system_fields
            } if namespace == key.namespace() && record_id == key.record_id()
                && *granted_kind == kind && system_fields == &expected)
        })
        .count();
    if count != 1 {
        return Err(fail(
            MetadataStoreErrorCode::AuthorityDenied,
            "exact unique structural field source grant required",
        ));
    }
    Ok(())
}
fn patch(
    schema: &FieldSchema,
    key: ContributionKey,
    kind: SourceKind,
    current: Option<&ContributionDocument>,
    marker: Value,
    evidence: &Value,
) -> Result<ContributionDocument> {
    let edits = json!({"motion_catalog":{"op":"set","value":marker}});
    let change = ContributionPatch::from_json_with_system_fields(
        schema,
        key,
        kind,
        current.map_or(0, |d| d.revision()),
        &serde_json::to_string(&edits)?,
        &encode(evidence),
    )?;
    Ok(plan_update(schema, current, &change)?)
}
fn changed_marker(
    before: &Value,
    subject: &str,
    prior: Option<&str>,
    pin: Option<&str>,
    next: Option<&str>,
) -> Result<Value> {
    let object = before
        .as_object()
        .ok_or_else(|| corrupt("invalid structural marker"))?;
    if object.get("contract_version") != Some(&json!(1))
        || object.get("item_id").and_then(Value::as_str) != Some(subject)
    {
        return Err(corrupt("structural marker subject/contract differs"));
    }
    let ids = object
        .get("external_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| corrupt("structural identities absent"))?;
    let mut found = None;
    let mut seen = BTreeSet::new();
    for identity in ids {
        let id = identity
            .as_object()
            .ok_or_else(|| corrupt("invalid structural identity"))?;
        let provider = id
            .get("provider")
            .and_then(Value::as_str)
            .ok_or_else(|| corrupt("structural identity provider absent"))?;
        let value = id
            .get("value")
            .and_then(Value::as_str)
            .ok_or_else(|| corrupt("structural identity value absent"))?;
        if provider.is_empty() || value.is_empty() || !seen.insert(provider) {
            return Err(corrupt("duplicate or empty structural provider identity"));
        }
        if provider == "nfo" {
            if id.len() != 3 || id.get("namespace_version").and_then(Value::as_str) != Some("") {
                return Err(corrupt("unknown NFO structural identity shape"));
            }
            found = Some(value);
        }
    }
    if found != prior && !(pin.is_some() && found == pin) {
        return Err(corrupt(
            "structural NFO identity lacks current document/pin provenance",
        ));
    }
    if found == next {
        return Ok(before.clone());
    }
    let mut after = before.clone();
    let output = after["external_ids"]
        .as_array_mut()
        .expect("validated array");
    if let Some(index) = output.iter().position(|id| id["provider"] == "nfo") {
        if let Some(value) = next {
            output[index]["value"] = json!(value);
        } else {
            output.remove(index);
        }
    } else if let Some(value) = next {
        output.push(json!({"provider":"nfo","value":value,"namespace_version":""}));
    }
    Ok(after)
}

#[cfg(test)]
mod tests;
