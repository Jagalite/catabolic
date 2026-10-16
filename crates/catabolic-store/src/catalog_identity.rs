// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Native identity composition through existing query, component and fallback
//! owners. These are trusted store APIs, not an authentication boundary.
//!
//! On schema 32, a privileged host may supply a complete revision-pinned
//! manifest. It establishes no persistent timeline or evidence authority.
//! On schema 33, supplied authoritative manifests are refused: `resolve_persisted`
//! loads retained typed ownership and complete editorial context membership from
//! the durable identity owner. Both paths are read-only metadata composition;
//! neither admits interval publication, muxing or playback effects.
use crate::{
    Error, Result, Store, component_mutation, fallback_mutation,
    fallback_resolution::{self, Context, Probe, Resolver},
    migration::rows,
    selection::Evaluation,
};
use catabolic_core::{
    catalog_identity::{
        EquivalenceEvidence, IDENTITY_CONTRACT_VERSION, IntervalUs, SegmentationSet, SourceExtent,
        VersionBinding, VersionCandidate, VersionContract, VersionState, select_complete_version,
        select_explicit_version, validate_segmentation_set, validate_version,
    },
    component, encode, sha256,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestVersion {
    pub version: VersionContract,
    pub equivalence: Option<EquivalenceEvidence>,
}

/// All versions referenced by the resolution and every member of its explicit
/// segmentation sets, within one editorial segmentation context. Epoch and
/// catalog identity fence the supplied snapshot. Separate privileged manifests
/// do not establish durable/global nonoverlap before the typed storage owner.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogManifest {
    pub contract_version: u32,
    pub database_id: String,
    pub expected_epoch: i64,
    pub versions: Vec<ManifestVersion>,
    pub segmentation_sets: Vec<SegmentationSet>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VersionSelection {
    Explicit { version_id: String },
    Equivalent { candidates: Vec<String> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionRequest {
    pub edition_item_id: String,
    pub timeline_id: String,
    pub coverage: IntervalUs,
    /// Preference order applies to whole versions, never individual parts.
    pub selection: VersionSelection,
    pub policy_id: String,
    pub catalog: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResolvedPart {
    pub binding: VersionBinding,
    pub components: Vec<Value>,
    /// Existing packaging decision is diagnostic only: native interval-aware
    /// publication has not been admitted by this read-only bridge.
    pub legacy_packaging: Value,
    pub content_path: Option<String>,
    pub requires_interval_enforcement: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct VersionResolution {
    pub contract_version: u32,
    pub manifest_digest: String,
    pub epoch: i64,
    pub selected_version_id: Option<String>,
    pub parts: Vec<ResolvedPart>,
    pub attempted_versions: Vec<String>,
    pub publication_ready: bool,
}

fn fail(code: &str) -> Error {
    Error(code.into())
}

fn identity<T>(value: catabolic_core::catalog_identity::Result<T>) -> Result<T> {
    value.map_err(|e| fail(e.code()))
}

/// Validate complete owner sets, exact associations/revisions and explicit
/// segmentation membership against the actual catalog. No metadata label is
/// accepted as evidence of native timeline ownership or equivalence.
pub fn validate_manifest(store: &Store, profile: &str, manifest: &CatalogManifest) -> Result<()> {
    validate_manifest_inner(store, profile, manifest, false)
}

fn validate_manifest_inner(
    store: &Store,
    profile: &str,
    manifest: &CatalogManifest,
    authoritative: bool,
) -> Result<()> {
    if !authoritative && store.schema_version >= 33 {
        return Err(fail("catalog_identity_authoritative_records_required"));
    }
    if manifest.contract_version != IDENTITY_CONTRACT_VERSION {
        return Err(fail("catalog_identity_unsupported_contract"));
    }
    if manifest.database_id != store.database_id {
        return Err(fail("catalog_identity_catalog_mismatch"));
    }
    fallback_resolution::require_epoch(store, manifest.expected_epoch)?;
    if manifest.versions.is_empty()
        || manifest.versions.len() > 64
        || manifest.segmentation_sets.len() > 64
        || manifest
            .segmentation_sets
            .iter()
            .map(|set| set.segments.len())
            .sum::<usize>()
            > 1024
        || manifest
            .versions
            .iter()
            .map(|v| v.version.bindings.len())
            .sum::<usize>()
            > 1024
    {
        return Err(fail("catalog_identity_manifest_budget"));
    }
    let mut versions = BTreeSet::new();
    let mut owners = BTreeSet::new();
    let mut bindings = BTreeMap::new();
    let mut associations = BTreeSet::new();
    for entry in &manifest.versions {
        let version = &entry.version;
        identity(validate_version(version))?;
        if !authoritative && version.state != VersionState::Ready {
            return Err(fail("catalog_identity_version_not_ready"));
        }
        if !versions.insert(&version.version_id) || !owners.insert(&version.association_item_id) {
            return Err(fail("catalog_identity_duplicate_version_owner"));
        }
        for id in [&version.association_item_id, &version.edition_item_id] {
            if !store
                .db
                .prepare("SELECT 1 FROM items WHERE id=?")?
                .exists([id])?
            {
                return Err(fail("catalog_identity_missing_owner"));
            }
        }
        let active = rows(
            &store.db,
            "SELECT id,file_id,part FROM item_files WHERE item_id=? AND role='primary' AND active=1 ORDER BY id LIMIT 1025",
            &[version.association_item_id.clone().into()],
        )?;
        if active.len() != version.bindings.len() {
            return Err(fail("catalog_identity_binding_set_mismatch"));
        }
        for binding in &version.bindings {
            if bindings.insert(&binding.binding_id, binding).is_some()
                || !associations.insert(&binding.association_id)
            {
                return Err(fail("catalog_identity_duplicate_binding"));
            }
            if !active.iter().any(|row| {
                row["id"] == binding.association_id
                    && row["file_id"] == binding.file.file_id
                    && row["part"].as_u64() == Some(u64::from(binding.part))
            }) {
                return Err(fail("catalog_identity_binding_set_mismatch"));
            }
            let snapshot = crate::media_mutation::snapshot(store, profile, &binding.file.file_id)?;
            if crate::public_components::revision(&snapshot)? != binding.file.revision {
                return Err(fail("catalog_identity_stale_file_revision"));
            }
            // A conflicting primary is an integrity refusal, not permission to
            // hide it by selecting the next version. Missing probe evidence may
            // still be an unavailable candidate handled by the existing owner.
            for row in rows(
                &store.db,
                &format!(
                    "SELECT occurrence_id FROM ({}) WHERE profile=? AND association_id=? AND kind='video' AND current=1 LIMIT 257",
                    crate::query::view_sql("catalog_component_occurrences")
                ),
                &[
                    profile.to_owned().into(),
                    binding.association_id.clone().into(),
                ],
            )? {
                component_mutation::get(
                    store,
                    profile,
                    row["occurrence_id"].as_str().unwrap_or(""),
                    true,
                )?;
            }
        }
    }
    let mut sets = BTreeSet::new();
    let mut segmented_revisions = BTreeSet::new();
    let mut segmented = BTreeSet::new();
    for set in &manifest.segmentation_sets {
        identity(validate_segmentation_set(set))?;
        if set.segments.is_empty() || !sets.insert(&set.segmentation_set_id) {
            return Err(fail("catalog_identity_invalid_segmentation_set"));
        }
        if !segmented_revisions.insert((&set.file.file_id, &set.file.revision)) {
            return Err(fail("catalog_identity_duplicate_segmentation_revision"));
        }
        for segment in &set.segments {
            let Some(binding) = bindings.get(&segment.binding_id) else {
                return Err(fail("catalog_identity_segmentation_member_missing"));
            };
            if !segmented.insert(&segment.binding_id)
                || binding.file != segment.file
                || binding.source
                    != (SourceExtent::Slice {
                        interval: segment.source_interval,
                    })
            {
                return Err(fail("catalog_identity_segmentation_binding_mismatch"));
            }
        }
    }
    if bindings.values().any(|binding| {
        matches!(binding.source, SourceExtent::Slice { .. })
            && !segmented.contains(&binding.binding_id)
    }) {
        return Err(fail("catalog_identity_slice_requires_segmentation_set"));
    }
    fallback_resolution::require_epoch(store, manifest.expected_epoch)
}

fn selected(
    request: &VersionRequest,
    candidates: &[&ManifestVersion],
    available: &[Vec<String>],
) -> Result<Option<usize>> {
    if let VersionSelection::Explicit { version_id } = &request.selection {
        let version = &candidates[0].version;
        if version.edition_item_id != request.edition_item_id
            || version.timeline_id != request.timeline_id
            || version.declared_coverage != Some(request.coverage)
        {
            return Err(fail("catalog_identity_version_scope_mismatch"));
        }
        return identity(select_explicit_version(version_id, version, &available[0]))
            .map(|ready| ready.then_some(0));
    }
    let candidates = candidates
        .iter()
        .zip(available)
        .map(|(entry, ids)| VersionCandidate {
            version: &entry.version,
            equivalence: entry.equivalence.as_ref(),
            available_binding_ids: ids,
        })
        .collect::<Vec<_>>();
    identity(select_complete_version(
        &request.edition_item_id,
        &request.timeline_id,
        request.coverage,
        &candidates,
    ))
}

/// Resolve one complete equivalent version with existing query/component/probe
/// owners. This privileged seam intentionally has no HTTP or legacy adapter.
/// Every candidate is admitted before any probe; a malformed later alternative
/// cannot be hidden by an early winner. No partial plan escapes as a selection.
pub fn resolve_manifest(
    evaluation: &mut Evaluation,
    manifest: &CatalogManifest,
    request: &VersionRequest,
    probe: &mut Probe<'_>,
) -> Result<VersionResolution> {
    resolve_manifest_inner(evaluation, manifest, request, probe, false)
}

/// Resolve using retained typed ownership, never an externally supplied
/// authoritative manifest. Native metadata still does not admit media effects.
pub fn resolve_persisted(
    evaluation: &mut Evaluation,
    request: &VersionRequest,
    probe: &mut Probe<'_>,
) -> Result<VersionResolution> {
    if evaluation.http {
        return Err(fail("catalog_identity_privileged_owner_required"));
    }
    if evaluation.store().schema_version < 33 {
        return Err(fail("catalog_identity_native_schema_required"));
    }
    evaluation.store().transaction_available()?;
    let generation: i64 = evaluation.store().db.query_row(
        "SELECT generation FROM catalog_state WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    let ids = match &request.selection {
        VersionSelection::Explicit { version_id } => std::slice::from_ref(version_id),
        VersionSelection::Equivalent { candidates } => candidates.as_slice(),
    };
    let manifest = crate::catalog_identity_mutation::load_manifest(
        evaluation.store(),
        &evaluation.profile,
        ids,
    )?;
    let result = resolve_manifest_inner(evaluation, &manifest, request, probe, true)?;
    let after: i64 = evaluation.store().db.query_row(
        "SELECT generation FROM catalog_state WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    if generation != after {
        return Err(fail("catalog_identity_stale_generation"));
    }
    evaluation.store().transaction_available()?;
    Ok(result)
}

fn resolve_manifest_inner(
    evaluation: &mut Evaluation,
    manifest: &CatalogManifest,
    request: &VersionRequest,
    probe: &mut Probe<'_>,
    authoritative: bool,
) -> Result<VersionResolution> {
    if evaluation.http {
        return Err(fail("catalog_identity_privileged_owner_required"));
    }
    validate_manifest_inner(
        evaluation.store(),
        &evaluation.profile,
        manifest,
        authoritative,
    )?;
    let requested = match &request.selection {
        VersionSelection::Explicit { version_id } => std::slice::from_ref(version_id),
        VersionSelection::Equivalent { candidates } => candidates.as_slice(),
    };
    if requested.is_empty() || requested.len() > 64 {
        return Err(fail("catalog_identity_candidate_budget"));
    }
    let candidates = requested
        .iter()
        .map(|id| {
            manifest
                .versions
                .iter()
                .find(|entry| entry.version.version_id == *id)
                .ok_or_else(|| fail("catalog_identity_unknown_version"))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut available = vec![Vec::new(); candidates.len()];
    selected(request, &candidates, &available)?;
    let policy =
        fallback_mutation::get(evaluation.store(), &evaluation.profile, &request.policy_id)?;
    if policy["definition"]["package"].is_null() {
        return Err(fail("catalog_identity_component_policy_required"));
    }
    let mut result = VersionResolution {
        contract_version: IDENTITY_CONTRACT_VERSION,
        manifest_digest: sha256(encode(&serde_json::to_value(manifest)?)),
        epoch: manifest.expected_epoch,
        selected_version_id: None,
        parts: vec![],
        attempted_versions: candidates
            .iter()
            .map(|entry| entry.version.version_id.clone())
            .collect(),
        publication_ready: false,
    };
    let mut resolver = Resolver::new(Context::Local(evaluation), probe, None);
    if authoritative {
        resolver.native_identity_read();
    }
    // One existing-owner operation covers every candidate, so its entry,
    // candidate, query, probe and response budgets cannot reset per version.
    let entries = candidates
        .iter()
        .flat_map(|candidate| {
            candidate.version.bindings.iter().map(|binding| {
                json!({
                    "item_id":candidate.version.association_item_id,"part":binding.part
                })
            })
        })
        .collect::<Vec<_>>();
    let plan = resolver.resolve(
        &entries,
        &request.policy_id,
        &request.catalog,
        &BTreeMap::new(),
        false,
    )?;
    if plan["tiers"]
        .as_array()
        .is_none_or(|tiers| tiers.iter().any(|tier| tier["state"] == "blocked"))
    {
        return Err(fail("catalog_identity_candidate_query_blocked"));
    }
    let decisions = plan["decisions"]
        .as_array()
        .ok_or_else(|| fail("catalog_identity_invalid_resolution"))?;
    if decisions.len() != entries.len() {
        return Err(fail("catalog_identity_invalid_resolution"));
    }
    for (index, candidate) in candidates.iter().enumerate() {
        let version = &candidate.version;
        fallback_resolution::require_epoch(resolver.store(), manifest.expected_epoch)?;
        let mut bindings = version.bindings.iter().collect::<Vec<_>>();
        bindings.sort_by_key(|binding| binding.part);
        let mut parts = vec![];
        for binding in bindings {
            let decision = decisions
                .iter()
                .find(|decision| {
                    decision["item_id"] == version.association_item_id
                        && decision["part"].as_u64() == Some(u64::from(binding.part))
                })
                .ok_or_else(|| fail("catalog_identity_invalid_resolution"))?;
            if !decision["state"]
                .as_str()
                .is_some_and(|state| state.starts_with("resolved"))
            {
                continue;
            }
            if decision["item_id"] != version.association_item_id
                || decision["part"].as_u64() != Some(u64::from(binding.part))
                || decision["file_id"] != binding.file.file_id
                || decision["revision"] != binding.file.revision
            {
                return Err(fail("catalog_identity_selected_binding_mismatch"));
            }
            let primary = &plan["captures"][decision["entry_id"].as_str().unwrap_or("")];
            let package = &decision["evidence"]["component_package"];
            let Some(components) = package["components"].as_array().filter(|v| !v.is_empty())
            else {
                return Err(fail("catalog_identity_missing_component_evidence"));
            };
            let video = &components[0]["occurrence"];
            let mut bound = vec![];
            for component in components {
                let occurrence = &component["occurrence"];
                let Some(compatibility) =
                    component::compatible_binding(version, binding, occurrence, video, primary)
                else {
                    return Err(fail("catalog_identity_component_binding_unsupported"));
                };
                let mut component = component.clone();
                component["compatibility"] = compatibility;
                bound.push(component);
            }
            let slice = matches!(binding.source, SourceExtent::Slice { .. });
            parts.push(ResolvedPart {
                binding: binding.clone(),
                components: bound,
                legacy_packaging: package["packaging"].clone(),
                content_path: if slice {
                    None
                } else {
                    decision["content_path"].as_str().map(str::to_owned)
                },
                requires_interval_enforcement: slice,
            });
            available[index].push(binding.binding_id.clone());
        }
        if selected(request, &candidates, &available)? == Some(index) {
            validate_manifest_inner(
                resolver.store(),
                resolver.profile(),
                manifest,
                authoritative,
            )?;
            result.selected_version_id = Some(version.version_id.clone());
            result.parts = parts;
            break;
        }
    }
    fallback_resolution::require_epoch(resolver.store(), manifest.expected_epoch)?;
    resolver.account_value(&serde_json::to_value(&result)?)?;
    Ok(result)
}
