//! Relocation preparation and fixed completed-offline source composite.
//! Structural capture alone is descriptive, never physical-equivalence or
//! binding authority. Explicit completed offline custody consumes descriptor
//! proof and the sole receipt owner; ordinary41 startup/dispatch stays closed.
mod allocation;
pub(crate) mod authorizer;
pub(crate) mod current;
mod custody;
mod inventory_only;
mod pre_effect;
pub(crate) use custody::VerifiedContent;
pub(crate) use pre_effect::PreEffectSourcePass;
use pre_effect::SourcePreparationPass;
#[cfg(test)]
pub(crate) use pre_effect::{assert_fence_for_test, assert_preparation_fence_for_test};
mod export_history;
mod restored_evidence;
pub use restored_evidence::RestoredSourceEvidence;
pub(crate) use restored_evidence::{
    capture_restored_source_evidence_in, capture_restored_source_evidence_with_witness_in,
};
mod history;
pub(crate) use export_history::{
    CapturedSourceHistory, RetainedRestoredSourceHistory, capture_complete_history_in, capture_complete_history_in_with_control_workspace,
    capture_restored_source_history_in,
};
pub(crate) mod file_endpoint;
pub(crate) mod library_scope;
pub use file_endpoint::{CurrentFileEndpoint, FileEndpointRequest};
pub use library_scope::{
    LibraryScopeSnapshot, LibraryScopeSource, LibraryScopeSourceFacts, capture_host_library_scope,
};
mod withdrawal;
pub(crate) use withdrawal::{
    CapturedSourceWithdrawal, capture_completed_withdrawal, revalidate_completed_withdrawal,
};
// Private candidate owner, not registered in public command dispatch.
#[allow(dead_code)]
mod mutation;
pub(crate) use mutation::{
    SourceRenewalExecution, execute_completed, prepare_completed, receipt_completed,
};
pub use mutation::{SourceRenewalPreparation, SourceRenewalReceipt};
#[cfg(test)]
pub(crate) use mutation::{
    execute_for_test, source_budget_failure_for_test, swallowed_renewal_failure_for_test,
};
pub(crate) mod scope_history;
#[cfg(test)]
mod scope_history_tests;

use crate::{
    Error, Result, Store,
    catalog_identity::ManifestVersion,
    catalog_identity_mutation::{StructuralReadBudget, load_structural_context_in_with_control_workspace},
    operation_budget::BudgetCharge,
};
use std::collections::{BTreeMap, BTreeSet};

/// Default-deny on every ordinary Store connection. The deferred SQL barrier
/// alone does not authorize minting a permit in an arbitrary writer transaction.
/// Only the fixed completed-offline composite (or its private fixture) may
/// install an exact independently verified source capability.
pub(crate) fn install(db: &rusqlite::Connection) -> Result<()> {
    db.create_scalar_function(
        "catalog_source_succession_permit_allowed",
        5,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
        |_| Ok(0_i64),
    )?;
    db.create_scalar_function(
        "catalog_source_evidence_work",
        2,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
        |args| {
            let bytes: i64 = args.get(0)?;
            let revision: i64 = args.get(1)?;
            let fail = |message: String| {
                rusqlite::Error::UserFunctionError(Box::new(std::io::Error::other(message)))
            };
            if bytes < 0 || revision < 1 {
                return Err(fail("invalid source evidence work charge".into()));
            }
            let bytes = usize::try_from(bytes)
                .map_err(|_| fail("source evidence charge overflow".into()))?;
            let budget = crate::operation_budget::OperationBudget::current().ok_or_else(|| {
                fail("source evidence work requires admitted operation budget".into())
            })?;
            budget
                .charge_without_poll(BudgetCharge {
                    work: bytes / 64 + 1,
                    ..Default::default()
                })
                .map_err(|e| fail(format!("source evidence work budget: {e:?}")))?;
            Ok(i64::try_from(bytes).map_err(|_| fail("source evidence charge overflow".into()))?)
        },
    )?;
    db.create_scalar_function(
        "catalog_source_evidence_refused",
        0,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
        |_| {
            Err::<i64, _>(rusqlite::Error::UserFunctionError(Box::new(
                std::io::Error::other("source succession evidence exceeds retained read budget"),
            )))
        },
    )?;
    Ok(())
}

const MAX_FILES: usize = 1024;
const MAX_CONTEXTS: usize = 64;
const MAX_VERSIONS: usize = 1024;
const MAX_BYTES: usize = 8 * 1024 * 1024;

/// Fields are private and there is no deserializer. Even this capture supplies
/// no candidate bytes, root descriptor, requester or source lifecycle authority.
pub(crate) struct CapturedSuccession {
    catalog_id: String,
    incarnation: String,
    generation: i64,
    profile: String,
    source: String,
    files: BTreeMap<String, String>,
    contexts: Vec<Context>,
    scope_heads: Vec<scope_history::ScopeHead>,
}
struct Context {
    id: String,
    versions: Vec<ManifestVersion>,
    heads: Vec<(String, String, i64)>,
}
fn refuse(reason: &str) -> Error {
    Error(format!("source succession: {reason}"))
}
fn charge_rows(
    admission: &mut impl StructuralReadBudget,
    count: i64,
    bytes: i64,
    maximum: usize,
) -> Result<()> {
    let count = usize::try_from(count).map_err(|_| refuse("negative row count"))?;
    let bytes = usize::try_from(bytes).map_err(|_| refuse("negative byte count"))?;
    if count > maximum || bytes > MAX_BYTES {
        return Err(refuse("complete inventory exceeds bound"));
    }
    admission.charge(BudgetCharge {
        items: count,
        values: count
            .checked_mul(4)
            .ok_or_else(|| refuse("budget overflow"))?,
        bytes,
        work: count,
    })
}

/// Enumerate every current typed reference to every registry file for this
/// source, then retain the entire owning contexts (including unrelated members).
/// The existing structural owner validates membership, heads and segmentation;
/// it intentionally does not invent current occurrence availability.
pub(crate) fn capture_source_succession_in(
    store: &Store,
    profile: &str,
    source: &str,
    admission: &mut impl StructuralReadBudget,
) -> Result<CapturedSuccession> {
    capture_source_succession_in_with_control_workspace(store, profile, source, admission, None)
}
fn capture_source_succession_in_with_control_workspace(
    store: &Store,
    profile: &str,
    source: &str,
    admission: &mut impl StructuralReadBudget,
    control: Option<&crate::migration::BootstrapWorkspace<'_>>,
) -> Result<CapturedSuccession> {
    let _source_read = current::enter_read(store)?;
    if !matches!(store.schema_version, 40 | 41)
        || !store.writable
        || store.db.is_autocommit()
        || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
    {
        return Err(refuse("retained native40/41 descriptive writer required"));
    }
    crate::query::name(profile)?;
    crate::query::name(source)?;
    store.transaction_available_with_control_workspace(control)?;
    let budget = store.catalog_operation_budget()?;
    budget
        .check()
        .map_err(|e| refuse(&format!("operation budget: {e:?}")))?;
    let db = &store.db;
    let registered = db
        .prepare("SELECT 1 FROM bindings WHERE profile=? AND kind='source' AND owner=?")?
        .exists([profile, source])?;
    let generated = db
        .prepare("SELECT 1 FROM generated_locations WHERE location=?")?
        .exists([source])?;
    if !registered || generated {
        return Err(refuse("ordinary registered source required"));
    }
    if let Some(expected_binding) = history::current_binding_in(store, profile, source, admission)?
    {
        require_current_binding_in(store, profile, source, &expected_binding, admission)?;
    }
    capture_current_structure_in(store, profile, source, admission, control)
}

// Shared exact latest-binding comparison. Historical file/head currentness is
// deliberately not required here: legitimate later scans may supersede it.
fn require_current_binding_in(
    store: &Store,
    profile: &str,
    source: &str,
    expected_binding: &serde_json::Value,
    admission: &mut impl StructuralReadBudget,
) -> Result<()> {
    require_current_binding_from(&store.db, profile, source, expected_binding, admission)
}
fn require_current_binding_from(
    db: &rusqlite::Connection,
    profile: &str,
    source: &str,
    expected_binding: &serde_json::Value,
    admission: &mut impl StructuralReadBudget,
) -> Result<()> {
    let bytes: i64 = db.query_row(
            "SELECT 256+length(CAST(profile AS BLOB))+length(CAST(kind AS BLOB))+length(CAST(owner AS BLOB))+length(CAST(root AS BLOB)) FROM bindings WHERE profile=? AND kind='source' AND owner=?",
            [profile, source], |r| r.get(0))?;
    charge_rows(admission, 1, bytes, 1)?;
    let actual = crate::migration::rows(
        db,
        "SELECT * FROM bindings WHERE profile=? AND kind='source' AND owner=?",
        &[
            rusqlite::types::Value::Text(profile.into()),
            rusqlite::types::Value::Text(source.into()),
        ],
    )?;
    if actual.len() != 1 || actual.first() != Some(expected_binding) {
        return Err(refuse(
            "current binding differs from permanent succession head",
        ));
    }
    Ok(())
}

// Fixed composite terminal reader. History was authenticated before the first
// write; no ordinary caller can use this to bypass binding-chain admission.
fn capture_current_structure_in(
    store: &Store,
    profile: &str,
    source: &str,
    admission: &mut impl StructuralReadBudget,
    control: Option<&crate::migration::BootstrapWorkspace<'_>>,
) -> Result<CapturedSuccession> {
    if store.schema_version != 41 && store.schema_version != 40
        || store.db.is_autocommit()
        || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
    {
        return Err(refuse("retained source structural writer required"));
    }
    let db = &store.db;
    let generation: i64 =
        db.query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
            r.get(0)
        })?;
    let (count, bytes): (i64, i64) = db.query_row(
        "SELECT count(*),COALESCE(sum(128+length(CAST(id AS BLOB))+length(CAST(path AS BLOB))),0) FROM files WHERE location=?",
        [source], |r| Ok((r.get(0)?, r.get(1)?)))?;
    charge_rows(admission, count, bytes, MAX_FILES)?;
    let files: BTreeMap<String, String> = db
        .prepare("SELECT id,path FROM files WHERE location=? ORDER BY id")?
        .query_map([source], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    if files.len() != usize::try_from(count).map_err(|_| refuse("file count"))? {
        return Err(refuse("incomplete registry inventory"));
    }
    // Select by current version membership, not by the mutable head of a
    // binding belonging to a different/retired version snapshot.
    let selected = "SELECT DISTINCT v.id FROM catalog_live_versions v JOIN catalog_version_bindings m ON m.version_id=v.id AND m.version_revision=v.current_revision JOIN catalog_binding_revisions r ON r.binding_id=m.binding_id AND r.revision=m.binding_revision JOIN files f ON f.id=r.file_id WHERE r.profile=? AND f.location=?";
    let (count, bytes): (i64, i64) = db.query_row(
        &format!("SELECT count(*),COALESCE(sum(128+length(CAST(id AS BLOB))),0) FROM ({selected})"),
        [profile, source],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    charge_rows(admission, count, bytes, MAX_VERSIONS)?;
    let versions: Vec<String> = db
        .prepare(&format!("{selected} ORDER BY v.id"))?
        .query_map([profile, source], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    let mut covered = BTreeSet::new();
    let mut contexts = Vec::new();
    let mut context_ids = BTreeSet::new();
    for version in &versions {
        if covered.contains(version) {
            continue;
        }
        if contexts.len() >= MAX_CONTEXTS {
            return Err(refuse("context closure exceeds bound"));
        }
        let current = load_structural_context_in_with_control_workspace(store, profile, version, admission, control)?;
        if !context_ids.insert(current.context_id().to_owned()) {
            return Err(refuse("duplicate context closure"));
        }
        for member in current.versions() {
            if !covered.insert(member.version.version_id.clone()) {
                return Err(refuse("version appears in multiple contexts"));
            }
        }
        if covered.len() > MAX_VERSIONS {
            return Err(refuse("version closure exceeds bound"));
        }
        // The identity owner already admitted and validated these buffers.
        // Move them into the retained description: no clone allocation or
        // duplicate size-discovery serialization remains to reserve here.
        let (id, versions, heads) = current.into_source_parts();
        contexts.push(Context {
            id,
            versions,
            heads,
        });
    }
    if versions.iter().any(|id| !covered.contains(id)) {
        return Err(refuse("affected version omitted"));
    }
    let scope_heads = if store.schema_version == 41 {
        scope_history::capture(store, source, &files, &contexts, admission)?
    } else {
        Vec::new()
    };
    let after: i64 = db.query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
        r.get(0)
    })?;
    if after != generation {
        return Err(refuse("capture generation changed"));
    }
    store.transaction_available_with_control_workspace(control)?;
    Ok(CapturedSuccession {
        catalog_id: store.database_id.clone(),
        incarnation: store
            .catalog_incarnation
            .clone()
            .ok_or_else(|| refuse("catalog incarnation missing"))?,
        generation,
        profile: profile.into(),
        source: source.into(),
        files,
        contexts,
        scope_heads,
    })
}

// Stream into a counting/charging sink rather than allocate a JSON copy merely
// to discover its size. Charges are cumulative with the structural owner.
fn charge_copy(
    admission: &mut impl StructuralReadBudget,
    value: &impl serde::Serialize,
) -> Result<()> {
    struct Sink<'a, B> {
        admission: &'a mut B,
        bytes: usize,
    }
    impl<B: StructuralReadBudget> std::io::Write for Sink<'_, B> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes = self
                .bytes
                .checked_add(bytes.len())
                .filter(|n| *n <= MAX_BYTES)
                .ok_or_else(|| std::io::Error::other("source succession copy bound"))?;
            self.admission
                .charge(BudgetCharge {
                    bytes: bytes.len(),
                    work: 1,
                    ..Default::default()
                })
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(
        Sink {
            admission,
            bytes: 0,
        },
        value,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Budget {
        bytes: usize,
        items: usize,
    }
    impl StructuralReadBudget for Budget {
        fn charge(&mut self, c: BudgetCharge) -> Result<()> {
            self.bytes = self
                .bytes
                .checked_add(c.bytes)
                .ok_or_else(|| refuse("overflow"))?;
            self.items += c.items;
            if self.bytes > MAX_BYTES {
                return Err(refuse("aggregate budget"));
            }
            Ok(())
        }
        fn raw_json(&mut self, raw: &str) -> Result<()> {
            self.charge(BudgetCharge {
                bytes: raw.len(),
                ..Default::default()
            })
        }
    }
    #[test]
    fn ordinary_store_never_mints_a_source_succession_permit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("native.sqlite3");
        crate::migration::initialize_native_at(&path, 40).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let allowed: i64 = store
            .db
            .query_row(
                "SELECT catalog_source_succession_permit_allowed('default','source',1,?,1)",
                [store.catalog_incarnation.as_deref().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(allowed, 0);
    }

    #[test]
    fn candidate_generic_writer_cannot_self_issue_a_source_permit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("native.sqlite3");
        crate::migration::initialize_native_at(&path, 40).unwrap();
        crate::candidate41_test_support::install_candidate41_for_test(&path).unwrap();
        let store =
            crate::candidate41_test_support::open_candidate41_for_test(&path, true).unwrap();
        let error = store.transaction(|db| {
            db.execute(
                "INSERT INTO catalog_source_succession_permits(profile,source_id,succession_revision,incarnation) VALUES ('default','source',1,?)",
                [store.catalog_incarnation.as_deref().unwrap()],
            )?;
            Ok(())
        }).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("source succession permit requires exact retained owner"),
            "{error}"
        );
        let count: i64 = store
            .db
            .query_row(
                "SELECT count(*) FROM catalog_source_succession_permits",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        store.validate_candidate41_for_test().unwrap();
    }

    #[test]
    fn complete_inventory_refuses_overflow_instead_of_truncating() {
        for (count, bytes) in [
            (-1, 0),
            (0, -1),
            (MAX_FILES as i64 + 1, 0),
            (1, MAX_BYTES as i64 + 1),
        ] {
            assert!(charge_rows(&mut Budget::default(), count, bytes, MAX_FILES).is_err());
        }
        let mut budget = Budget::default();
        charge_rows(&mut budget, MAX_FILES as i64, 12, MAX_FILES).unwrap();
        assert_eq!(budget.items, MAX_FILES);
        assert_eq!(budget.bytes, 12);
    }
    #[test]
    fn retained_copy_charges_cumulatively_without_json_buffer() {
        let mut budget = Budget::default();
        charge_copy(&mut budget, &vec!["a", "b"]).unwrap();
        assert_eq!(
            budget.bytes,
            serde_json::to_vec(&vec!["a", "b"]).unwrap().len()
        );
        budget.bytes = MAX_BYTES;
        assert!(charge_copy(&mut budget, &"x").is_err());
    }
}

impl CapturedSuccession {
    /// First-cohort structural decision only. This does not prove that any
    /// required occurrence is present or that candidate bytes match it.
    /// Unsupported siblings remain captured and are refused, never dropped.
    // Effect cohort decision remains unregistered until the succession owner.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn required_current_files(&self) -> Result<BTreeMap<String, String>> {
        use catabolic_core::catalog_identity::SourceExtent;
        let mut required = BTreeMap::new();
        for context in &self.contexts {
            for member in &context.versions {
                let affected = member
                    .version
                    .bindings
                    .iter()
                    .any(|binding| self.files.contains_key(&binding.file.file_id));
                if !affected {
                    continue;
                }
                if member.equivalence.is_some() {
                    return Err(refuse("equivalence succession is not qualified"));
                }
                for binding in &member.version.bindings {
                    if !matches!(binding.source, SourceExtent::WholeFile { .. }) {
                        return Err(refuse("slice succession is not qualified"));
                    }
                    if !self.files.contains_key(&binding.file.file_id) {
                        continue;
                    }
                    if let Some(old) =
                        required.insert(binding.file.file_id.clone(), binding.file.revision.clone())
                    {
                        if old != binding.file.revision {
                            return Err(refuse(
                                "affected versions disagree on current physical revision",
                            ));
                        }
                    }
                }
            }
        }
        if required.keys().ne(self.files.keys()) {
            return Err(refuse(
                "unreferenced registry file disposition is not qualified",
            ));
        }
        Ok(required)
    }
}

impl CapturedSuccession {
    pub(crate) fn profile(&self) -> &str {
        &self.profile
    }
    pub(crate) fn source(&self) -> &str {
        &self.source
    }
    pub(crate) fn context_ids(&self) -> impl Iterator<Item = &str> {
        self.contexts.iter().map(|c| c.id.as_str())
    }
    /// Terminal structural fence only. A future atomic effect must additionally
    /// revalidate original/candidate descriptors, binding/policy, physical facts,
    /// requester and Motion lifecycle through independently retained authority.
    pub(crate) fn revalidate_in(
        &self,
        store: &Store,
        admission: &mut impl StructuralReadBudget,
    ) -> Result<()> {
        if self.catalog_id != store.database_id
            || store.catalog_incarnation.as_deref() != Some(self.incarnation.as_str())
        {
            return Err(refuse("catalog identity changed"));
        }
        // Re-enumeration detects a newly referencing context as well as changed
        // heads within an old context; checking old contexts alone is incomplete.
        let current = capture_source_succession_in(store, &self.profile, &self.source, admission)?;
        if current.generation != self.generation
            || current.files != self.files
            || current.scope_heads != self.scope_heads
            || current.contexts.len() != self.contexts.len()
            || current
                .contexts
                .iter()
                .zip(&self.contexts)
                .any(|(a, b)| a.id != b.id || a.heads != b.heads)
        {
            return Err(refuse("complete source/context closure changed"));
        }
        Ok(())
    }
}

#[cfg(test)]
impl CapturedSuccession {
    pub(crate) fn closure_counts_for_test(&self) -> (usize, usize, usize, usize) {
        (
            self.files.len(),
            self.contexts.len(),
            self.contexts.iter().map(|c| c.versions.len()).sum(),
            self.contexts
                .iter()
                .flat_map(|c| &c.versions)
                .map(|v| v.version.bindings.len())
                .sum(),
        )
    }
}

/// Normal-host descriptive preparation. Not a content commitment, current byte
/// grant, binding replacement authority, or serializable relocation intent.
pub struct SourcePreparation(CapturedSuccession);
impl SourcePreparation {
    pub fn profile(&self) -> &str {
        &self.0.profile
    }
    pub fn source(&self) -> &str {
        &self.0.source
    }
    pub fn generation(&self) -> i64 {
        self.0.generation
    }
    /// Complete registry locators, including unsupported/unavailable members.
    /// These are selectors only; no path or native revision proves equal bytes.
    pub fn registry_files(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .files
            .iter()
            .map(|(id, path)| (id.as_str(), path.as_str()))
    }
    pub fn context_count(&self) -> usize {
        self.0.contexts.len()
    }
}
fn normal_read(store: &Store) -> Result<()> {
    if store.schema_version != 40 || !store.writable || !store.db.is_autocommit() {
        return Err(refuse(
            "normal native40 retained owner without ambient transaction required",
        ));
    }
    store.transaction_available()
}
pub fn prepare(
    store: &Store,
    expected: &crate::catalog_projection::TrustedProjectionAuthority,
    profile: &str,
    source: &str,
) -> Result<SourcePreparation> {
    normal_read(store)?;
    store.transaction(|_| {
        let mut budget =
            crate::catalog_nfo_attribution::ProofBudget::new(store.catalog_operation_budget()?);
        let captured = capture_source_succession_in(store, profile, source, &mut budget)?;
        if captured.catalog_id != expected.catalog_id
            || captured.incarnation != expected.incarnation
            || captured.generation != expected.expected_generation
        {
            return Err(refuse("source preparation identity/generation differs"));
        }
        Ok(SourcePreparation(captured))
    })
}
pub fn revalidate(store: &Store, prepared: &SourcePreparation) -> Result<()> {
    normal_read(store)?;
    store.transaction(|_| {
        let mut budget =
            crate::catalog_nfo_attribution::ProofBudget::new(store.catalog_operation_budget()?);
        prepared.0.revalidate_in(store, &mut budget)
    })
}

#[cfg(test)]
pub(crate) fn verify_candidate_content_for_test(
    store: &Store,
    profile: &str,
    source: &str,
    root: &std::path::Path,
    cancel: &std::sync::atomic::AtomicBool,
    budget: &mut impl StructuralReadBudget,
    after: impl FnOnce(),
) -> Result<String> {
    let captured = capture_source_succession_in(store, profile, source, budget)?;
    let held = custody::verify_in(store, &captured, root, cancel, budget)?;
    after();
    held.revalidate(store, cancel)?;
    captured.revalidate_in(store, budget)?;
    Ok(held.commitment().to_owned())
}

/// Candidate-only test entry for the fixed preparation mechanics. This does not
/// manufacture Completed admission or change the generic verifier above. The
/// actual completed fixture independently exercises the public opaque facade.
#[cfg(test)]
pub(crate) fn verify_candidate_preparation_for_test(
    store: &Store,
    profile: &str,
    source: &str,
    root: &std::path::Path,
    cancel: &std::sync::atomic::AtomicBool,
    budget: &mut crate::catalog_nfo_attribution::ProofBudget,
    after: impl FnOnce(),
) -> Result<String> {
    let pass = SourcePreparationPass::for_test(store, profile, source, root, cancel, budget)?;
    let held = custody::verify_preparation_in(store, &pass, profile, source, root, cancel, budget)?;
    after();
    held.revalidate(store, cancel)?;
    pass.finish(store, profile, source, root, budget)?;
    Ok(held.commitment().to_owned())
}

#[cfg(test)]
pub(crate) use withdrawal::assert_occurrence_capture_parity_for_test;
