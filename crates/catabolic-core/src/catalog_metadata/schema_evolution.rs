// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Explicit v2 policy unions and append-only compatibility. All operations are
//! pure; the owner persists registry/history and performs authorization and CAS.

use super::*;

pub const METADATA_CONTRACT_VERSION_V2: u32 = 2;
pub const MAX_METADATA_V2_EXTERNAL_FIELDS: usize = 100;
pub const MAX_METADATA_V2_FIELD_BYTES: usize = 100;

/// Parse external replacement fields before planning their schema union. This
/// deliberately does not normalize values or authorize reserved names; validate
/// the resulting request against the planned schema with ContributionReplacement.
/// Duplicate JSON keys and all existing JSON byte/node/depth limits fail closed.
pub fn replacement_fields(raw: &str) -> Result<BTreeMap<String, Value>> {
    let value = bounded_json(
        raw,
        MAX_METADATA_DOCUMENT_BYTES,
        MetadataError::DocumentBudget,
    )?;
    let fields = value.as_object().ok_or(MetadataError::ExpectedObject)?;
    if fields.len() > MAX_METADATA_V2_EXTERNAL_FIELDS {
        return Err(MetadataError::FieldBudget);
    }
    for field in fields.keys() {
        v2_field_name(field)?;
    }
    Ok(fields
        .iter()
        .map(|(field, value)| (field.clone(), value.clone()))
        .collect())
}

impl FieldSchema {
    /// A v2 union may contain policies from many sources. The 100-field limit is
    /// per external contribution, not a schema-union limit. Reserved names are
    /// supplied explicitly by the trusted owner, never inferred from spelling.
    /// Both policy lists together remain within the existing byte/node budgets.
    /// An explicitly empty v2 union is valid for an uninitialized contribution;
    /// it has its own fingerprint and no synthetic or implicitly allowed fields.
    /// The v1 constructor continues to reject empty schemas.
    pub fn new_v2(
        external_policies: Vec<(String, FieldPolicy)>,
        system_policies: Vec<(String, FieldPolicy)>,
    ) -> Result<Self> {
        if external_policies
            .len()
            .saturating_add(system_policies.len())
            > MAX_METADATA_JSON_NODES
        {
            return Err(MetadataError::JsonNodeBudget);
        }
        let mut fields = BTreeMap::new();
        let mut system_fields = BTreeSet::new();
        for (reserved, policies) in [(false, external_policies), (true, system_policies)] {
            for (field, policy) in policies {
                v2_field_name(&field)?;
                if policy.value_type != ValueType::String
                    && policy.normalization != Normalization::Exact
                {
                    return Err(MetadataError::InvalidPolicy);
                }
                if reserved {
                    system_fields.insert(field.clone());
                }
                if fields.insert(field, policy).is_some() {
                    return Err(MetadataError::DuplicateField);
                }
            }
        }
        let mut schema = Self {
            fields,
            fingerprint: String::new(),
            contract_version: METADATA_CONTRACT_VERSION_V2,
            system_fields,
        };
        let payload = schema.v2_payload();
        let encoded = crate::encode_with(&payload, false, true);
        if encoded.len() > MAX_METADATA_SNAPSHOT_BYTES {
            return Err(MetadataError::SnapshotBudget);
        }
        json_nodes(&payload)?;
        schema.fingerprint = crate::sha256(encoded);
        Ok(schema)
    }

    pub const fn contract_version(&self) -> u32 {
        self.contract_version
    }

    pub fn system_fields(&self) -> &BTreeSet<String> {
        &self.system_fields
    }

    fn v2_payload(&self) -> Value {
        json!({
            "contract_version": self.contract_version,
            "fields": self.fields,
            "system_fields": self.system_fields,
        })
    }

    pub(super) fn validate_field_name(&self, field: &str) -> Result<()> {
        if self.contract_version == METADATA_CONTRACT_VERSION_V2 {
            v2_field_name(field)
        } else {
            field_name(field)
        }
    }

    pub(super) fn validate_field_set<'a>(
        &self,
        fields: impl Iterator<Item = &'a str>,
        allow_system: bool,
    ) -> Result<()> {
        let fields: Vec<_> = fields.collect();
        let external_count = fields
            .iter()
            .filter(|field| !self.system_fields.contains(**field))
            .count();
        let limit = if self.contract_version == METADATA_CONTRACT_VERSION_V2 {
            MAX_METADATA_V2_EXTERNAL_FIELDS
        } else {
            MAX_METADATA_FIELDS
        };
        if external_count > limit {
            return Err(MetadataError::FieldBudget);
        }
        for field in fields {
            self.validate_field_name(field)?;
            if !allow_system && self.system_fields.contains(field) {
                return Err(MetadataError::ReservedField);
            }
            if !self.fields.contains_key(field) {
                return Err(MetadataError::UnknownField);
            }
        }
        Ok(())
    }
}

fn v2_field_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_METADATA_V2_FIELD_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        Err(MetadataError::InvalidField)
    } else {
        Ok(())
    }
}

/// Verify an append-only policy transition. V1 has no reserved classification;
/// its first v2 owner may explicitly classify existing policies as system ones.
/// Once on v2, neither a policy nor its classification can change or disappear.
pub fn validate_schema_extension(historical: &FieldSchema, current: &FieldSchema) -> Result<()> {
    if historical == current {
        return Ok(());
    }
    if current.contract_version != METADATA_CONTRACT_VERSION_V2 {
        return Err(MetadataError::SchemaMismatch);
    }
    for (field, policy) in &historical.fields {
        if current.fields.get(field) != Some(policy)
            || (historical.contract_version == METADATA_CONTRACT_VERSION_V2
                && historical.system_fields.contains(field)
                    != current.system_fields.contains(field))
        {
            return Err(MetadataError::SchemaMismatch);
        }
    }
    Ok(())
}

/// Plan an explicit append-only union. Re-declaring an existing identical policy
/// is idempotent, but duplicate input names or policy changes fail closed. For
/// v1 migration, system_additions explicitly classifies old reserved policies.
/// No document, retained fingerprint or source revision is modified here.
pub fn plan_schema_extension(
    current: &FieldSchema,
    external_additions: Vec<(String, FieldPolicy)>,
    system_additions: Vec<(String, FieldPolicy)>,
) -> Result<FieldSchema> {
    if external_additions
        .len()
        .saturating_add(system_additions.len())
        > MAX_METADATA_JSON_NODES
    {
        return Err(MetadataError::JsonNodeBudget);
    }
    let mut policies = current.fields.clone();
    let mut reserved = current.system_fields.clone();
    let mut seen = BTreeSet::new();
    for (system, additions) in [(false, external_additions), (true, system_additions)] {
        for (field, policy) in additions {
            if !seen.insert(field.clone()) {
                return Err(MetadataError::DuplicateField);
            }
            if let Some(existing) = current.fields.get(&field) {
                if *existing != policy
                    || (current.contract_version == METADATA_CONTRACT_VERSION_V2
                        && current.system_fields.contains(&field) != system)
                {
                    return Err(MetadataError::SchemaMismatch);
                }
            }
            if system {
                reserved.insert(field.clone());
            }
            policies.insert(field, policy);
        }
    }
    let (system, external): (Vec<_>, Vec<_>) = policies
        .into_iter()
        .partition(|(field, _)| reserved.contains(field));
    let proposed = FieldSchema::new_v2(external, system)?;
    validate_schema_extension(current, &proposed)?;
    Ok(proposed)
}

/// Revalidate a historical revision for a current-union read or write plan.
/// The returned copy uses the current fingerprint, but keeps values, evidence,
/// attribution and revision exactly. The historical document is never mutated;
/// owners must retain it with its original schema fingerprint and not persist
/// read-time copies as new source revisions.
pub fn revalidate_document(
    current: &FieldSchema,
    historical: &FieldSchema,
    document: &ContributionDocument,
) -> Result<ContributionDocument> {
    check_schema(historical, &document.schema_fingerprint)?;
    validate_schema_extension(historical, current)?;
    current.validate_field_set(document.fields.keys().map(String::as_str), true)?;
    for (field, value) in &document.fields {
        // Validate shape/nullability without re-normalizing retained bytes.
        current.normalize(field, value.clone(), true)?;
    }
    ContributionDocument::validated(
        current,
        document.key.clone(),
        document.kind,
        document.revision,
        document.fields.clone(),
        document.evidence.clone(),
    )
}

/// Complete replacement of an external contribution. This is intentionally not
/// encoded as remove+set patch edits: 100 old fields may become 100 new fields
/// without a 200-edit patch or an inflated patch limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContributionReplacement {
    schema_fingerprint: String,
    key: ContributionKey,
    kind: SourceKind,
    expected_revision: i64,
    fields: BTreeMap<String, Value>,
    evidence: Value,
}

impl ContributionReplacement {
    pub fn from_json(
        schema: &FieldSchema,
        key: ContributionKey,
        kind: SourceKind,
        expected_revision: i64,
        fields_json: &str,
        evidence_json: &str,
    ) -> Result<Self> {
        revision_number(expected_revision, true)?;
        if kind == SourceKind::LegacyImport {
            return Err(MetadataError::LegacyReconciliationRequired);
        }
        let document =
            ContributionDocument::from_json(schema, key, kind, 1, fields_json, evidence_json)?;
        Ok(Self {
            schema_fingerprint: document.schema_fingerprint,
            key: document.key,
            kind,
            expected_revision,
            fields: document.fields,
            evidence: document.evidence,
        })
    }

    pub fn key(&self) -> &ContributionKey {
        &self.key
    }

    pub const fn kind(&self) -> SourceKind {
        self.kind
    }

    pub const fn expected_revision(&self) -> i64 {
        self.expected_revision
    }

    pub fn schema_fingerprint(&self) -> &str {
        &self.schema_fingerprint
    }

    pub fn fields(&self) -> &BTreeMap<String, Value> {
        &self.fields
    }

    pub fn evidence(&self) -> &Value {
        &self.evidence
    }
}

/// Preserve reserved values while atomically replacing all external fields.
/// Empty replacement deletes the external contribution, not its source revision
/// or its system fields. The owner must commit the exact CAS in its transaction.
pub fn plan_replace(
    schema: &FieldSchema,
    current: Option<&ContributionDocument>,
    replacement: &ContributionReplacement,
) -> Result<ContributionDocument> {
    check_schema(schema, &replacement.schema_fingerprint)?;
    if let Some(current) = current {
        check_schema(schema, &current.schema_fingerprint)?;
        if current.key != replacement.key {
            return Err(MetadataError::SourceMismatch);
        }
        if current.kind != replacement.kind {
            return Err(MetadataError::SourceKindMismatch);
        }
    }
    let actual = current.map_or(0, |document| document.revision);
    let revision = next_revision(replacement.expected_revision, actual)?;
    let mut fields = replacement.fields.clone();
    if let Some(current) = current {
        for field in &schema.system_fields {
            if let Some(value) = current.fields.get(field) {
                fields.insert(field.clone(), value.clone());
            }
        }
    }
    ContributionDocument::validated(
        schema,
        replacement.key.clone(),
        replacement.kind,
        revision,
        fields,
        replacement.evidence.clone(),
    )
}

pub(super) fn json_nodes(value: &Value) -> Result<usize> {
    let mut count = 0usize;
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        count += 1;
        if count > MAX_METADATA_JSON_NODES {
            return Err(MetadataError::JsonNodeBudget);
        }
        match value {
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => pending.extend(values.values()),
            _ => {}
        }
    }
    Ok(count)
}

/// Index observed fields once instead of scanning every document for every
/// union policy. Schema, inputs and output remain byte/node bounded, and work
/// is proportional to bounded input nodes plus union policies, not their product.
pub(super) fn resolve_v2(
    schema: &FieldSchema,
    subject: &str,
    documents: &[ContributionDocument],
) -> Result<MetadataResolution> {
    identifier(subject)?;
    if documents.len() > MAX_METADATA_DOCUMENTS {
        return Err(MetadataError::SourceBudget);
    }
    let payload = schema.v2_payload();
    let mut bytes = crate::encode_with(&payload, false, true).len();
    let mut nodes = json_nodes(&payload)?;
    let mut sorted = BTreeMap::new();
    let mut locals = 0usize;
    for document in documents {
        check_schema(schema, &document.schema_fingerprint)?;
        if document.key.subject != subject {
            return Err(MetadataError::SubjectMismatch);
        }
        if sorted.insert(&document.key, document).is_some() {
            return Err(MetadataError::DuplicateSource);
        }
        bytes = bytes
            .checked_add(document.encoded_bytes)
            .ok_or(MetadataError::SnapshotBudget)?;
        if bytes > MAX_METADATA_SNAPSHOT_BYTES {
            return Err(MetadataError::SnapshotBudget);
        }
        nodes = nodes
            .checked_add(json_nodes(
                &serde_json::to_value(document).expect("validated document"),
            )?)
            .ok_or(MetadataError::JsonNodeBudget)?;
        if nodes > MAX_METADATA_JSON_NODES {
            return Err(MetadataError::JsonNodeBudget);
        }
        if document.kind == SourceKind::Local {
            locals += 1;
        }
    }
    if locals > 1 {
        return Err(MetadataError::MultipleLocalSources);
    }

    // Each field stores only its strongest tier. BTreeMap order makes candidate
    // and source ordering identical to v1, regardless of input document order.
    type Groups = BTreeMap<String, ConflictCandidate>;
    let mut observed: BTreeMap<&str, (u8, ResolutionOrigin, Groups)> = BTreeMap::new();
    for document in sorted.values() {
        let (rank, origin) = match document.kind {
            SourceKind::Local => (0, ResolutionOrigin::Local),
            SourceKind::LegacyImport => (1, ResolutionOrigin::RetainedLegacy),
            SourceKind::Provider => (2, ResolutionOrigin::Provider),
            SourceKind::Scan => (3, ResolutionOrigin::Scan),
        };
        for (field, value) in &document.fields {
            let entry = observed
                .entry(field)
                .or_insert_with(|| (rank, origin, BTreeMap::new()));
            if entry.0 < rank {
                continue;
            }
            if entry.0 > rank {
                *entry = (rank, origin, BTreeMap::new());
            }
            let identity = crate::encode_with(value, false, true);
            entry
                .2
                .entry(identity)
                .or_insert_with(|| ConflictCandidate {
                    value: value.clone(),
                    sources: Vec::new(),
                })
                .sources
                .push(SourceRevision::from(*document));
        }
    }
    let mut fields = BTreeMap::new();
    for field in schema.fields.keys() {
        let resolution = match observed.remove(field.as_str()) {
            None => FieldResolution::Absent,
            Some((_, origin, groups)) if groups.len() == 1 => {
                let candidate = groups.into_values().next().expect("one candidate");
                FieldResolution::Resolved {
                    value: candidate.value,
                    origin,
                    sources: candidate.sources,
                }
            }
            Some((_, origin, groups)) => FieldResolution::Conflict {
                origin,
                candidates: groups.into_values().collect(),
            },
        };
        fields.insert(field.clone(), resolution);
    }
    let resolution = MetadataResolution {
        contract_version: METADATA_CONTRACT_VERSION_V2,
        schema_fingerprint: schema.fingerprint.clone(),
        subject: subject.to_owned(),
        fields,
        inputs: sorted
            .values()
            .map(|document| SourceRevision::from(*document))
            .collect(),
    };
    let output = serde_json::to_value(&resolution).expect("validated resolution");
    if crate::encode_with(&output, false, true).len() > MAX_METADATA_SNAPSHOT_BYTES {
        return Err(MetadataError::SnapshotBudget);
    }
    json_nodes(&output)?;
    Ok(resolution)
}

#[cfg(test)]
mod tests;
