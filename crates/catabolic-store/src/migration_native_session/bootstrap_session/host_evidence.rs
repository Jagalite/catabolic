//! Borrowed host descriptions for the offline bootstrap facade. These are not
//! verified provenance or transferable write capabilities. The embedding host
//! must independently establish the complete archived absence/origin audits.
//! Conversion runs only inside the retained writer, after receipt-first dispatch
//! for execution. Existing baseline/history owners still authenticate every part
//! and command and independently recapture the complete bootstrap inventory.
use crate::{
    Error, Result, Store,
    catalog_import::{ImportBatch, SnapshotManifest},
    catalog_import_typed::PhysicalMapping,
    catalog_nfo_attribution::{
        NfoCreatedSourceAuthority, NfoImportAuthority, NfoImportedOriginAuthority, ProofBudget,
        bootstrap_entry,
    },
    operation_budget::{BudgetCharge, OperationBudget},
};
use catabolic_core::{
    catalog_command::CommandEnvelope,
    catalog_metadata::{MAX_METADATA_EVIDENCE_BYTES, MAX_METADATA_JSON_DEPTH},
};
#[cfg(test)]
use serde_json::Value;
use std::collections::BTreeSet;

/// One exact historical contribution and its complete ordered later commands.
/// No serde conversion or public verified token is provided.
#[derive(Clone, Copy, Debug)]
pub struct HostSourceEvidence<'a> {
    pub baseline: HostBaselineEvidence<'a>,
    pub later_commands: &'a [CommandEnvelope],
}
#[derive(Clone, Copy, Debug)]
pub enum HostBaselineEvidence<'a> {
    /// The complete absence audit independently establishes no automatic NFO
    /// identity and no sidecar attribution before the first API contribution.
    /// The HTTP intent digest is read from that command's exact evidence, not
    /// confused with the distinct native command envelope digest.
    NativeCreated {
        first: &'a CommandEnvelope,
        subject: &'a str,
        complete_absence_audit_digest: &'a str,
    },
    Imported {
        subject: &'a str,
        metadata: HostImportPart<'a>,
        audit: HostImportAudit<'a>,
        origin: HostImportOrigin<'a>,
    },
}
/// Complete immutable part bytes are borrowed, never cloned once per subject.
#[derive(Clone, Copy, Debug)]
pub struct HostImportPart<'a> {
    pub manifest: &'a SnapshotManifest,
    pub batches: &'a [ImportBatch],
    pub activation: &'a CommandEnvelope,
}
/// Positive audit prepared from the host's complete frozen root and origins.
/// Missing current rows or request markers cannot establish this assertion.
#[derive(Clone, Copy, Debug)]
pub struct HostImportAudit<'a> {
    pub root_manifest_digest: Option<&'a str>,
    pub origin_audit_digest: &'a str,
    pub no_origin_subjects: &'a BTreeSet<String>,
}
#[derive(Clone, Copy, Debug)]
pub enum HostImportOrigin<'a> {
    NoOrigin,
    /// Exact historical mapping, not current filesystem/sidecar custody.
    Physical {
        part: HostImportPart<'a>,
        motion_file_id: &'a str,
        physical: &'a PhysicalMapping,
    },
}
impl HostSourceEvidence<'_> {
    fn subject(&self) -> &str {
        match self.baseline {
            HostBaselineEvidence::NativeCreated { subject, .. }
            | HostBaselineEvidence::Imported { subject, .. } => subject,
        }
    }
    fn first(&self) -> &CommandEnvelope {
        match self.baseline {
            HostBaselineEvidence::NativeCreated { first, .. } => first,
            HostBaselineEvidence::Imported { metadata, .. } => metadata.activation,
        }
    }
}
impl<'a> HostImportPart<'a> {
    fn private(self) -> bootstrap_entry::ImportPart<'a> {
        bootstrap_entry::ImportPart {
            manifest: self.manifest,
            batches: self.batches,
            activation: self.activation,
        }
    }
}
fn refused(reason: impl std::fmt::Display) -> Error {
    Error(format!("offline_bootstrap_host_evidence_refused: {reason}"))
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.trim().is_empty()
        && !value.chars().any(char::is_control)
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn command_scope(command: &CommandEnvelope, catalog: &str, host: &str) -> Result<()> {
    if command.key().catalog_id() != catalog || command.key().host_namespace() != host {
        return Err(refused("historical command catalog/host differs"));
    }
    Ok(())
}

/// Borrow only the exact Activate scalar fields. The existing verifier checks
/// the reserved operation key, manifest digest, every descriptor and permanent
/// receipt. Parsing this description alone establishes none of those facts.
#[derive(Clone, Copy)]
struct Activation<'a> {
    import_id: &'a str,
    input_digest: &'a str,
    reference_ack_id: &'a str,
    mapping_digest: &'a str,
}
fn activation<'a>(part: HostImportPart<'a>) -> Result<Activation<'a>> {
    let outer = part.activation.payload();
    if outer.as_object().is_none_or(|o| o.len() != 2) || outer["operation"] != "import" {
        return Err(refused("exact import activation required"));
    }
    let mutation = &outer["mutation"];
    if mutation.as_object().is_none_or(|o| o.len() != 5) || mutation["action"] != "activate" {
        return Err(refused("exact Activate mutation required"));
    }
    let text = |field: &str| {
        mutation[field]
            .as_str()
            .ok_or_else(|| refused("Activate scalar is not text"))
    };
    let result = Activation {
        import_id: text("import_id")?,
        input_digest: text("input_digest")?,
        reference_ack_id: text("reference_ack_id")?,
        mapping_digest: text("mapping_digest")?,
    };
    if !identifier(result.import_id)
        || result.import_id != part.manifest.import_id
        || !identifier(&part.manifest.source_namespace)
        || !identifier(result.reference_ack_id)
        || !digest(result.input_digest)
        || !digest(result.mapping_digest)
    {
        return Err(refused("Activate identity or digest differs from manifest"));
    }
    Ok(result)
}
fn native_evidence(first: &CommandEnvelope) -> Result<&str> {
    let payload = first.payload();
    let value = match payload["operation"].as_str() {
        Some("metadata_contribution_replace") => &payload["request"]["evidence_json"],
        Some("metadata") if payload["mutation"]["change"]["action"] == "patch" => {
            &payload["mutation"]["change"]["evidence_json"]
        }
        Some("metadata_initialize_and_patch") => &payload["evidence_json"],
        _ => return Err(refused("unsupported first native metadata operation")),
    };
    let raw = value
        .as_str()
        .ok_or_else(|| refused("first evidence_json missing"))?;
    if raw.len() > MAX_METADATA_EVIDENCE_BYTES {
        return Err(refused("first evidence exceeds document bound"));
    }
    Ok(raw)
}
/// Called only after complete-list raw admission. This is a real duplicate-safe
/// decode of the exact retained first evidence, not a request digest fallback.
fn native_request_digest(first: &CommandEnvelope) -> Result<String> {
    let evidence = catabolic_core::json::parse(native_evidence(first)?, true).map_err(refused)?;
    if evidence.as_object().is_none_or(|o| o.len() != 3)
        || evidence["owner"] != "motion.metadata.replace"
        || evidence["expected_aggregate_revision"].as_u64().is_none()
    {
        return Err(refused(
            "first evidence is not exact Motion replacement evidence",
        ));
    }
    let intent = evidence["request_digest"]
        .as_str()
        .filter(|value| digest(value))
        .ok_or_else(|| refused("first evidence intent digest missing or malformed"))?;
    Ok(intent.to_owned())
}

enum Audit {
    Native(NfoCreatedSourceAuthority),
    Imported {
        metadata: NfoImportAuthority,
        physical: Option<NfoImportedOriginAuthority>,
    },
}
struct OwnedEvidence<'a> {
    source: HostSourceEvidence<'a>,
    audit: Audit,
}
impl OwnedEvidence<'_> {
    fn borrowed(&self) -> bootstrap_entry::SourceEvidence<'_> {
        let baseline = match (self.source.baseline, &self.audit) {
            (HostBaselineEvidence::NativeCreated { first, .. }, Audit::Native(audit)) => {
                bootstrap_entry::BaselineEvidence::NativeCreated { first, audit }
            }
            (
                HostBaselineEvidence::Imported {
                    subject,
                    metadata,
                    origin,
                    ..
                },
                Audit::Imported {
                    metadata: audit,
                    physical,
                },
            ) => {
                let origin = match (origin, physical) {
                    (HostImportOrigin::NoOrigin, None) => {
                        bootstrap_entry::ImportOriginEvidence::NoOrigin
                    }
                    (HostImportOrigin::Physical { part, .. }, Some(audit)) => {
                        bootstrap_entry::ImportOriginEvidence::Physical {
                            part: part.private(),
                            audit,
                        }
                    }
                    _ => unreachable!("adapter constructs matching origin audit"),
                };
                bootstrap_entry::BaselineEvidence::Imported {
                    subject,
                    metadata: metadata.private(),
                    audit,
                    origin,
                }
            }
            _ => unreachable!("adapter constructs matching baseline audit"),
        };
        bootstrap_entry::SourceEvidence {
            baseline,
            later_commands: self.source.later_commands,
        }
    }
}

fn materialize<'a>(
    catalog: &str,
    host: &str,
    descriptions: &[HostSourceEvidence<'a>],
    shared: OperationBudget,
) -> Result<Vec<OwnedEvidence<'a>>> {
    if !identifier(host) {
        return Err(refused("invalid host namespace"));
    }
    // Fixed Vec/index storage is charged in addition to the streamed represented
    // strings below. Both complete collections are admitted before any reserve.
    let unit = std::mem::size_of::<OwnedEvidence<'a>>()
        .checked_add(std::mem::size_of::<bootstrap_entry::SourceEvidence<'a>>())
        .and_then(|n| n.checked_add(4 * std::mem::size_of::<&str>()))
        .ok_or_else(|| refused("evidence storage overflow"))?;
    let bytes = descriptions
        .len()
        .checked_mul(unit)
        .ok_or_else(|| refused("evidence list allocation overflow"))?;
    shared
        .charge(BudgetCharge {
            items: descriptions.len(),
            bytes,
            values: descriptions.len(),
            work: descriptions.len(),
        })
        .map_err(|failure| refused(format!("complete list admission: {failure:?}")))?;
    let mut budget = ProofBudget::new(shared);
    for source in descriptions {
        if !identifier(source.subject()) {
            return Err(refused("invalid exact source subject"));
        }
        command_scope(source.first(), catalog, host)?;
        // Only borrowed bookkeeping is read here; child owners independently
        // admit full roots/history when they really decode/hash those bytes.
        budget.serialized(
            &(
                source.first().key(),
                source.first().digest().as_str(),
                source.later_commands.len(),
            ),
            4,
        )?;
        for command in source.later_commands {
            budget.serialized(&(command.key(), command.digest().as_str()), 4)?;
            command_scope(command, catalog, host)?;
        }
        match source.baseline {
            HostBaselineEvidence::NativeCreated {
                first,
                subject,
                complete_absence_audit_digest,
            } => {
                if !digest(complete_absence_audit_digest) {
                    return Err(refused(
                        "independent complete absence audit digest required",
                    ));
                }
                budget.raw_json(native_evidence(first)?, MAX_METADATA_JSON_DEPTH)?;
                // The fourth string reserves the 64-byte decoded intent clone;
                // first.digest() is used ONLY for its exact identical size, never
                // as the semantic request_digest or a substitute audit.
                budget.serialized(
                    &(
                        host,
                        subject,
                        complete_absence_audit_digest,
                        first.digest().as_str(),
                    ),
                    4,
                )?;
            }
            HostBaselineEvidence::Imported {
                subject,
                metadata,
                audit,
                origin,
            } => {
                if !digest(audit.origin_audit_digest)
                    || audit.root_manifest_digest.is_some_and(|v| !digest(v))
                {
                    return Err(refused("independent archived origin audit required"));
                }
                let activation = activation(metadata)?;
                budget.serialized(
                    &(
                        host,
                        audit.root_manifest_digest,
                        activation.import_id,
                        activation.input_digest,
                        &metadata.manifest.source_namespace,
                        activation.reference_ack_id,
                        activation.mapping_digest,
                        audit.origin_audit_digest,
                        audit.no_origin_subjects,
                    ),
                    4,
                )?;
                for subject in audit.no_origin_subjects {
                    if !identifier(subject) {
                        return Err(refused("invalid no-origin subject"));
                    }
                }
                match origin {
                    HostImportOrigin::NoOrigin if audit.no_origin_subjects.contains(subject) => {}
                    HostImportOrigin::Physical {
                        part,
                        motion_file_id,
                        physical,
                    } if !audit.no_origin_subjects.contains(subject) => {
                        command_scope(part.activation, catalog, host)?;
                        let physical_activation = self::activation(part)?;
                        if !identifier(motion_file_id) {
                            return Err(refused("invalid archived Motion file ID"));
                        }
                        budget.serialized(
                            &(
                                host,
                                audit.root_manifest_digest,
                                activation.import_id,
                                activation.input_digest,
                                physical_activation.import_id,
                                physical_activation.input_digest,
                                physical_activation.mapping_digest,
                                audit.origin_audit_digest,
                                subject,
                                motion_file_id,
                                physical,
                            ),
                            64,
                        )?;
                    }
                    _ => {
                        return Err(refused(
                            "positive origin audit disagrees with selected origin cohort",
                        ));
                    }
                }
            }
        }
    }
    let mut seen = BTreeSet::new();
    for source in descriptions {
        if !seen.insert(source.subject()) {
            return Err(refused("duplicate source subject"));
        }
    }
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(descriptions.len())
        .map_err(refused)?;
    for source in descriptions {
        let audit = match source.baseline {
            HostBaselineEvidence::NativeCreated {
                first,
                subject,
                complete_absence_audit_digest,
            } => Audit::Native(NfoCreatedSourceAuthority {
                host_namespace: host.to_owned(),
                subject: subject.to_owned(),
                request_digest: native_request_digest(first)?,
                complete_absence_audit_digest: complete_absence_audit_digest.to_owned(),
            }),
            HostBaselineEvidence::Imported {
                subject,
                metadata,
                audit,
                origin,
            } => {
                let activated = activation(metadata)?;
                let metadata_audit = NfoImportAuthority {
                    host_namespace: host.to_owned(),
                    root_manifest_digest: audit.root_manifest_digest.map(str::to_owned),
                    import_id: activated.import_id.to_owned(),
                    input_digest: activated.input_digest.to_owned(),
                    source_namespace: metadata.manifest.source_namespace.clone(),
                    reference_ack_id: activated.reference_ack_id.to_owned(),
                    mapping_digest: activated.mapping_digest.to_owned(),
                    origin_audit_digest: audit.origin_audit_digest.to_owned(),
                    no_origin_subjects: audit.no_origin_subjects.clone(),
                };
                let physical = match origin {
                    HostImportOrigin::NoOrigin => None,
                    HostImportOrigin::Physical {
                        part,
                        motion_file_id,
                        physical,
                    } => {
                        let physical_activation = activation(part)?;
                        Some(NfoImportedOriginAuthority {
                            host_namespace: host.to_owned(),
                            root_manifest_digest: audit.root_manifest_digest.map(str::to_owned),
                            metadata_import_id: activated.import_id.to_owned(),
                            metadata_input_digest: activated.input_digest.to_owned(),
                            physical_import_id: physical_activation.import_id.to_owned(),
                            physical_input_digest: physical_activation.input_digest.to_owned(),
                            physical_mapping_digest: physical_activation.mapping_digest.to_owned(),
                            origin_audit_digest: audit.origin_audit_digest.to_owned(),
                            subject: subject.to_owned(),
                            motion_file_id: motion_file_id.to_owned(),
                            physical: physical.clone(),
                        })
                    }
                };
                Audit::Imported {
                    metadata: metadata_audit,
                    physical,
                }
            }
        };
        owned.push(OwnedEvidence {
            source: *source,
            audit,
        });
    }
    Ok(owned)
}

/// Internal lexical borrowing only. Call from the session's existing writer:
/// preview may use it directly; execution MUST reach it only after receipt-first
/// replay/expiry resolution. No private audit or verified token is returned.
pub(super) fn with_evidence_in<R>(
    store: &Store,
    host: &str,
    descriptions: &[HostSourceEvidence<'_>],
    use_evidence: impl FnOnce(&[bootstrap_entry::SourceEvidence<'_>]) -> Result<R>,
) -> Result<R> {
    with_evidence_in_with_control_workspace(store, host, descriptions, use_evidence, None)
}
pub(super) fn with_evidence_in_with_control_workspace<R>(
    store: &Store, host: &str, descriptions: &[HostSourceEvidence<'_>],
    use_evidence: impl FnOnce(&[bootstrap_entry::SourceEvidence<'_>]) -> Result<R>,
    control: Option<&crate::migration::BootstrapWorkspace<'_>>,
) -> Result<R> {
    if control.is_some() && !descriptions.is_empty() {
        return Err(refused("control workspace requires empty bootstrap evidence"));
    }
    if store.schema_version != 41 || !store.writable || store.db.is_autocommit() {
        return Err(refused("offline retained native41 writer required"));
    }
    store.transaction_available_with_control_workspace(control)?;
    let owned = materialize(
        &store.database_id,
        host,
        descriptions,
        store.catalog_operation_budget()?,
    )?;
    let mut views = Vec::new();
    views.try_reserve_exact(owned.len()).map_err(refused)?;
    views.extend(owned.iter().map(OwnedEvidence::borrowed));
    use_evidence(&views)
}

#[cfg(test)]
mod tests {
    // Description-adapter tests only. These constructed envelopes are NOT
    // committed import/source proof; real receipt/owner fixtures exercise the
    // consuming session entry independently.
    use super::*;
    use crate::operation_budget::BudgetLimits;
    use catabolic_core::catalog_command::CommandKey;
    use serde_json::json;
    use std::time::Duration;

    const AUDIT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const INTENT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    fn budget() -> OperationBudget {
        OperationBudget::new(
            Duration::from_secs(30),
            BudgetLimits {
                items: 8192,
                values: 65536,
                bytes: 16 * 1024 * 1024,
                work: 65536,
            },
        )
        .unwrap()
    }
    fn envelope(id: &str, payload: Value) -> CommandEnvelope {
        CommandEnvelope::new(
            1,
            CommandKey::new("catalog", "host", id).unwrap(),
            "incarnation",
            1,
            &payload.to_string(),
        )
        .unwrap()
    }
    fn evidence() -> String {
        json!({"owner":"motion.metadata.replace","request_digest":INTENT,
            "expected_aggregate_revision":7})
        .to_string()
    }
    fn native(first: &CommandEnvelope) -> HostSourceEvidence<'_> {
        HostSourceEvidence {
            baseline: HostBaselineEvidence::NativeCreated {
                first,
                subject: "work",
                complete_absence_audit_digest: AUDIT,
            },
            later_commands: &[],
        }
    }
    fn manifest(id: &str) -> SnapshotManifest {
        SnapshotManifest {
            contract_version: 3,
            import_id: id.into(),
            source_namespace: "motion.schema68.export.v4".into(),
            source_snapshot_id: "frozen-original".into(),
            batches: vec![],
        }
    }
    fn activate(manifest: &SnapshotManifest) -> CommandEnvelope {
        envelope(
            &manifest.import_id,
            json!({"operation":"import", "mutation": {
                "action":"activate", "import_id":manifest.import_id, "input_digest":INTENT,
                "reference_ack_id":"saved-reference-ack", "mapping_digest":AUDIT,
            }}),
        )
    }
    fn part<'a>(
        manifest: &'a SnapshotManifest,
        activation: &'a CommandEnvelope,
    ) -> HostImportPart<'a> {
        HostImportPart {
            manifest,
            activation,
            batches: &[],
        }
    }

    #[test]
    fn native_description_derives_http_intent_not_envelope_digest_for_all_first_forms() {
        let raw = evidence();
        for payload in [
            json!({"operation":"metadata_contribution_replace","request":{"evidence_json":raw}}),
            json!({"operation":"metadata","mutation":{"change":{"action":"patch","evidence_json":raw}}}),
            json!({"operation":"metadata_initialize_and_patch","evidence_json":raw}),
        ] {
            let first = envelope("native-first", payload);
            let owned = materialize("catalog", "host", &[native(&first)], budget()).unwrap();
            let view = owned[0].borrowed();
            let bootstrap_entry::BaselineEvidence::NativeCreated {
                first: actual,
                audit,
            } = view.baseline
            else {
                panic!("native description")
            };
            assert!(std::ptr::eq(actual, &first));
            assert_eq!(audit.request_digest, INTENT);
            assert_ne!(audit.request_digest, first.digest().as_str());
            assert_eq!(audit.complete_absence_audit_digest, AUDIT);
        }
    }

    #[test]
    fn native_description_refuses_duplicate_missing_and_wrong_owner_evidence() {
        for raw in [
            format!(r#"{{"owner":"motion.metadata.replace","request_digest":"{INTENT}","request_digest":"{INTENT}","expected_aggregate_revision":7}}"#),
            json!({"owner":"motion.metadata.replace","expected_aggregate_revision":7}).to_string(),
            json!({"owner":"unrelated","request_digest":INTENT,"expected_aggregate_revision":7}).to_string(),
            json!({"owner":"motion.metadata.replace","request_digest":"not-a-digest","expected_aggregate_revision":7}).to_string(),
            json!({"owner":"motion.metadata.replace","request_digest":INTENT,"expected_aggregate_revision":-1}).to_string(),
        ] {
            let first = envelope("bad-evidence", json!({"operation":"metadata_contribution_replace","request":{"evidence_json":raw}}));
            assert!(materialize("catalog", "host", &[native(&first)], budget()).is_err());
        }
    }

    #[test]
    fn imported_description_derives_exact_activation_scalars_and_borrows_roots() {
        let manifest = manifest("metadata-part");
        let activation = activate(&manifest);
        let no_origin_subjects = ["work".into()].into();
        let source = HostSourceEvidence {
            baseline: HostBaselineEvidence::Imported {
                subject: "work",
                metadata: part(&manifest, &activation),
                audit: HostImportAudit {
                    root_manifest_digest: Some(INTENT),
                    origin_audit_digest: AUDIT,
                    no_origin_subjects: &no_origin_subjects,
                },
                origin: HostImportOrigin::NoOrigin,
            },
            later_commands: &[],
        };
        let owned = materialize("catalog", "host", &[source], budget()).unwrap();
        let bootstrap_entry::BaselineEvidence::Imported {
            metadata,
            audit,
            origin,
            ..
        } = owned[0].borrowed().baseline
        else {
            panic!("imported description")
        };
        assert!(std::ptr::eq(metadata.manifest, &manifest));
        assert!(std::ptr::eq(metadata.activation, &activation));
        assert_eq!(audit.import_id, manifest.import_id);
        assert_eq!(audit.source_namespace, manifest.source_namespace);
        assert_eq!(audit.input_digest, INTENT);
        assert_eq!(audit.reference_ack_id, "saved-reference-ack");
        assert_eq!(audit.mapping_digest, AUDIT);
        assert!(matches!(
            origin,
            bootstrap_entry::ImportOriginEvidence::NoOrigin
        ));
        let wrong = self::manifest("another-import");
        assert!(super::activation(part(&wrong, &activation)).is_err());
    }

    #[test]
    fn physical_description_retains_exact_mapping_and_rejects_contradictory_absence() {
        let metadata = manifest("metadata-part");
        let metadata_activation = activate(&metadata);
        let physical_part = manifest("physical-part");
        let physical_activation = activate(&physical_part);
        let no_origin_subjects = BTreeSet::new();
        let physical = PhysicalMapping {
            source_occurrence_id: "occurrence".into(),
            profile: "profile".into(),
            location: "root".into(),
            path: "film.mkv".into(),
            exact: catabolic_core::catalog_identity::ExactFileRevision {
                file_id: "native-file".into(),
                revision: "exact-retained-revision".into(),
            },
            observed_status: Some("present".into()),
        };
        let source = HostSourceEvidence {
            baseline: HostBaselineEvidence::Imported {
                subject: "work",
                metadata: part(&metadata, &metadata_activation),
                audit: HostImportAudit {
                    root_manifest_digest: Some(INTENT),
                    origin_audit_digest: AUDIT,
                    no_origin_subjects: &no_origin_subjects,
                },
                origin: HostImportOrigin::Physical {
                    part: part(&physical_part, &physical_activation),
                    motion_file_id: "motion-file",
                    physical: &physical,
                },
            },
            later_commands: &[],
        };
        let owned = materialize("catalog", "host", &[source], budget()).unwrap();
        let bootstrap_entry::BaselineEvidence::Imported {
            origin: bootstrap_entry::ImportOriginEvidence::Physical { part, audit },
            ..
        } = owned[0].borrowed().baseline
        else {
            panic!("physical description")
        };
        assert!(std::ptr::eq(part.manifest, &physical_part));
        assert_eq!(audit.physical, physical);
        assert_eq!(audit.metadata_import_id, "metadata-part");
        assert_eq!(audit.physical_import_id, "physical-part");
        assert_eq!(audit.physical_mapping_digest, AUDIT);
        let absent = ["work".into()].into();
        let mut contradiction = source;
        let HostBaselineEvidence::Imported { audit, .. } = &mut contradiction.baseline else {
            unreachable!()
        };
        audit.no_origin_subjects = &absent;
        assert!(materialize("catalog", "host", &[contradiction], budget()).is_err());
    }

    #[test]
    fn whole_list_scope_duplicate_and_shared_budget_refuse_before_decode_or_callback() {
        let first = envelope(
            "native-first",
            json!({"operation":"metadata_contribution_replace","request":{"evidence_json":evidence()}}),
        );
        assert!(materialize("different-catalog", "host", &[native(&first)], budget()).is_err());
        assert!(materialize("catalog", "different-host", &[native(&first)], budget()).is_err());
        assert!(
            materialize(
                "catalog",
                "host",
                &[native(&first), native(&first)],
                budget()
            )
            .is_err()
        );
        let invalid = envelope(
            "invalid-first",
            json!({"operation":"metadata_contribution_replace","request":{"evidence_json":"not JSON"}}),
        );
        let limited = OperationBudget::new(
            Duration::from_secs(30),
            BudgetLimits {
                items: 1,
                values: 1,
                bytes: 1,
                work: 1,
            },
        )
        .unwrap();
        let error = materialize("catalog", "host", &[native(&invalid)], limited.clone())
            .err()
            .unwrap();
        assert!(error.0.contains("complete list admission"));
        assert!(limited.failure().is_some());
    }
}
