//! Receipt-backed admission of output produced by a trusted external job owner.
//! This never executes tools, creates a native execution lifetime, releases a
//! Motion attempt, or treats external cleanup as a native cleanup witness.
use crate::{
    Error, Result, Store,
    catalog_identity_mutation::IdentityMutation,
    catalog_import_typed::{
        self, ImportedRelationship, NativeIdentityBatch, PhysicalMapping, TypedAuthority,
        TypedBatch,
    },
};
use catabolic_core::{
    catalog_command::CommandEnvelope,
    catalog_identity::{ExactFileRevision, SourceExtent, VersionState},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::File,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    sync::Arc,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedOutputRequest {
    pub profile: String,
    pub job_id: String,
    pub attempt: i64,
    pub preparation_digest: String,
    pub provenance: Value,
    pub input_version_id: String,
    pub input_version_revision: i64,
    pub input_timeline_revision: i64,
    /// Explicit narrowly scoped promotion of the exact measured original.
    pub establish_provisional: bool,
    pub input: ExactFileRevision,
    pub input_ctime_ns: i64,
    pub output: PhysicalMapping,
    pub output_ctime_ns: i64,
    pub output_size: i64,
    pub output_sha256: String,
    pub output_duration_us: i64,
    pub output_binding: Value,
    pub item: Value,
    pub native: NativeIdentityBatch,
    pub relationships: Vec<ImportedRelationship>,
}
/// Independently constructed from the retained external job preparation,
/// successful validation/disposition, native source proof and read-only output
/// FD. Never deserialize this grant or simply copy an untrusted request into it.
#[derive(Clone, Debug)]
pub struct GeneratedOutputAuthority {
    pub host_namespace: String,
    pub request: GeneratedOutputRequest,
    pub output_file: Arc<File>,
    pub typed: TypedAuthority,
}
fn fail(message: &str) -> Error {
    Error(message.into())
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn occurrence(store: &Store, profile: &str, file: &ExactFileRevision, ctime: i64) -> Result<Value> {
    let snapshot = crate::components::occurrence(&store.db, profile, &file.file_id)?;
    if snapshot["status"] != "present" || crate::components::revision(&snapshot) != file.revision {
        return Err(fail("generated admission occurrence changed"));
    }
    use rusqlite::OptionalExtension;
    let observed:Option<i64>=store.db.query_row("SELECT c.ctime_ns FROM native_source_ctimes c JOIN observations o ON o.profile=c.profile AND o.file_id=c.file_id AND o.size=c.size AND o.mtime_ns=c.mtime_ns AND o.device=c.device AND o.inode=c.inode WHERE c.profile=? AND c.file_id=? AND o.status='present'",[profile,&file.file_id],|r|r.get(0)).optional()?;
    if observed != Some(ctime)
        || crate::source::validate(&snapshot)?["ctime_ns"].as_i64() != Some(ctime)
    {
        return Err(fail("generated admission ctime proof changed"));
    }
    Ok(snapshot)
}
pub(crate) fn admit(
    store: &Store,
    command: &CommandEnvelope,
    request: &GeneratedOutputRequest,
    authority: &GeneratedOutputAuthority,
    time: i64,
) -> Result<Value> {
    if store.schema_version < 39
        || store.db.is_autocommit()
        || authority.host_namespace != command.key().host_namespace()
        || authority.request != *request
        || request.job_id.is_empty()
        || request.job_id.len() > 256
        || request.attempt < 1
        || !digest(&request.preparation_digest)
        || !digest(&request.output_sha256)
        || !request.provenance.is_object()
        || request.provenance.as_object().is_none_or(|v| v.is_empty())
        || request.output_duration_us <= 0
        || request.output_size <= 0
        || request.input_version_revision < 1
        || request.input_timeline_revision < 1
        || request.output.profile != request.profile
        || request.input.file_id == request.output.exact.file_id
    {
        return Err(fail(
            "generated output authority, preparation, or transaction mismatch",
        ));
    }
    request.input.validate().map_err(|e| fail(&e.to_string()))?;
    request
        .output
        .exact
        .validate()
        .map_err(|e| fail(&e.to_string()))?;
    if !authority
        .typed
        .physical_files
        .get(&request.profile)
        .is_some_and(|files| files.contains(&request.output.exact.file_id))
    {
        return Err(fail("generated output lacks exact physical owner scope"));
    }
    let input = occurrence(
        store,
        &request.profile,
        &request.input,
        request.input_ctime_ns,
    )?;
    let output = occurrence(
        store,
        &request.profile,
        &request.output.exact,
        request.output_ctime_ns,
    )?;
    if output["location"] != request.output.location
        || output["path"] != request.output.path
        || request.output.observed_status.as_deref() != Some("present")
        || output["size"].as_i64() != Some(request.output_size)
        || crate::observation::binding(store, &request.profile, "source", &request.output.location)?
            != request.output_binding
    {
        return Err(fail(
            "generated output path, size, or source binding changed",
        ));
    }
    let fd = authority.output_file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    let stat = authority.output_file.metadata()?;
    let mtime = i128::from(stat.mtime()) * 1_000_000_000 + i128::from(stat.mtime_nsec());
    let ctime = i128::from(stat.ctime()) * 1_000_000_000 + i128::from(stat.ctime_nsec());
    if flags < 0
        || flags & libc::O_ACCMODE != libc::O_RDONLY
        || !stat.is_file()
        || stat.nlink() != 1
        || stat.len() != request.output_size as u64
        || output["device"].as_u64() != Some(stat.dev())
        || output["inode"].as_u64() != Some(stat.ino())
        || output["mtime_ns"].as_i64().map(i128::from) != Some(mtime)
        || ctime != i128::from(request.output_ctime_ns)
        || (input["device"] == output["device"] && input["inode"] == output["inode"])
    {
        return Err(fail(
            "generated output retained read-only descriptor proof changed",
        ));
    }
    let manifest = crate::catalog_identity_mutation::load_manifest(
        store,
        &request.profile,
        std::slice::from_ref(&request.input_version_id),
    )?;
    let original = manifest
        .versions
        .iter()
        .find(|v| v.version.version_id == request.input_version_id)
        .ok_or_else(|| fail("generated input version missing"))?;
    let (head,timeline_head,context):(i64,i64,String)=store.db.query_row("SELECT v.current_revision,t.current_revision,v.segmentation_context_id FROM catalog_versions v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision JOIN catalog_timelines t ON t.id=r.timeline_id WHERE v.id=?",[&request.input_version_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    if head != request.input_version_revision
        || timeline_head != request.input_timeline_revision
        || original.version.state != VersionState::Ready
        || original.version.bindings.len() != 1
        || !original
            .version
            .bindings
            .iter()
            .any(|b| b.file == request.input)
    {
        return Err(fail(
            "generated input version, timeline, or exact file changed",
        ));
    }
    let item_id = request.item["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| fail("generated version item ID required"))?;
    if request
        .item
        .get("identities")
        .is_some_and(|v| v.as_object().is_none_or(|o| !o.is_empty()))
        || request.item["metadata"]
            .get("motion_generated_output")
            .is_some()
        || request.native.occurrences != vec![request.output.clone()]
        || request.native.associations.len() != 1
        || request.native.mutations.len() != 1
    {
        return Err(fail(
            "generated admission requires exactly one new output owner and association",
        ));
    }
    let a = &request.native.associations[0];
    let mutation: IdentityMutation = serde_json::from_value(request.native.mutations[0].clone())?;
    if a.item_id != item_id
        || a.source_occurrence_id != request.output.source_occurrence_id
        || a.part != 1
        || mutation.profile != request.profile
        || mutation.context.id != context
        || mutation.context.expected_revision.is_none()
        || mutation.versions.len() != 1
    {
        return Err(fail(
            "generated admission cannot replace input context or timelines",
        ));
    }
    let target_timeline_revision = request
        .input_timeline_revision
        .checked_add(i64::from(request.establish_provisional))
        .ok_or_else(|| fail("timeline revision exhausted"))?;
    let target_input_revision = request
        .input_version_revision
        .checked_add(i64::from(request.establish_provisional))
        .ok_or_else(|| fail("version revision exhausted"))?;
    let timeline_state:String=store.db.query_row("SELECT r.identity_state FROM catalog_timelines t JOIN catalog_timeline_revisions r ON r.timeline_id=t.id AND r.revision=t.current_revision WHERE t.id=?",[&original.version.timeline_id],|r|r.get(0))?;
    if request.establish_provisional {
        let owners:i64=store.db.query_row("SELECT count(*) FROM catalog_live_versions v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision WHERE r.timeline_id=?",[&original.version.timeline_id],|r|r.get(0))?;
        if timeline_state != "provisional" || owners != 1 || mutation.timelines.len() != 1 {
            return Err(fail(
                "provisional establishment requires the sole exact measured original",
            ));
        }
        let t = &mutation.timelines[0];
        if t.timeline_id != original.version.timeline_id
            || t.edition_item_id != original.version.edition_item_id
            || t.expected_revision != Some(request.input_timeline_revision)
            || t.identity_state != crate::catalog_identity_mutation::TimelineState::Established
            || t.coverage != original.version.declared_coverage
        {
            return Err(fail(
                "provisional establishment may not alter identity, owner, or coverage",
            ));
        }
    } else if timeline_state != "established" || !mutation.timelines.is_empty() {
        return Err(fail(
            "generated equivalence requires an established timeline or explicit exact promotion",
        ));
    }
    let context_grant = authority
        .typed
        .existing_contexts
        .get(&context)
        .ok_or_else(|| fail("missing exact generated context grant"))?;
    let timeline_grant = authority
        .typed
        .existing_timelines
        .get(&original.version.timeline_id)
        .ok_or_else(|| fail("missing exact generated timeline grant"))?;
    if context_grant.host_namespace != authority.host_namespace
        || context_grant.catalog_id != store.database_id
        || Some(&context_grant.incarnation) != store.catalog_incarnation.as_ref()
        || Some(context_grant.revision) != mutation.context.expected_revision
        || context_grant.authority_namespace != mutation.context.authority_namespace
        || context_grant.context_key != mutation.context.context_key
        || timeline_grant.host_namespace != authority.host_namespace
        || timeline_grant.catalog_id != store.database_id
        || Some(&timeline_grant.incarnation) != store.catalog_incarnation.as_ref()
        || timeline_grant.revision != request.input_timeline_revision
        || timeline_grant.edition_item_id != original.version.edition_item_id
    {
        return Err(fail("generated context/timeline grant identity mismatch"));
    }
    let change = &mutation.versions[0];
    let version = &change.version;
    if change.expected_revision.is_some()
        || change.expected_timeline_revision != target_timeline_revision
        || version.state != VersionState::Ready
        || version.version_id == request.input_version_id
        || version.association_item_id != item_id
        || version.edition_item_id != original.version.edition_item_id
        || version.timeline_id != original.version.timeline_id
        || version.bindings.len() != 1
        || change.equivalence.is_none()
    {
        return Err(fail(
            "generated output requires new ready version and explicit exact equivalence grant",
        ));
    }
    let equivalence = change.equivalence.as_ref().unwrap();
    if mutation.evidence.len() != 1
        || mutation.evidence[0].id != equivalence.evidence_id
        || mutation.evidence[0].basis != equivalence.basis
        || mutation.evidence[0].claims.iter().any(|claim| {
            claim.edition_item_id != version.edition_item_id
                || claim.timeline_id != version.timeline_id
                || claim.timeline_revision != target_timeline_revision
                || !((claim.version_id == version.version_id
                    && claim.version_revision == 1
                    && claim.exact_files == vec![request.output.exact.clone()])
                    || (claim.version_id == request.input_version_id
                        && claim.version_revision == target_input_revision
                        && claim.exact_files == vec![request.input.clone()]))
        })
    {
        return Err(fail(
            "generated equivalence evidence exceeds exact input/output preparation scope",
        ));
    }
    if request.establish_provisional
        && !mutation.evidence[0].claims.iter().any(|c| {
            c.version_id == request.input_version_id && c.version_revision == target_input_revision
        })
    {
        return Err(fail(
            "establishment requires explicit exact original repin evidence",
        ));
    }
    let binding = &version.bindings[0];
    if binding.file != request.output.exact
        || binding.association_id != a.source_association_id
        || binding.part != 1
        || binding.source
            != (SourceExtent::WholeFile {
                duration_us: Some(request.output_duration_us),
            })
    {
        return Err(fail("generated binding does not match measured output"));
    }
    if request.relationships.len() != 1
        || request.relationships[0].source_item_id != version.edition_item_id
        || request.relationships[0].target_item_id != item_id
        || request.relationships[0].kind != "custom:motion_version"
    {
        return Err(fail(
            "generated output requires exact edition to version relationship",
        ));
    }
    let exists: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM items WHERE id=?)",
        [item_id],
        |r| r.get(0),
    )?;
    if exists {
        return Err(fail("generated output item allocation already exists"));
    }
    let mut item = request.item.clone();
    let metadata = item["metadata"]
        .as_object_mut()
        .ok_or_else(|| fail("generated item metadata object required"))?;
    metadata.insert("motion_generated_output".into(),json!({"job_id":request.job_id,"attempt":request.attempt,"preparation_digest":request.preparation_digest,"provenance":request.provenance,"input_version_id":request.input_version_id,"input_version_revision":request.input_version_revision,"input_timeline_revision":request.input_timeline_revision,"input":request.input,"input_ctime_ns":request.input_ctime_ns,"output":request.output.exact,"output_ctime_ns":request.output_ctime_ns,"output_size":request.output_size,"output_sha256":request.output_sha256,"output_duration_us":request.output_duration_us}));
    let created = crate::curation::item_put_typed(store, &item).map_err(|e| e.error)?;
    // Associations and editorial identity retain their existing native owners;
    // unlike import, this operation may repin the one exact existing original.
    let association = crate::curation::associate_in_transaction(
        store,
        &request.profile,
        &store.db,
        &json!({"file_id":request.output.exact.file_id,"item_id":item_id,"role":"primary","part":1,"metadata":{}}),
        "explicit",
    )?;
    let mut applied = mutation.clone();
    applied.versions[0].version.bindings[0].association_id = association.clone();
    if request.establish_provisional {
        let expected=store.db.prepare("SELECT binding_id,binding_revision FROM catalog_version_bindings WHERE version_id=? AND version_revision=?")?.query_map(rusqlite::params![request.input_version_id,request.input_version_revision],|r|Ok((r.get::<_,String>(0)?,Some(r.get::<_,i64>(1)?))))?.collect::<std::result::Result<std::collections::BTreeMap<_,_>,_>>()?;
        applied
            .versions
            .push(crate::catalog_identity_mutation::VersionChange {
                expected_revision: Some(request.input_version_revision),
                expected_timeline_revision: target_timeline_revision,
                version: original.version.clone(),
                equivalence: Some(equivalence.clone()),
                expected_binding_revisions: expected,
            });
    }
    let payload = serde_json::to_value(&applied)?;
    let generation: i64 =
        store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })?;
    let identity_command = CommandEnvelope::new(
        command.contract_version(),
        command.key().clone(),
        command.admission_incarnation(),
        generation,
        &json!({"operation":"identity","mutation":payload}).to_string(),
    )
    .map_err(|e| fail(&e.to_string()))?;
    let identity = crate::catalog_identity_mutation::apply_identity(
        store,
        &payload,
        &identity_command,
        authority
            .typed
            .identity
            .as_ref()
            .ok_or_else(|| fail("missing generated identity authority"))?,
        time,
    )
    .map_err(|e| e.error)?;
    let batch = TypedBatch {
        relationships: request.relationships.clone(),
        ..Default::default()
    };
    batch.validate().map_err(|e| e.error)?;
    let mut mappings = catalog_import_typed::apply_with_origin(
        store,
        command,
        &[batch],
        &[item_id.to_owned()].into(),
        Some(&authority.typed),
        time,
        "explicit",
    )
    .map_err(|e| e.error)?;
    mappings.push(json!({"owner":"occurrence","source_occurrence_id":request.output.source_occurrence_id,"profile":request.profile,"file_id":request.output.exact.file_id,"file_revision":request.output.exact.revision,"location":request.output.location,"path":request.output.path,"observed_status":"present"}));
    mappings.push(json!({"owner":"association","source_association_id":a.source_association_id,"association_id":association,"item_id":item_id,"file_id":request.output.exact.file_id,"file_revision":request.output.exact.revision,"part":1}));
    mappings.push(json!({"owner":"identity","result":identity}));
    // Recheck physical custody after every typed mutation, before outer receipt
    // commit. Publication journal acknowledgment remains Motion's responsibility.
    occurrence(
        store,
        &request.profile,
        &request.input,
        request.input_ctime_ns,
    )?;
    occurrence(
        store,
        &request.profile,
        &request.output.exact,
        request.output_ctime_ns,
    )?;
    let revision:i64=store.db.query_row("SELECT revision FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key=json_array(?) AND deleted=0",[item_id],|r|r.get(0))?;
    let (version_revision,timeline_revision):(i64,i64)=store.db.query_row("SELECT v.current_revision,t.current_revision FROM catalog_versions v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision JOIN catalog_timelines t ON t.id=r.timeline_id WHERE v.id=?",[&version.version_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    Ok(
        json!({"operation":"generated_output_admit","job_id":request.job_id,"attempt":request.attempt,"preparation_digest":request.preparation_digest,"item":created,"typed_results":mappings,"item_revision_baselines":{(item_id):revision},"established_provisional":request.establish_provisional,"input_version_id":request.input_version_id,"input_version_revision":target_input_revision,"version_id":version.version_id,"version_revision":version_revision,"timeline_id":version.timeline_id,"timeline_revision":timeline_revision,"edition_item_id":version.edition_item_id,"provenance":request.provenance,"output":request.output,"output_ctime_ns":request.output_ctime_ns,"output_size":request.output_size,"output_sha256":request.output_sha256,"output_duration_us":request.output_duration_us}),
    )
}

/// Current authoritative preparation facts. These facts reserve nothing: the
/// eventual immutable command must retain this generation and exact heads.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedInputFacts {
    pub catalog_id: String,
    pub incarnation: String,
    pub generation: i64,
    pub profile: String,
    pub context: crate::catalog_identity_mutation::ContextChange,
    pub original: crate::catalog_identity::ManifestVersion,
    pub version_revision: i64,
    pub timeline: crate::catalog_identity_mutation::TimelineChange,
    pub binding_revisions: std::collections::BTreeMap<String, i64>,
}

/// Privileged host preparation read, not application-user authorization. The
/// caller independently scopes the selected version to its job preparation.
/// Only existing endpoint grants are returned; no new effect is authorized.
pub fn capture_input(
    store: &Store,
    authority: &crate::catalog_projection::TrustedProjectionAuthority,
    host_namespace: &str,
    profile: &str,
    version_id: &str,
) -> Result<(GeneratedInputFacts, TypedAuthority)> {
    use crate::catalog_identity_mutation::{ContextChange, TimelineChange, TimelineState};
    use crate::catalog_import_typed::{
        ExistingContextGrant, ExistingItemGrant, ExistingTimelineGrant,
    };
    if store.schema_version < 39
        || !store.db.is_autocommit()
        || [host_namespace, profile, version_id]
            .iter()
            .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
    {
        return Err(fail("invalid generated input capture scope or transaction"));
    }
    store.transaction_available()?;
    store.transaction(|db| {
        let (catalog_id,incarnation,generation):(String,String,i64)=db.query_row("SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if catalog_id!=authority.catalog_id || incarnation!=authority.incarnation || generation!=authority.expected_generation {
            return Err(fail("generated input catalog/incarnation/generation changed"));
        }
        let manifest=crate::catalog_identity_mutation::load_manifest(store,profile,&[version_id.to_owned()])?;
        let original=manifest.versions.into_iter().find(|v|v.version.version_id==version_id).ok_or_else(||fail("generated input missing"))?;
        let (version_revision,context_id,namespace,key,context_revision,timeline_revision,pinned_timeline_revision,state,start,end):(i64,String,String,String,i64,i64,i64,String,Option<i64>,Option<i64>)=db.query_row(
            "SELECT v.current_revision,c.id,c.authority_namespace,c.context_key,c.current_revision,t.current_revision,r.timeline_revision,tr.identity_state,tr.coverage_start_us,tr.coverage_end_us FROM catalog_live_versions v JOIN catalog_segmentation_contexts c ON c.id=v.segmentation_context_id JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision JOIN catalog_timelines t ON t.id=r.timeline_id JOIN catalog_timeline_revisions tr ON tr.timeline_id=t.id AND tr.revision=t.current_revision WHERE v.id=?",[version_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?)))?;
        if timeline_revision!=pinned_timeline_revision {return Err(fail("generated input references stale timeline head"));}
        let identity_state=match state.as_str(){"provisional"=>TimelineState::Provisional,"established"=>TimelineState::Established,_=>return Err(fail("unknown generated timeline state"))};
        let coverage=match (start,end){(None,None)=>None,(Some(s),Some(e))=>Some(catabolic_core::catalog_identity::IntervalUs::new(s,e).map_err(|e|fail(&e.to_string()))?),_=>return Err(fail("incomplete generated timeline coverage"))};
        let binding_revisions=db.prepare("SELECT binding_id,binding_revision FROM catalog_version_bindings WHERE version_id=? AND version_revision=?")?.query_map(rusqlite::params![version_id,version_revision],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?)))?.collect::<std::result::Result<std::collections::BTreeMap<_,_>,_>>()?;
        if binding_revisions.len()!=original.version.bindings.len() {return Err(fail("generated binding snapshot incomplete"));}
        let edition=original.version.edition_item_id.clone();
        let timeline_id=original.version.timeline_id.clone();
        let item_revision:i64=db.query_row("SELECT r.revision FROM catalog_entity_revisions r JOIN items i ON r.entity_key=json_array(i.id) WHERE r.entity_kind='items' AND i.id=? AND r.deleted=0 AND NOT EXISTS(SELECT 1 FROM catalog_logical_retirements x WHERE x.item_id=i.id)",[&edition],|r|r.get(0))?;
        let mut grants=TypedAuthority::default();
        grants.existing_items.insert(edition.clone(),ExistingItemGrant{host_namespace:host_namespace.into(),catalog_id:catalog_id.clone(),incarnation:incarnation.clone(),item_revision});
        grants.existing_contexts.insert(context_id.clone(),ExistingContextGrant{host_namespace:host_namespace.into(),catalog_id:catalog_id.clone(),incarnation:incarnation.clone(),authority_namespace:namespace.clone(),context_key:key.clone(),revision:context_revision});
        grants.existing_timelines.insert(timeline_id.clone(),ExistingTimelineGrant{host_namespace:host_namespace.into(),catalog_id:catalog_id.clone(),incarnation:incarnation.clone(),edition_item_id:edition.clone(),revision:timeline_revision});
        Ok((GeneratedInputFacts{catalog_id,incarnation,generation,profile:profile.into(),context:ContextChange{id:context_id,authority_namespace:namespace,context_key:key,expected_revision:Some(context_revision)},original,version_revision,timeline:TimelineChange{timeline_id,edition_item_id:edition,expected_revision:Some(timeline_revision),identity_state,coverage},binding_revisions},grants))
    })
}

/// Descriptive retained history only. In particular, `playable` is always false,
/// including for versions which happen to remain live. This never substitutes
/// for the active identity/occurrence owner or grants artifact access.
pub fn historical_version(
    store: &Store,
    authority: &crate::catalog_projection::TrustedProjectionAuthority,
    profile: &str,
    version_id: &str,
    association_item_id: &str,
    revision: i64,
) -> Result<Value> {
    fn rows(
        db: &rusqlite::Connection,
        sql: &str,
        args: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<Value>> {
        use rusqlite::types::ValueRef;
        let mut statement = db.prepare(sql)?;
        let names = statement
            .column_names()
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let mut cursor = statement.query(args)?;
        let mut result = Vec::new();
        let mut bytes = 0usize;
        while let Some(row) = cursor.next()? {
            if result.len() >= 64 {
                return Err(fail("historical version row budget exceeded"));
            }
            let mut object = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                let value = match row.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(v) => json!(v),
                    ValueRef::Text(v) => Value::String(
                        std::str::from_utf8(v)
                            .map_err(|_| fail("invalid retained text"))?
                            .to_owned(),
                    ),
                    _ => return Err(fail("unsupported retained history value")),
                };
                object.insert(name.clone(), value);
            }
            let value = Value::Object(object);
            bytes += serde_json::to_vec(&value)?.len();
            if bytes > 1024 * 1024 {
                return Err(fail("historical version byte budget exceeded"));
            }
            result.push(value);
        }
        Ok(result)
    }
    fn one(mut values: Vec<Value>) -> Result<Value> {
        if values.len() != 1 {
            return Err(fail("exact historical owner or revision missing"));
        }
        Ok(values.remove(0))
    }
    if store.schema_version < 39
        || !store.db.is_autocommit()
        || revision < 1
        || [profile, version_id, association_item_id]
            .iter()
            .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
    {
        return Err(fail("invalid historical version scope"));
    }
    store.transaction_available()?;
    store.transaction(|db|{
        let (catalog,incarnation,generation):(String,String,i64)=db.query_row("SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if catalog!=authority.catalog_id || incarnation!=authority.incarnation || generation!=authority.expected_generation {return Err(fail("historical catalog/incarnation/generation changed"));}
        let version=one(rows(db,"SELECT * FROM catalog_versions WHERE id=? AND association_item_id=?",&[&version_id,&association_item_id])?)?;
        let snapshot=one(rows(db,"SELECT * FROM catalog_version_revisions WHERE version_id=? AND revision=?",&[&version_id,&revision])?)?;
        let timeline_id=snapshot["timeline_id"].as_str().ok_or_else(||fail("historical timeline missing"))?;
        let timeline_revision=snapshot["timeline_revision"].as_i64().ok_or_else(||fail("historical timeline revision missing"))?;
        let timeline=one(rows(db,"SELECT * FROM catalog_timelines WHERE id=?",&[&timeline_id])?)?;
        let timeline_snapshot=one(rows(db,"SELECT * FROM catalog_timeline_revisions WHERE timeline_id=? AND revision=?",&[&timeline_id,&timeline_revision])?)?;
        let bindings=rows(db,"SELECT b.association_id,r.* FROM catalog_version_bindings m JOIN catalog_bindings b ON b.id=m.binding_id AND b.version_id=m.version_id JOIN catalog_binding_revisions r ON r.binding_id=m.binding_id AND r.revision=m.binding_revision WHERE m.version_id=? AND m.version_revision=? ORDER BY r.part,r.binding_id",&[&version_id,&revision])?;
        let count:i64=db.query_row("SELECT count(*) FROM catalog_version_bindings WHERE version_id=? AND version_revision=?",rusqlite::params![version_id,revision],|r|r.get(0))?;
        if bindings.is_empty() || count!=bindings.len() as i64 || bindings.iter().any(|b|b["profile"]!=profile) {return Err(fail("historical binding profile or retained snapshot mismatch"));}
        let retirement=rows(db,"SELECT * FROM catalog_version_retirements WHERE version_id=?",&[&version_id])?.into_iter().next();
        if retirement.as_ref().is_some_and(|r|r["version_revision"]!=version["current_revision"]) {return Err(fail("historical retirement does not freeze current head"));}
        let edition=timeline["edition_item_id"].as_str().ok_or_else(||fail("historical edition missing"))?;
        let logical_retirements=rows(db,"SELECT * FROM catalog_logical_retirements WHERE item_id IN (?,?) ORDER BY item_id",&[&association_item_id,&edition])?;
        let context_id=version["segmentation_context_id"].as_str().ok_or_else(||fail("historical context missing"))?;
        let context=one(rows(db,"SELECT * FROM catalog_segmentation_contexts WHERE id=?",&[&context_id])?)?;
        let item=one(rows(db,"SELECT i.*,r.revision AS native_item_revision FROM items i JOIN catalog_entity_revisions r ON r.entity_kind='items' AND r.entity_key=json_array(i.id) AND r.deleted=0 WHERE i.id=?",&[&association_item_id])?)?;
        let retired=retirement.is_some() || !logical_retirements.is_empty();
        Ok(json!({"contract_version":1,"descriptive_only":true,"playable":false,"retired":retired,"catalog_id":catalog,"incarnation":incarnation,"generation":generation,"profile":profile,"version":version,"version_snapshot":snapshot,"timeline":timeline,"timeline_snapshot":timeline_snapshot,"bindings":bindings,"retirement":retirement,"logical_retirements":logical_retirements,"context":context,"association_item":item}))
    })
}
