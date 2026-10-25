// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Atomic fresh observation discovery through existing owners. This is not an
//! import activation: no source freeze, cutover ledger or admission claim exists.
use crate::{
    Error, Store,
    catalog_identity_mutation::IdentityAuthority,
    catalog_import_typed::{
        self, ImportedRelationship, NativeIdentityBatch, RelationshipImportAuthority,
        TypedAuthority, TypedBatch,
    },
};
use catabolic_core::catalog_command::CommandEnvelope;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
/// Exact trusted host allocation and source scope, never request-deserialized.
#[derive(Clone, Debug)]
pub struct FreshMediaDiscoveryAuthority {
    pub host_namespace: String,
    pub new_item_ids: BTreeSet<String>,
    pub physical_files: BTreeMap<String, BTreeSet<String>>,
    pub identity: IdentityAuthority,
    pub relationship_edges: BTreeSet<(String, String, String)>,
    pub measured_originals: BTreeMap<String, MeasuredOriginalGrant>,
}
/// Host probe evidence for this exact native occurrence. A duration from an
/// unrelated revision or another file never authorizes placement/readiness.
#[derive(Clone, Debug)]
pub struct MeasuredOriginalGrant {
    pub file: catabolic_core::catalog_identity::ExactFileRevision,
    pub duration_us: i64,
    pub observed_ctime_ns: i64,
}
pub(crate) fn validate(
    store: &Store,
    command: &CommandEnvelope,
    items: &[Value],
    native: &NativeIdentityBatch,
    relationships: &[ImportedRelationship],
    authority: &FreshMediaDiscoveryAuthority,
) -> crate::Result<TypedAuthority> {
    let ids = items
        .iter()
        .map(|item| {
            item["id"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error("discovery requires explicit item IDs".into()))
        })
        .collect::<crate::Result<BTreeSet<_>>>()?;
    if authority.host_namespace != command.key().host_namespace()
        || ids != authority.new_item_ids
        || ids.len() != items.len()
        || ids.is_empty()
        || ids.len() > 128
        || native.mutations.is_empty()
        || native.occurrences.is_empty()
        || native.associations.is_empty()
    {
        return Err(Error(
            "discovery item allocation or host authority mismatch".into(),
        ));
    }
    use catabolic_core::catalog_identity::{IntervalUs, SourceExtent, VersionState};
    let mut measured_used = BTreeSet::new();
    for raw in &native.mutations {
        let m: crate::catalog_identity_mutation::IdentityMutation =
            serde_json::from_value(raw.clone())?;
        if m.context.expected_revision.is_some()
            || !m.evidence.is_empty()
            || m.versions.is_empty()
            || m.timelines.iter().any(|t| {
                t.expected_revision.is_some()
                    || t.identity_state
                        != crate::catalog_identity_mutation::TimelineState::Provisional
            })
        {
            return Err(Error(
                "discovery requires fresh provisional context/timelines with no equivalence".into(),
            ));
        }
        let mut used_timelines = BTreeSet::new();
        for v in &m.versions {
            if v.expected_revision.is_some()
                || v.equivalence.is_some()
                || v.version.bindings.len() != 1
                || v.expected_timeline_revision != 1
                || !used_timelines.insert(&v.version.timeline_id)
            {
                return Err(Error(
                    "discovery requires one new own-file version per fresh timeline".into(),
                ));
            }
            let timeline = m
                .timelines
                .iter()
                .find(|t| {
                    t.timeline_id == v.version.timeline_id
                        && t.edition_item_id == v.version.edition_item_id
                })
                .ok_or_else(|| Error("discovery version lacks its exact new timeline".into()))?;
            let binding = &v.version.bindings[0];
            if let Some(measurement) = authority.measured_originals.get(&v.version.version_id) {
                if store.schema_version < 39 {
                    return Err(Error(
                        "measured discovery requires native39 ctime evidence".into(),
                    ));
                }
                use rusqlite::OptionalExtension;
                let current_ctime: Option<i64> = store.db.query_row(
                    "SELECT c.ctime_ns FROM native_source_ctimes c JOIN observations o ON o.profile=c.profile AND o.file_id=c.file_id AND o.size=c.size AND o.mtime_ns=c.mtime_ns AND o.device=c.device AND o.inode=c.inode WHERE c.profile=? AND c.file_id=? AND o.status='present'",
                    [&m.profile,&binding.file.file_id], |r|r.get(0)).optional()?;
                if current_ctime != Some(measurement.observed_ctime_ns) {
                    return Err(Error(
                        "measured discovery ctime differs from exact native observation".into(),
                    ));
                }
                let interval = IntervalUs::new(0, measurement.duration_us)
                    .map_err(|e| Error(e.to_string()))?;
                if measurement.file != binding.file
                    || v.version.state != VersionState::Ready
                    || binding.source
                        != (SourceExtent::WholeFile {
                            duration_us: Some(measurement.duration_us),
                        })
                    || binding.timeline_interval != Some(interval)
                    || v.version.declared_coverage != Some(interval)
                    || timeline.coverage != Some(interval)
                    || binding.part != 1
                {
                    return Err(Error(
                        "measured discovery does not match exact own-file duration and coverage"
                            .into(),
                    ));
                }
                measured_used.insert(v.version.version_id.clone());
            } else if v.version.state != VersionState::Draft
                || v.version.declared_coverage.is_some()
                || timeline.coverage.is_some()
                || binding.timeline_interval.is_some()
                || binding.source != (SourceExtent::WholeFile { duration_us: None })
            {
                return Err(Error(
                    "unmeasured discovery must remain draft with unknown whole-file extent".into(),
                ));
            }
        }
        if used_timelines.len() != m.timelines.len() {
            return Err(Error("discovery has unused timelines".into()));
        }
    }
    if measured_used != authority.measured_originals.keys().cloned().collect() {
        return Err(Error(
            "discovery includes unused measured-original grants".into(),
        ));
    }
    let batch = TypedBatch {
        native: Some(native.clone()),
        relationships: relationships.to_vec(),
        ..Default::default()
    };
    batch.validate().map_err(|e| e.error)?;
    Ok(TypedAuthority {
        physical_files: authority.physical_files.clone(),
        identity: Some(authority.identity.clone()),
        relationships: Some(RelationshipImportAuthority {
            host_namespace: authority.host_namespace.clone(),
            edges: authority.relationship_edges.clone(),
        }),
        ..Default::default()
    })
}
pub(crate) fn apply(
    store: &Store,
    command: &CommandEnvelope,
    native: NativeIdentityBatch,
    relationships: Vec<ImportedRelationship>,
    authority: &FreshMediaDiscoveryAuthority,
    typed: &TypedAuthority,
    time: i64,
) -> crate::Result<Vec<Value>> {
    let batch = TypedBatch {
        native: Some(native),
        relationships,
        ..Default::default()
    };
    catalog_import_typed::apply_with_origin(
        store,
        command,
        &[batch],
        &authority.new_item_ids,
        Some(typed),
        time,
        "explicit",
    )
    .map_err(|e| e.error)
}
