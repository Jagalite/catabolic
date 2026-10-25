//! Fixed source-renewal identity plan. This token is not editorial authority and
//! cannot be deserialized/cloned or used by an arbitrary mutation callback.
use super::*;
use crate::catalog_source_succession::{PreEffectSourcePass, VerifiedContent};

pub(crate) struct PreparedSourceContexts {
    catalog: String,
    incarnation: String,
    envelope: CommandEnvelope,
    plans: Vec<IdentityMutation>,
}
/// The only input physical revisions come from independently retained old/new
/// descriptors. Validate complete old contexts before any source renewal write.
pub(crate) fn prepare_source_contexts(
    store: &Store,
    command: &CommandEnvelope,
    pass: &PreEffectSourcePass<'_, '_>,
    content: &VerifiedContent,
    budget: &mut crate::catalog_nfo_attribution::ProofBudget,
) -> crate::Result<PreparedSourceContexts> {
    if store.schema_version != 41
        || store.db.is_autocommit()
        || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
        || command.key().catalog_id() != store.database_id
        || store.catalog_incarnation.as_deref() != Some(command.admission_incarnation())
    {
        return Err(StoreError(
            "source context preparation owner differs".into(),
        ));
    }
    let generation: i64 =
        store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })?;
    if generation != command.expected_generation() {
        return Err(StoreError("source context generation differs".into()));
    }
    let captured = pass.check(store, command, budget)?;
    let revisions = content.revisions();
    if !captured.admit_inventory_only_in(store, &revisions, budget)? {
        let required = captured.required_current_files()?;
        if required.len() != revisions.len()
            || required.iter().any(|(id, old)| {
                revisions
                    .get(id)
                    .is_none_or(|(observed, _)| old != observed)
            })
        {
            return Err(StoreError("complete old source revisions differ".into()));
        }
    }
    let mut plans = Vec::new();
    for context in captured.context_ids() {
        let old = load_context(&store.db, captured.profile(), context)?;
        validate_context(&store.db, context)?;
        validate_active(&store.db, captured.profile(), &old)?;
        let (namespace,key,head):(String,String,i64)=store.db.query_row("SELECT authority_namespace,context_key,current_revision FROM catalog_segmentation_contexts WHERE id=?",[context],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if namespace != command.key().host_namespace() {
            return Err(StoreError("source context namespace differs".into()));
        }
        let mut changes = Vec::new();
        for entry in old {
            if entry.equivalence.is_some()
                || entry
                    .version
                    .bindings
                    .iter()
                    .any(|b| !matches!(b.source, SourceExtent::WholeFile { .. }))
            {
                return Err(StoreError(
                    "source renewal requires complete whole-file contexts without equivalence"
                        .into(),
                ));
            }
            if !entry
                .version
                .bindings
                .iter()
                .any(|b| revisions.contains_key(&b.file.file_id))
            {
                continue;
            }
            let mut version = entry.version;
            let (revision,timeline_revision):(i64,i64)=store.db.query_row("SELECT v.current_revision,r.timeline_revision FROM catalog_versions v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision WHERE v.id=?",[&version.version_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            let mut expected_binding_revisions = BTreeMap::new();
            for binding in &mut version.bindings {
                let old:i64=store.db.query_row("SELECT binding_revision FROM catalog_version_bindings WHERE version_id=? AND version_revision=? AND binding_id=?",params![version.version_id,revision,binding.binding_id],|r|r.get(0))?;
                expected_binding_revisions.insert(binding.binding_id.clone(), Some(old));
                if let Some((old, new)) = revisions.get(&binding.file.file_id) {
                    if binding.file.revision != *old {
                        return Err(StoreError("source binding old occurrence differs".into()));
                    }
                    binding.file.revision = new.clone();
                }
            }
            changes.push(VersionChange {
                expected_revision: Some(revision),
                expected_timeline_revision: timeline_revision,
                version,
                equivalence: None,
                expected_binding_revisions,
            });
        }
        if changes.is_empty() {
            return Err(StoreError("affected context has no source renewal".into()));
        }
        let plan = IdentityMutation {
            contract_version: 1,
            profile: captured.profile().into(),
            context: ContextChange {
                id: context.into(),
                authority_namespace: namespace,
                context_key: key,
                expected_revision: Some(head),
            },
            timelines: Vec::new(),
            versions: changes,
            evidence: Vec::new(),
        };
        plan.validate()?;
        plans.push(plan);
    }
    Ok(PreparedSourceContexts {
        catalog: store.database_id.clone(),
        incarnation: command.admission_incarnation().into(),
        envelope: command.clone(),
        plans,
    })
}
/// Consume only inside the fixed source composite after its exact planned
/// physical renewal. Existing context, association, occurrence, timeline and
/// segmentation validators all run; no old-context skip is introduced.
pub(crate) fn apply_source_contexts(
    store: &Store,
    command: &CommandEnvelope,
    prepared: PreparedSourceContexts,
    now: i64,
) -> crate::Result<Vec<Value>> {
    if store.db.is_autocommit()
        || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
        || store.database_id != prepared.catalog
        || store.catalog_incarnation.as_deref() != Some(prepared.incarnation.as_str())
        || command != &prepared.envelope
    {
        return Err(StoreError(
            "source context single-command custody differs".into(),
        ));
    }
    let mut results = Vec::new();
    for plan in prepared.plans {
        let authority = IdentityAuthority {
            authority_namespace: plan.context.authority_namespace.clone(),
            context_keys: [plan.context.context_key.clone()].into(),
            allow_declared_evidence: false,
            allow_verified_evidence: false,
        };
        results.push(apply(
            &store.db,
            &plan,
            command.key(),
            &authority,
            now,
            None,
        )?);
    }
    Ok(results)
}
