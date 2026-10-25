//! Fixed source composite for explicitly completed offline custody and private
//! candidate fixtures. Ordinary41 startup/dispatch and Motion admission remain
//! separate. Receipt owner applies only after its receipt-first replay decision.
use super::*;
use catabolic_core::{
    catalog_command::{CommandEnvelope, CommandResult},
    encode, sha256,
};
use rusqlite::{params, types::Value as SqlValue};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    operation: String,
    profile: String,
    source: String,
    candidate_root: String,
    content_commitment: String,
    // Exact immutable Motion intent commitment, not a native privilege grant.
    admission_digest: String,
}
struct Evidence {
    file: String,
    kind: &'static str,
    predecessor: String,
    key: Value,
    old: Value,
    new: Value,
}
fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .ok_or_else(|| refuse("fixed renewal field shape differs"))
}
fn rows_in(
    store: &Store,
    table: &'static str,
    filter: &str,
    args: &[SqlValue],
    budget: &mut impl StructuralReadBudget,
) -> Result<Vec<Value>> {
    // Table/filter are fixed source-owner SQL, never user strings. Charge all
    // selected text and scalar cells before retaining complete evidence rows.
    let columns: Vec<String> = store
        .db
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get(1))?
        .collect::<std::result::Result<_, _>>()?;
    if columns.is_empty() {
        return Err(refuse("fixed renewal table absent"));
    }
    let size = columns
        .iter()
        .map(|c| format!("coalesce(length(CAST(\"{c}\" AS BLOB)),0)"))
        .collect::<Vec<_>>()
        .join("+");
    let (count, bytes): (i64, i64) = store.db.query_row(
        &format!("SELECT count(*),coalesce(sum(256+{size}),0) FROM {table} WHERE {filter}"),
        rusqlite::params_from_iter(args),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    charge_rows(budget, count, bytes, 16384)?;
    crate::migration::rows(
        &store.db,
        &format!(
            "SELECT * FROM {table} WHERE {filter} ORDER BY {}",
            columns
                .iter()
                .map(|c| format!("\"{c}\" COLLATE BINARY"))
                .collect::<Vec<_>>()
                .join(",")
        ),
        args,
    )
}
/// Only after complete source history validation, resolve the latest live
/// same-key transfer. The permanent base row remains unchanged in the next hop.
fn prior_transfer(
    store: &Store,
    profile: &str,
    source: &str,
    file: &str,
    kind: &str,
    key: &Value,
    base: &Value,
    revision: i64,
    budget: &mut impl StructuralReadBudget,
) -> Result<(Value, String)> {
    let rows = rows_in(
        store,
        "catalog_source_live_transfers",
        "profile=? AND source_id=? AND file_id=? AND evidence_kind=? AND evidence_key=? AND succession_revision=?",
        &[
            SqlValue::Text(profile.to_owned()),
            SqlValue::Text(source.to_owned()),
            SqlValue::Text(file.to_owned()),
            SqlValue::Text(kind.to_owned()),
            encode(key).into(),
            (revision - 1).into(),
        ],
        budget,
    )?;
    if rows.len() != 1 {
        return Err(refuse("current exact predecessor transfer unavailable"));
    }
    let prior = &rows[0];
    if text(prior, "original_json")? != encode(base) {
        return Err(refuse("predecessor immutable base evidence differs"));
    }
    let renewed = catabolic_core::decode_json(text(prior, "renewed_json")?)?;
    if text(prior, "evidence_digest")? != sha256(encode(&json!([base, renewed]))) {
        return Err(refuse("predecessor evidence commitment differs"));
    }
    Ok((renewed, text(prior, "evidence_digest")?.to_owned()))
}
fn insert(store: &Store, table: &'static str, row: &Value) -> Result<()> {
    let allowed = [
        "catalog_source_succession_files",
        "catalog_source_succession_contexts",
        "catalog_source_succession_evidence",
        "catalog_source_succession_heads",
        "catalog_source_succession_context_heads",
        "catalog_source_succession_scopes",
        "catalog_source_successions",
        "component_occurrences",
    ];
    if !allowed.contains(&table) {
        return Err(refuse("fixed renewal insert table differs"));
    }
    let object = row
        .as_object()
        .ok_or_else(|| refuse("fixed renewal row shape"))?;
    let columns = object
        .keys()
        .map(|k| format!("\"{k}\""))
        .collect::<Vec<_>>()
        .join(",");
    let values = object
        .values()
        .map(|v| match v {
            Value::Null => Ok(SqlValue::Null),
            Value::String(s) => Ok(SqlValue::Text(s.clone())),
            Value::Number(n) => n
                .as_i64()
                .map(SqlValue::Integer)
                .ok_or_else(|| refuse("fixed renewal scalar differs")),
            _ => Err(refuse("fixed renewal encoded value differs")),
        })
        .collect::<Result<Vec<_>>>()?;
    store.db.execute(
        &format!(
            "INSERT INTO {table}({columns}) VALUES({})",
            vec!["?"; values.len()].join(",")
        ),
        rusqlite::params_from_iter(values),
    )?;
    Ok(())
}
fn without_ctime(v: &Value) -> Value {
    let mut v = v.clone();
    v.as_object_mut()
        .expect("owned snapshot")
        .remove("ctime_ns");
    v
}
fn retained_command_result(result: &Value) -> Result<CommandResult> {
    CommandResult::new(&encode(result)).map_err(|_| refuse("source command result is invalid"))
}
fn permit(store: &Store, profile: &str, source: &str, revision: i64) -> Result<()> {
    // Exact one-insertion grant. Revocation is atomic and survives early error
    // or unwind without executing SQL from Drop or from a SQLite callback.
    struct Revoke(Arc<AtomicBool>);
    impl Drop for Revoke {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let active = Arc::new(AtomicBool::new(true));
    let _revoke = Revoke(active.clone());
    let expected = (
        profile.to_owned(),
        source.to_owned(),
        revision,
        store
            .catalog_incarnation
            .clone()
            .ok_or_else(|| refuse("incarnation absent"))?,
    );
    store.db.create_scalar_function(
        "catalog_source_succession_permit_allowed",
        5,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
        move |c| {
            let matches = c.get::<String>(0)? == expected.0
                && c.get::<String>(1)? == expected.1
                && c.get::<i64>(2)? == expected.2
                && c.get::<String>(3)? == expected.3
                && c.get::<i64>(4)? == 1;
            Ok(i64::from(matches && active.swap(false, Ordering::AcqRel)))
        },
    )?;
    store.db.execute("INSERT INTO catalog_source_succession_permits(profile,source_id,succession_revision,incarnation) VALUES(?,?,?,?)",params![profile,source,revision,store.catalog_incarnation])?;
    Ok(())
}
fn occurrence_id(command: &CommandEnvelope, old: &Value) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        encode(&json!([
            "source_succession_occurrence",
            command.key().catalog_id(),
            command.key().host_namespace(),
            command.key().operation_id(),
            command.digest().as_str(),
            old["id"]
        ]))
        .as_bytes(),
    )
    .to_string()
}
/// The public command engine remains receipt-first. This function must never be
/// called on a saved committed/expired command merely to reconstruct its result.
fn apply_in(
    store: &Store,
    command: &CommandEnvelope,
    now: i64,
    cancel: &AtomicBool,
    budget: &mut crate::catalog_nfo_attribution::ProofBudget,
    custody_slot: &mut Option<VerifiedContent>,
    control: Option<&crate::migration::BootstrapWorkspace<'_>>,
    #[cfg(test)] fail_after_physical_renewal: bool,
) -> Result<Value> {
    #[cfg(test)]
    budget.trace_phase("input");
    let _source_read = current::enter_read(store)?;
    if store.schema_version != 41
        || store.db.is_autocommit()
        || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
    {
        return Err(refuse("fixed candidate receipt writer required"));
    }
    charge_copy(budget, command.payload())?;
    let request: Request = serde::Deserialize::deserialize(command.payload())?;
    if request.admission_digest.len() != 64
        || !request
            .admission_digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(refuse("relocation admission digest malformed"));
    }
    if request.operation != "source_succession" {
        return Err(refuse("fixed succession operation differs"));
    }
    #[cfg(test)]
    budget.trace_phase("initial_capture");
    let pass = PreEffectSourcePass::new_with_control_workspace(
        store,
        command,
        &request.profile,
        &request.source,
        cancel,
        budget,
        control,
    )?;
    let old = pass.check(store, command, budget)?;
    #[cfg(test)]
    budget.trace_phase("descriptor_verification");
    *custody_slot = Some(custody::verify_pass_in(
        store,
        command,
        &pass,
        Path::new(&request.candidate_root),
        cancel,
        budget,
    )?);
    let content = custody_slot
        .as_ref()
        .expect("fixed owner installed custody");
    if content.commitment() != request.content_commitment {
        return Err(refuse("saved content commitment differs"));
    }
    #[cfg(test)]
    budget.trace_phase("context_plan");
    let contexts = crate::catalog_identity_mutation::prepare_source_contexts(
        store, command, &pass, content, budget,
    )?;
    let args = [
        SqlValue::Text(old.profile.clone()),
        SqlValue::Text(old.source.clone()),
    ];
    #[cfg(test)]
    budget.trace_phase("evidence_collection");
    let old_binding = rows_in(
        store,
        "bindings",
        "profile=? AND kind='source' AND owner=?",
        &args,
        budget,
    )?
    .pop()
    .ok_or_else(|| refuse("source binding absent"))?;
    let new_binding = content.root_binding(&old_binding)?;
    let revision:i64=store.db.query_row("SELECT coalesce(max(revision),0)+1 FROM catalog_source_successions WHERE profile=? AND source_id=?",params![old.profile,old.source],|r|r.get(0))?;
    let predecessor:String=store.db.query_row("SELECT coalesce((SELECT record_digest FROM catalog_source_successions WHERE profile=? AND source_id=? ORDER BY revision DESC LIMIT 1),?)",params![old.profile,old.source,"0".repeat(64)],|r|r.get(0))?;
    let mut file_rows = Vec::new();
    let mut evidence = Vec::<Evidence>::new();
    let mut assertion_tokens = Vec::new();
    for (id, original, next, digest) in content.complete_files() {
        let file_args = [
            SqlValue::Text(old.profile.clone()),
            SqlValue::Text(id.into()),
        ];
        let observation = rows_in(
            store,
            "observations",
            "profile=? AND file_id=?",
            &file_args,
            budget,
        )?
        .pop()
        .ok_or_else(|| refuse("old observation absent"))?;
        let scan = if observation["observation_origin"] == "scan" {
            let scans = rows_in(
                store,
                "scans",
                "id=? AND profile=? AND location=?",
                &[
                    SqlValue::Text(text(&observation, "scan_id")?.to_owned()),
                    old.profile.clone().into(),
                    old.source.clone().into(),
                ],
                budget,
            )?;
            if scans.len() != 1 {
                return Err(refuse("old scan authority absent"));
            }
            json!({"id":observation["scan_id"],"profile":old.profile,"location":old.source})
        } else {
            Value::Null
        };
        file_rows.push(json!({"profile":old.profile,"source_id":old.source,"succession_revision":revision,"file_id":id,"relative_path":old.files[id],"disposition":"verified_whole_original","old_scan_provenance_json":encode(&scan),"old_observation_json":encode(&observation),"old_occurrence_json":encode(&without_ctime(original)),"new_occurrence_json":encode(&without_ctime(next)),"old_ctime_ns":original["ctime_ns"],"new_ctime_ns":next["ctime_ns"],"content_sha256":digest,"bytes":next["size"]}));
        for fact in rows_in(
            store,
            "file_facts",
            "profile=? AND file_id=? AND status='complete'",
            &file_args,
            budget,
        )? {
            let key = json!([fact["operation"]]);
            let (mut renewed, predecessor) = if observation["observation_origin"] == "scan"
                && crate::components::revision(&catabolic_core::decode_json(text(
                    &fact, "snapshot",
                )?)?)
                    == crate::components::revision(original)
            {
                (fact.clone(), "0".repeat(64))
            } else {
                prior_transfer(
                    store,
                    &old.profile,
                    &old.source,
                    id,
                    "processing_fact",
                    &key,
                    &fact,
                    revision,
                    budget,
                )?
            };
            if crate::components::revision(&catabolic_core::decode_json(text(
                &renewed, "snapshot",
            )?)?)
                != crate::components::revision(original)
            {
                return Err(refuse("predecessor fact tuple differs"));
            }
            renewed["snapshot"] = json!(encode(&without_ctime(next)));
            if evidence.len() >= 16384 {
                return Err(refuse("effect evidence exceeds bound"));
            }
            evidence.push(Evidence {
                file: id.into(),
                kind: "processing_fact",
                predecessor,
                key,
                old: fact,
                new: renewed,
            });
        }
        for header in rows_in(
            store,
            "media_header_checks",
            "profile=? AND file_id=?",
            &file_args,
            budget,
        )? {
            let key = json!([id]);
            let (mut renewed, predecessor) = if observation["observation_origin"] == "scan"
                && ["size", "mtime_ns", "device", "inode"]
                    .iter()
                    .all(|k| header[k] == original[k])
            {
                (header.clone(), "0".repeat(64))
            } else {
                prior_transfer(
                    store,
                    &old.profile,
                    &old.source,
                    id,
                    "media_header",
                    &key,
                    &header,
                    revision,
                    budget,
                )?
            };
            if ["size", "mtime_ns", "device", "inode"]
                .iter()
                .any(|k| renewed[k] != original[k])
            {
                return Err(refuse("predecessor header tuple differs"));
            }
            for k in ["size", "mtime_ns", "device", "inode"] {
                renewed[k] = next[k].clone();
            }
            renewed["scan_id"] = Value::Null;
            if evidence.len() >= 16384 {
                return Err(refuse("effect evidence exceeds bound"));
            }
            evidence.push(Evidence {
                file: id.into(),
                kind: "media_header",
                predecessor,
                key,
                old: header,
                new: renewed,
            });
        }
        for occurrence in rows_in(
            store,
            "component_occurrences",
            "profile=? AND file_id=?",
            &file_args,
            budget,
        )? {
            let current = crate::component_mutation::get(
                store,
                &old.profile,
                text(&occurrence, "id")?,
                false,
            )?;
            if current["current"] != true {
                continue;
            }
            let mut renewed = occurrence.clone();
            renewed["id"] = json!(occurrence_id(command, &occurrence));
            renewed["snapshot"] = json!(encode(&without_ctime(next)));
            renewed["revision"] = json!(crate::components::revision(next));
            for assertion in rows_in(
                store,
                "component_assertions",
                "occurrence_id=? AND active=1",
                &[SqlValue::Text(text(&occurrence, "id")?.to_owned())],
                budget,
            )? {
                let token = crate::component_mutation::prepare_assertion_transfer(
                    store,
                    command,
                    &old.profile,
                    text(&assertion, "id")?,
                    content,
                    budget,
                )?;
                if token.occurrence() != &renewed {
                    return Err(refuse("component owner renewal plan differs"));
                }
                if assertion_tokens.len() >= 16384 {
                    return Err(refuse("complete assertion transfer count exceeds bound"));
                }
                assertion_tokens.push((id.to_owned(), token));
            }
            if evidence.len() >= 16384 {
                return Err(refuse("effect evidence exceeds bound"));
            }
            evidence.push(Evidence {
                file: id.into(),
                kind: "component_occurrence",
                predecessor: "0".repeat(64),
                key: json!([occurrence["id"]]),
                old: occurrence,
                new: renewed,
            });
        }
    }
    #[cfg(test)]
    budget.trace_phase("complete_prewrite_pass_fence");
    content.revalidate(store, cancel)?;
    let old = pass.finish(store, command, budget)?;
    // Every effect below is fenced by the deferred absent-parent commit barrier.
    // On ANY later error it remains, so even a swallowed error cannot COMMIT.
    #[cfg(test)]
    budget.trace_phase("physical_effects");
    permit(store, &old.profile, &old.source, revision)?;
    for row in &file_rows {
        insert(store, "catalog_source_succession_files", row)?;
    }
    store.db.execute(
        "UPDATE bindings SET root=?,device=?,inode=? WHERE profile=? AND kind='source' AND owner=?",
        params![
            new_binding["root"].as_str(),
            new_binding["device"].as_i64(),
            new_binding["inode"].as_i64(),
            old.profile,
            old.source
        ],
    )?;
    for row in &file_rows {
        let next = catabolic_core::decode_json(text(row, "new_occurrence_json")?)?;
        store.db.execute("UPDATE observations SET size=?,mtime_ns=?,device=?,inode=?,status='present',scan_id=NULL,observation_origin='succession',succession_source_id=?,succession_revision=? WHERE profile=? AND file_id=?",params![next["size"].as_i64(),next["mtime_ns"].as_i64(),next["device"].as_i64(),next["inode"].as_i64(),old.source,revision,old.profile,row["file_id"].as_str()])?;
        store.db.execute("INSERT INTO native_source_ctimes(profile,file_id,size,mtime_ns,device,inode,ctime_ns) VALUES(?,?,?,?,?,?,?) ON CONFLICT(profile,file_id) DO UPDATE SET size=excluded.size,mtime_ns=excluded.mtime_ns,device=excluded.device,inode=excluded.inode,ctime_ns=excluded.ctime_ns",params![old.profile,row["file_id"].as_str(),next["size"].as_i64(),next["mtime_ns"].as_i64(),next["device"].as_i64(),next["inode"].as_i64(),row["new_ctime_ns"].as_i64()])?;
    }
    #[cfg(test)]
    if fail_after_physical_renewal {
        return Err(refuse("injected after actual physical renewal"));
    }
    #[cfg(test)]
    budget.trace_phase("context_effects");
    let identity_results =
        crate::catalog_identity_mutation::apply_source_contexts(store, command, contexts, now)?;
    for e in &evidence {
        if e.kind == "component_occurrence" {
            insert(store, "component_occurrences", &e.new)?;
        }
    }
    #[cfg(test)]
    budget.trace_phase("assertion_terminal");
    for (file, token) in assertion_tokens {
        let (old_assertion, new_assertion) = token.finish(store, command, budget)?;
        if evidence.len() >= 16384 {
            return Err(refuse("effect evidence exceeds bound"));
        }
        evidence.push(Evidence {
            file,
            kind: "component_assertion",
            predecessor: "0".repeat(64),
            key: json!([old_assertion["id"]]),
            old: old_assertion,
            new: new_assertion,
        });
    }
    #[cfg(test)]
    budget.trace_phase("complete_postcapture");
    let post = capture_current_structure_in(store, &old.profile, &old.source, budget, control)?;
    if post.files != old.files
        || post.contexts.len() != old.contexts.len()
        || post
            .contexts
            .iter()
            .zip(&old.contexts)
            .any(|(a, b)| a.id != b.id)
    {
        return Err(refuse("unexpected complete source postclosure"));
    }
    content.revalidate(store, cancel)?;
    #[cfg(test)]
    budget.trace_phase("effect_finalization");
    finish_effect(
        store,
        command,
        old,
        post,
        content,
        revision,
        predecessor,
        old_binding,
        new_binding,
        file_rows,
        evidence,
        identity_results,
        &request.admission_digest,
        budget,
    )
}
fn add_key(
    keys: &mut allocation::KeySet,
    kind: &str,
    parts: &[&str],
    budget: &mut impl StructuralReadBudget,
) -> Result<()> {
    if keys.len() >= 16384 {
        return Err(refuse("complete effect head count exceeds bound"));
    }
    allocation::key_parts(keys, kind, parts, budget)
}
fn file_keys(
    store: &Store,
    profile: &str,
    file: &str,
    keys: &mut allocation::KeySet,
    budget: &mut impl StructuralReadBudget,
) -> Result<()> {
    budget.charge(BudgetCharge {
        work: 1,
        ..Default::default()
    })?;
    let mut statement = store.db.prepare("SELECT location FROM files WHERE id=?")?;
    let mut rows = statement.query([file])?;
    let row = rows
        .next()?
        .ok_or_else(|| refuse("effect file source absent"))?;
    let source = row
        .get_ref(0)?
        .as_str()
        .map_err(|_| refuse("effect file source is not text"))?;
    add_key(keys, "files", &[file], budget)?;
    for k in ["observations", "native_source_ctimes"] {
        add_key(keys, k, &[profile, file], budget)?;
    }
    add_key(keys, "bindings", &[profile, "source", source], budget)?;
    for k in ["source_identity_policies", "source_observation_policies"] {
        add_key(keys, k, &[profile, source], budget)?;
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn finish_effect(
    store: &Store,
    command: &CommandEnvelope,
    old: CapturedSuccession,
    post: CapturedSuccession,
    content: &VerifiedContent,
    revision: i64,
    predecessor: String,
    old_binding: Value,
    new_binding: Value,
    file_rows: Vec<Value>,
    evidence: Vec<Evidence>,
    identity_results: Vec<Value>,
    admission_digest: &str,
    budget: &mut impl StructuralReadBudget,
) -> Result<Value> {
    #[cfg(test)]
    budget.trace_phase("effect_head_collection");
    let mut keys = allocation::KeySet::new();
    add_key(
        &mut keys,
        "bindings",
        &[&old.profile, "source", &old.source],
        budget,
    )?;
    for kind in ["source_identity_policies", "source_observation_policies"] {
        add_key(&mut keys, kind, &[&old.profile, &old.source], budget)?;
    }
    let mut context_heads = Vec::new();
    let mut contexts = Vec::new();
    for (index, context) in post.contexts.iter().enumerate() {
        for (kind, key, _) in &context.heads {
            if keys.len() >= 16384 || context_heads.len() >= 16384 {
                return Err(refuse("complete context effect heads exceed bound"));
            }
            allocation::copy_key(&mut keys, kind, key, budget)?;
            context_heads.push(json!({"profile":old.profile,"source_id":old.source,"succession_revision":revision,"context_id":context.id,"entity_kind":kind,"entity_key":key}));
        }
        contexts.push(json!({"profile":old.profile,"source_id":old.source,"succession_revision":revision,"context_id":context.id,"head_count":context.heads.len(),"old_heads_json":encode(&json!(old.contexts[index].heads)),"renewal_json":encode(&json!({"versions":context.versions})),"result_json":encode(&identity_results[index])}));
    }
    for id in old.files.keys() {
        file_keys(store, &old.profile, id, &mut keys, budget)?;
    }
    let mut evidence_rows = Vec::new();
    for e in evidence {
        match e.kind {
            "processing_fact" => add_key(
                &mut keys,
                "file_facts",
                &[&old.profile, &e.file, text(&e.old, "operation")?],
                budget,
            )?,
            "media_header" => add_key(
                &mut keys,
                "media_header_checks",
                &[&old.profile, &e.file],
                budget,
            )?,
            "component_occurrence" => {
                for row in [&e.old, &e.new] {
                    for (kind, key) in [
                        ("component_occurrences", "id"),
                        ("media_components", "component_id"),
                        ("item_files", "association_id"),
                        ("items", "item_id"),
                    ] {
                        add_key(&mut keys, kind, &[text(row, key)?], budget)?;
                    }
                }
            }
            "component_assertion" => {
                add_key(
                    &mut keys,
                    "component_assertions",
                    &[text(&e.old, "id")?],
                    budget,
                )?;
                add_key(&mut keys, "proposals", &[text(&e.old, "id")?], budget)?;
                let payload = catabolic_core::decode_json(text(&e.new, "payload")?)?;
                for d in payload["dependencies"]
                    .as_array()
                    .ok_or_else(|| refuse("assertion dependency shape"))?
                {
                    file_keys(store, &old.profile, text(d, "file_id")?, &mut keys, budget)?;
                }
                if let Some(file) = payload["compatibility"]["video_file_id"].as_str() {
                    file_keys(store, &old.profile, file, &mut keys, budget)?;
                }
            }
            _ => return Err(refuse("unreviewed evidence kind")),
        }
        evidence_rows.push(json!({"profile":old.profile,"source_id":old.source,"succession_revision":revision,"file_id":e.file,"evidence_kind":e.kind,"evidence_key":encode(&e.key),"predecessor_digest":e.predecessor,"evidence_digest":sha256(encode(&json!([e.old,e.new]))),"original_json":encode(&e.old),"renewed_json":encode(&e.new)}));
    }
    if keys.len() > 16384 {
        return Err(refuse("complete effect heads exceed bound"));
    }
    #[cfg(test)]
    budget.trace_phase("effect_head_admission");
    // Key strings/set storage were admitted before their actual allocations.
    // Move those strings into fixed output rows, without serializing a set only
    // to estimate a clone that no longer occurs.
    let mut heads = allocation::effect_rows(keys.len(), budget)?;
    let mut subjects = allocation::KeySet::new();
    allocation::key_parts(&mut subjects, "source_files", &[&old.source], budget)?;
    for (kind, key) in keys {
        budget.raw_json(&key)?;
        let value: Value = catabolic_core::decode_json(&key)?;
        budget.charge(BudgetCharge {
            work: 1,
            ..Default::default()
        })?;
        let (head,deleted):(i64,i64)=store.db.query_row("SELECT coalesce((SELECT revision FROM catalog_entity_revisions WHERE entity_kind=?1 AND entity_key=?2),0),coalesce((SELECT deleted FROM catalog_entity_revisions WHERE entity_kind=?1 AND entity_key=?2),0)",params![kind,key],|r|Ok((r.get(0)?,r.get(1)?)))?;
        for scope in scope_history::collections(&kind) {
            let subject = value[0]
                .as_str()
                .ok_or_else(|| refuse("effect collection subject is not text"))?;
            allocation::key_parts(&mut subjects, scope, &[subject], budget)?;
        }
        if kind == "items" {
            let subject = value[0]
                .as_str()
                .ok_or_else(|| refuse("effect parent subject is not text"))?;
            for edge in ["custom:motion_edition", "custom:motion_version"] {
                allocation::key_parts(
                    &mut subjects,
                    "motion_parent_target",
                    &[edge, subject],
                    budget,
                )?;
            }
        }
        heads.push(allocation::effect_row(
            &old.profile,
            &old.source,
            revision,
            kind,
            key,
            head,
            Some(deleted),
            budget,
        )?);
    }
    if subjects.len() > 16384 {
        return Err(refuse("complete effect scopes exceed bound"));
    }
    #[cfg(test)]
    budget.trace_phase("effect_scope_admission");
    let mut scopes = allocation::effect_rows(subjects.len(), budget)?;
    for (kind, key) in subjects {
        budget.charge(BudgetCharge {
            work: 1,
            ..Default::default()
        })?;
        let head:i64=store.db.query_row("SELECT coalesce((SELECT revision FROM catalog_source_scope_heads WHERE scope_kind=? AND scope_key=?),0)",params![kind,key],|r|r.get(0))?;
        scopes.push(allocation::effect_row(
            &old.profile,
            &old.source,
            revision,
            kind,
            key,
            head,
            None,
            budget,
        )?);
    }
    contexts.sort_by(|a, b| a["context_id"].as_str().cmp(&b["context_id"].as_str()));
    context_heads.sort_by(|a, b| {
        ["context_id", "entity_kind", "entity_key"]
            .map(|k| a[k].as_str())
            .cmp(&["context_id", "entity_kind", "entity_key"].map(|k| b[k].as_str()))
    });
    evidence_rows.sort_by(|a, b| {
        ["file_id", "evidence_kind", "evidence_key"]
            .map(|k| a[k].as_str())
            .cmp(&["file_id", "evidence_kind", "evidence_key"].map(|k| b[k].as_str()))
    });
    let generation: i64 =
        store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })?;
    let result = json!({"source":old.source,"profile":old.profile,"succession_revision":revision,"binding":new_binding,"files":file_rows.len(),"contexts":contexts.len(),"content_commitment":content.commitment(),"admission_digest":admission_digest});
    // The real receipt owner stores CommandResult's compact canonical bytes.
    // Retain those exact bytes; SQL/history must keep their strict equality.
    let command_result = retained_command_result(&result)?;
    let mapping =
        |binding: &Value| json!({"profile":old.profile,"source_id":old.source,"binding":binding});
    let mut header = json!({"profile":old.profile,"source_id":old.source,"revision":revision,"predecessor_digest":predecessor,"record_digest":"0".repeat(64),"catalog_id":old.catalog_id,"incarnation":old.incarnation,"expected_generation":command.expected_generation(),"committed_generation":generation,"command_host_namespace":command.key().host_namespace(),"command_operation_id":command.key().operation_id(),"payload_digest":command.digest().as_str(),"admission_digest":admission_digest,"old_binding_json":encode(&old_binding),"new_binding_json":encode(&new_binding),"policy_json":encode(&crate::observation::trust_policy(store,&old.profile,&old.source)?),"old_mapping_json":encode(&mapping(&old_binding)),"new_mapping_json":encode(&mapping(&new_binding)),"closure_digest":sha256(encode(&json!([heads,context_heads,scopes]))),"file_count":file_rows.len(),"context_count":contexts.len(),"head_count":heads.len(),"scope_count":scopes.len(),"evidence_count":evidence_rows.len(),"evidence_digest":sha256(encode(&json!(evidence_rows))),"result_digest":sha256(encode(&result)),"result_json":command_result.canonical_json()});
    #[cfg(test)]
    budget.trace_phase("effect_digest");
    let digest = history::admitted_digest(
        &header,
        &file_rows,
        &contexts,
        &evidence_rows,
        &heads,
        &context_heads,
        &scopes,
        budget,
    )?;
    // Test-only independent old encoder oracle includes every actual cohort.
    #[cfg(test)]
    assert_eq!(
        digest,
        history::digest(
            &header,
            &file_rows,
            &contexts,
            &evidence_rows,
            &heads,
            &context_heads,
            &scopes,
        )?
    );
    header["record_digest"] = json!(digest);
    insert(store, "catalog_source_successions", &header)?;
    for (table, rows) in [
        ("catalog_source_succession_contexts", &contexts),
        ("catalog_source_succession_evidence", &evidence_rows),
        ("catalog_source_succession_heads", &heads),
        ("catalog_source_succession_context_heads", &context_heads),
        ("catalog_source_succession_scopes", &scopes),
    ] {
        for row in rows {
            insert(store, table, row)?;
        }
    }
    // Pending owner check does not expose ordinary receipt-gated evidence.
    let complete:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_source_current_head_effects WHERE profile=? AND source_id=? AND revision=?)",params![old.profile,old.source,revision],|r|r.get(0))?;
    if !complete {
        return Err(refuse("exact pending effect closure differs"));
    }
    // The real command owner appends its sole receipt after this function returns.
    // Its deferred receipt FK still denies a swallowed success without that row.
    store.db.execute("DELETE FROM catalog_source_succession_permits WHERE profile=? AND source_id=? AND succession_revision=?",params![old.profile,old.source,revision])?;
    Ok(result)
}

/// Descriptive preparation output, not source/actor authority. The executing
/// owner must independently rebuild descriptor custody and match this digest.
#[derive(serde::Serialize)]
pub struct SourceRenewalPreparation {
    pub catalog_id: String,
    pub incarnation: String,
    pub expected_generation: i64,
    pub profile: String,
    pub source: String,
    pub candidate_root: String,
    pub content_commitment: String,
    pub old_binding: Value,
    pub new_binding: Value,
}
pub(crate) fn prepare_completed(
    store: &Store,
    admission: &crate::migration::CompletedSourcePreparationAdmission<'_, '_>,
    profile: &str,
    source: &str,
    candidate: &Path,
    cancel: &AtomicBool,
) -> Result<SourceRenewalPreparation> {
    admission.check(store, profile, source, candidate)?;
    if !store.db.is_autocommit() {
        return Err(refuse(
            "completed source preparation requires top-level custody",
        ));
    }
    let mut custody = None;
    let prepared = store.transaction_detailed_with_control_workspace(|_| {
        admission.check(store, profile, source, candidate)?;
        let inventory = crate::native_worker_inventory::inspect_completed_source_preparation(
            store, admission, profile, source, candidate,
        )?;
        if !inventory.native || !inventory.admits_new_work() {
            return Err(refuse(
                "source preparation has unresolved native worker custody",
            ));
        }
        let mut budget = crate::catalog_nfo_attribution::ProofBudget::source_succession(
            store.catalog_operation_budget()?,
        );
        let pass = SourcePreparationPass::new(
            store,
            admission,
            profile,
            source,
            candidate,
            cancel,
            &mut budget,
        )?;
        custody = Some(custody::verify_preparation_in(
            store,
            &pass,
            profile,
            source,
            candidate,
            cancel,
            &mut budget,
        )?);
        let held = custody.as_ref().expect("verified preparation custody");
        let captured = pass.check(store, profile, source, candidate, &mut budget)?;
        let old_binding = rows_in(
            store,
            "bindings",
            "profile=? AND kind='source' AND owner=?",
            &[
                SqlValue::Text(profile.to_owned()),
                SqlValue::Text(source.to_owned()),
            ],
            &mut budget,
        )?
        .pop()
        .ok_or_else(|| refuse("preparation source binding absent"))?;
        let new_binding = held.root_binding(&old_binding)?;
        let output = SourceRenewalPreparation {
            catalog_id: captured.catalog_id.clone(),
            incarnation: captured.incarnation.clone(),
            expected_generation: captured.generation,
            profile: profile.to_owned(),
            source: source.to_owned(),
            candidate_root: text(&new_binding, "root")?.to_owned(),
            content_commitment: held.commitment().to_owned(),
            old_binding,
            new_binding,
        };
        pass.check(store, profile, source, candidate, &mut budget)?;
        admission.check(store, profile, source, candidate)?;
        held.revalidate(store, cancel)?;
        // Last physical/admission checks remain inside the boundary guard. No
        // callback or write separates consuming the fence from returning.
        pass.finish(store, profile, source, candidate, &mut budget)?;
        Ok(output)
    }, admission.control_workspace());
    drop(custody);
    prepared
}

/// Receipt-only authenticated permanent effect. No Deserialize/Clone or
/// public constructor: copying its descriptions does not recreate this proof.
pub struct SourceRenewalReceipt {
    command: CommandEnvelope,
    effect: Value,
}
impl SourceRenewalReceipt {
    pub fn command(&self) -> &CommandEnvelope {
        &self.command
    }
    pub fn effect_description(&self) -> &Value {
        &self.effect
    }
}
pub(crate) fn receipt_completed(
    store: &Store,
    admission: &crate::migration::CompletedSourceAdmission<'_, '_>,
    command: &CommandEnvelope,
    authority: crate::catalog_commands::TrustedCommandAuthority<'_>,
) -> Result<Option<SourceRenewalReceipt>> {
    admission.check(store, command)?;
    if !store.db.is_autocommit()
        || authority.host_namespace != command.key().host_namespace()
        || authority.receipt_visibility
            != catabolic_core::catalog_command::ReceiptVisibility::FullResult
    {
        return Err(refuse("source receipt scope or visibility denied"));
    }
    store.transaction_detailed_with_control_workspace(|_| {
        admission.check(store, command)?;
        let mut budget = crate::catalog_nfo_attribution::ProofBudget::source_succession(
            store.catalog_operation_budget()?,
        );
        charge_copy(&mut budget, command.payload())?;
        let request: Request = serde::Deserialize::deserialize(command.payload())?;
        if request.operation != "source_succession" {
            return Err(refuse("source receipt operation differs"));
        }
        let args = [
            SqlValue::Text(command.key().catalog_id().to_owned()),
            SqlValue::Text(command.key().host_namespace().to_owned()),
            SqlValue::Text(command.key().operation_id().to_owned()),
        ];
        let receipts = rows_in(
            store,
            "catalog_command_receipts",
            "catalog_id=? AND host_namespace=? AND operation_id=?",
            &args,
            &mut budget,
        )?;
        let effects = rows_in(
            store,
            "catalog_source_successions",
            "catalog_id=? AND command_host_namespace=? AND command_operation_id=?",
            &args,
            &mut budget,
        )?;
        let Some(receipt) = receipts.first() else {
            if !effects.is_empty() {
                return Err(refuse("source effect has no permanent receipt"));
            }
            return Ok(None);
        };
        if receipts.len() != 1
            || effects.len() != 1
            || receipt["payload_digest"] != command.digest().as_str()
            || receipt["admission_incarnation"] != command.admission_incarnation()
            || receipt["expected_generation"] != command.expected_generation()
            || receipt["contract_version"] != command.contract_version()
            || receipt["canonicalization_version"] != command.digest().canonicalization_version()
        {
            return Err(refuse("source receipt exact command differs"));
        }
        let effect = &effects[0];
        if effect["profile"] != request.profile
            || effect["source_id"] != request.source
            || effect["admission_digest"] != request.admission_digest
            || effect["payload_digest"] != command.digest().as_str()
            || effect["incarnation"] != command.admission_incarnation()
            || effect["expected_generation"] != command.expected_generation()
        {
            return Err(refuse("source receipt effect domain differs"));
        }
        // Authenticate every permanent predecessor and full/expired receipt.
        // This never consults a root, inode, fresh hash or disposable result body.
        if history::current_binding_in(store, &request.profile, &request.source, &mut budget)?
            .is_none()
        {
            return Err(refuse("source receipt permanent history absent"));
        }
        let result = catabolic_core::decode_json(text(effect, "result_json")?)?;
        if result["content_commitment"] != request.content_commitment
            || result["admission_digest"] != request.admission_digest
        {
            return Err(refuse("source receipt saved intent differs"));
        }
        admission.check(store, command)?;
        Ok(Some(SourceRenewalReceipt {
            command: command.clone(),
            effect: effects.into_iter().next().expect("exact effect"),
        }))
    }, admission.control_workspace())
}

/// Fixed execution custody. The completed-owner token has no public
/// constructor; descriptors outlive the receipt engine's COMMIT or rollback.
pub(crate) struct SourceRenewalExecution<'a, 'workspace> {
    admission: &'a crate::migration::CompletedSourceAdmission<'a, 'workspace>,
    command: &'a CommandEnvelope,
    cancel: &'a AtomicBool,
    custody: Option<VerifiedContent>,
}
impl<'a, 'workspace> SourceRenewalExecution<'a, 'workspace> {
    pub(crate) fn control_workspace(&self) -> Option<&'a crate::migration::BootstrapWorkspace<'workspace>> {
        self.admission.control_workspace()
    }
    pub(crate) fn check(&self, store: &Store, command: &CommandEnvelope) -> Result<()> {
        if command != self.command || !store.db.is_autocommit() {
            return Err(refuse(
                "completed source exact envelope/top-level scope differs",
            ));
        }
        self.admission.check(store, command)?;
        if command.payload()["operation"] != "source_succession"
            || command
                .key()
                .operation_id()
                .starts_with(crate::catalog_import::OPERATION_PREFIX)
        {
            return Err(refuse("completed source operation differs"));
        }
        Ok(())
    }
    pub(crate) fn check_in(&self, store: &Store, command: &CommandEnvelope) -> Result<()> {
        if command != self.command
            || store.db.is_autocommit()
            || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
        {
            return Err(refuse("completed source receipt writer differs"));
        }
        self.admission.check(store, command)
    }
    pub(crate) fn apply_in(
        &mut self,
        store: &Store,
        command: &CommandEnvelope,
        now: i64,
    ) -> Result<Value> {
        if command != self.command || self.custody.is_some() {
            return Err(refuse("completed source execution cannot be reused"));
        }
        let inventory = crate::native_worker_inventory::inspect_completed_source(
            store,
            self.admission,
            command,
        )?;
        if !inventory.native || !inventory.admits_new_work() {
            return Err(refuse(
                "source succession has unresolved native worker custody",
            ));
        }
        let mut budget = crate::catalog_nfo_attribution::ProofBudget::source_succession(
            store.catalog_operation_budget()?,
        );
        apply_in(
            store,
            command,
            now,
            self.cancel,
            &mut budget,
            &mut self.custody,
            self.admission.control_workspace(),
            #[cfg(test)]
            false,
        )
    }
    pub(crate) fn terminal_in(&self, store: &Store, command: &CommandEnvelope) -> Result<()> {
        self.admission.check(store, command)?;
        self.custody
            .as_ref()
            .ok_or_else(|| refuse("source terminal custody absent"))?
            .revalidate(store, self.cancel)
    }
}
pub(crate) fn execute_completed<'a, 'workspace>(
    store: &Store,
    admission: &'a crate::migration::CompletedSourceAdmission<'a, 'workspace>,
    command: &'a CommandEnvelope,
    authority: crate::catalog_commands::TrustedCommandAuthority<'_>,
    policy: crate::catalog_commands::ReceiptPolicy,
    cancel: &'a AtomicBool,
) -> crate::catalog_commands::Result<crate::catalog_commands::CommandOutcome> {
    let mut execution = SourceRenewalExecution {
        admission,
        command,
        cancel,
        custody: None,
    };
    crate::catalog_commands::execute_completed_source(
        store,
        command,
        authority,
        policy,
        &mut execution,
    )
    // execution (including descriptors) drops only after transaction settlement.
}

/// Private fixture corridor consuming the fixed composite through the real
/// receipt engine. Replay never invokes apply_in or opens either source root.
#[cfg(test)]
pub(crate) fn execute_for_test(
    store: &Store,
    command: &CommandEnvelope,
    authority: crate::catalog_commands::TrustedCommandAuthority<'_>,
    policy: crate::catalog_commands::ReceiptPolicy,
    cancel: &AtomicBool,
    _budget: &mut crate::catalog_nfo_attribution::ProofBudget,
) -> crate::catalog_commands::Result<crate::catalog_commands::CommandOutcome> {
    execute_for_test_inner(store, command, authority, policy, cancel, false)
}

#[cfg(test)]
pub(crate) fn source_budget_failure_for_test(
    store: &Store,
    command: &CommandEnvelope,
    authority: crate::catalog_commands::TrustedCommandAuthority<'_>,
    policy: crate::catalog_commands::ReceiptPolicy,
    cancel: &AtomicBool,
) -> crate::catalog_commands::Result<crate::catalog_commands::CommandOutcome> {
    execute_for_test_inner(store, command, authority, policy, cancel, true)
}

#[cfg(test)]
fn execute_for_test_inner(
    store: &Store,
    command: &CommandEnvelope,
    authority: crate::catalog_commands::TrustedCommandAuthority<'_>,
    policy: crate::catalog_commands::ReceiptPolicy,
    cancel: &AtomicBool,
    fail_budget_after_effects: bool,
) -> crate::catalog_commands::Result<crate::catalog_commands::CommandOutcome> {
    let observed_effects = std::cell::Cell::new(false);
    let mut custody = None;
    let outcome = crate::catalog_commands::execute_candidate_for_test(
        store,
        command,
        authority,
        policy,
        |store, command, now| {
            let inventory =
                crate::native_worker_inventory::inspect_source_candidate_for_test(store)?;
            if !inventory.native || !inventory.admits_new_work() {
                return Err(refuse(
                    "source succession has unresolved native worker custody",
                ));
            }
            // Writer acquisition may replace the preceding read's operation.
            // Borrow the real receipt writer; never carry a stale fixture owner.
            let mut budget = crate::catalog_nfo_attribution::ProofBudget::source_succession(
                store.catalog_operation_budget()?,
            );
            let result = apply_in(
                store,
                command,
                now,
                cancel,
                &mut budget,
                &mut custody,
                None,
                false,
            )?;
            if fail_budget_after_effects {
                // Actual complete mutation ran and removed its deferred permit.
                // Swallow ONLY the deliberate source-local budget error, then
                // let the real receipt engine attempt normal final settlement.
                let renewed: i64 = store.db.query_row(
                    "SELECT count(*) FROM observations WHERE observation_origin='succession'",
                    [],
                    |r| r.get(0),
                )?;
                assert!(renewed > 0, "budget probe did not reach physical effects");
                let permits: i64 = store.db.query_row(
                    "SELECT count(*) FROM catalog_source_succession_permits",
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(
                    permits, 0,
                    "budget probe must not rely on deferred permit refusal"
                );
                observed_effects.set(true);
                assert!(
                    StructuralReadBudget::charge(
                        &mut budget,
                        BudgetCharge {
                            work: 64 * 1024 + 1,
                            ..Default::default()
                        }
                    )
                    .is_err()
                );
                assert_eq!(
                    store.catalog_operation_budget()?.failure(),
                    Some(crate::selection::EvaluationFailure::WorkBudget)
                );
            }
            Ok(result)
        },
    );
    // Both error rollback and success COMMIT have settled before descriptor
    // custody is released. No leak, async cancellation gap or caller callback.
    drop(custody);
    assert!(
        !fail_budget_after_effects || observed_effects.get(),
        "source budget probe failed before actual effects: {outcome:?}"
    );
    outcome
}

/// Deterministic fault after genuine renewal writes. The fixture deliberately
/// swallows that error; the real deferred barrier must still reject COMMIT.
#[cfg(test)]
pub(crate) fn swallowed_renewal_failure_for_test(
    store: &Store,
    command: &CommandEnvelope,
    cancel: &AtomicBool,
    _budget: &mut crate::catalog_nfo_attribution::ProofBudget,
) -> Result<()> {
    store.validate_candidate41_for_test()?;
    let mut custody = None;
    let outcome = store.transaction_detailed(|_| {
        let inventory = crate::native_worker_inventory::inspect_source_candidate_for_test(store)?;
        if !inventory.admits_new_work() {
            return Err(refuse("unresolved source custody"));
        }
        let mut budget = crate::catalog_nfo_attribution::ProofBudget::source_succession(
            store.catalog_operation_budget()?,
        );
        let error = apply_in(store, command, 0, cancel, &mut budget, &mut custody, None, true)
            .expect_err("fixed failure point must run after real effects");
        assert!(
            error
                .to_string()
                .contains("injected after actual physical renewal"),
            "fixture failed before actual physical renewal: {error}"
        );
        let permits: i64 = store.db.query_row(
            "SELECT count(*) FROM catalog_source_succession_permits",
            [],
            |r| r.get(0),
        )?;
        let renewed: i64 = store.db.query_row(
            "SELECT count(*) FROM observations WHERE observation_origin='succession'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(permits, 1, "fault did not retain exact permit");
        assert!(renewed > 0, "fault did not reach actual renewal");
        Ok(())
    });
    drop(custody);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_source_result_matches_real_command_result_codec() {
        let result = json!({"source":"media","files":2,"admission_digest":"admitted"});
        let retained = retained_command_result(&result).unwrap();
        let receipt =
            CommandResult::new(r#"{"source":"media","files":2,"admission_digest":"admitted"}"#)
                .unwrap();
        assert_eq!(retained.canonical_json(), receipt.canonical_json());
        assert_eq!(retained.value(), &result);
        assert_ne!(retained.canonical_json(), encode(&result));
        assert_eq!(sha256(encode(retained.value())), sha256(encode(&result)));
    }
}
