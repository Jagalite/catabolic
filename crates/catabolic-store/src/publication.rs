//! Read-only legacy rendition admission, including descriptor-fenced evidence.
use crate::{
    Error, Result, Store,
    components::occurrence,
    layout::fact,
    migration::rows,
    query::{decode, view_sql_for},
    source,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub(crate) fn ready(
    store: &Store,
    profile: &str,
    output: &Value,
    ancestors: &[String],
) -> Result<()> {
    let id = output["id"].as_str().unwrap();
    if ancestors.iter().any(|a| a == id) || ancestors.len() >= 64 {
        return Err(Error("rendition lineage cycle or depth exceeds 64".into()));
    }
    let mut path = ancestors.to_vec();
    path.push(id.into());
    for parent in rows(
        &store.db,
        &format!(
            "SELECT * FROM ({}) WHERE profile=? AND file_id=?",
            view_sql_for(store, "catalog_renditions")
        ),
        &[
            profile.to_owned().into(),
            output["source_file_id"].as_str().unwrap().to_owned().into(),
        ],
    )? {
        ready(store, profile, &parent, &path)?;
    }
    let (snapshot, digest) = if let Some(artifact) = output["artifact_id"].as_str() {
        let mut row=rows(&store.db,"SELECT a.*,j.snapshot AS input_snapshot FROM processing_artifacts a JOIN processing_jobs j ON j.id=a.job_id WHERE a.id=? AND j.state='complete' AND a.state='ready'",&[artifact.to_owned().into()])?.into_iter().next().ok_or_else(||Error("artifact is not ready".into()))?;
        let snapshot: Value = catabolic_core::decode_json(
            row["publication_snapshot"]
                .as_str()
                .filter(|v| !v.is_empty())
                .unwrap_or("{}"),
        )?;
        decode(&mut row, &["input_snapshot"])?;
        source::validate(&row["input_snapshot"])?;
        (snapshot, row["sha256"].clone())
    } else {
        let mut row=rows(&store.db,"SELECT ro.snapshot,ro.sha256,e.payload FROM receipt_outputs ro JOIN external_receipts e ON e.id=ro.receipt_id WHERE output_id=?",&[id.to_owned().into()])?.into_iter().next().ok_or_else(||Error("external output has no validated receipt".into()))?;
        decode(&mut row, &["snapshot", "payload"])?;
        let recorded = &row["payload"]["source"];
        let mut now = occurrence(&store.db, profile, recorded["file_id"].as_str().unwrap())?;
        if recorded["revision"].as_object().is_none_or(|revision| {
            revision
                .iter()
                .any(|(k, v)| k != "ctime_ns" && now[k] != *v)
        }) {
            return Err(Error("external rendition source revision is stale".into()));
        }
        now["ctime_ns"] = recorded["revision"]["ctime_ns"].clone();
        source::validate(&now)?;
        let expected = match output["purpose"].as_str() {
            Some("transcode" | "preview" | "thumbnail") => Some("video"),
            Some("audio") => Some("audio"),
            Some("subtitle") => Some("subtitle"),
            _ => None,
        };
        if let Some(expected) = expected {
            let current = fact(store, profile, output["file_id"].as_str().unwrap(), "probe")?;
            if current["current"] != true
                || !current["data"]["streams"]
                    .as_array()
                    .is_some_and(|streams| streams.iter().any(|s| s["codec_type"] == expected))
            {
                return Err(Error(
                    "external rendition requires a current probe with its declared stream kind"
                        .into(),
                ));
            }
        }
        (row["snapshot"].clone(), row["sha256"].clone())
    };
    let mut observed = occurrence(&store.db, profile, output["file_id"].as_str().unwrap())?;
    let live = source::validate(&observed)?;
    observed["ctime_ns"] = live["ctime_ns"].clone();
    let matches = snapshot.as_object().is_some_and(|s| !s.is_empty())
        && [
            "size",
            "mtime_ns",
            "ctime_ns",
            "device",
            "inode",
            "root",
            "root_device",
            "root_inode",
        ]
        .iter()
        .all(|k| observed[k] == snapshot[k]);
    if !matches {
        let verified = fact(
            store,
            profile,
            output["file_id"].as_str().unwrap(),
            "verify",
        )?;
        if verified["current"] != true
            || verified["snapshot"]["ctime_ns"] != observed["ctime_ns"]
            || verified["data"]["digest"] != digest
        {
            return Err(Error(
                "rendition evidence is stale; scan and verify it".into(),
            ));
        }
    }
    Ok(())
}
pub fn candidates(
    store: &Store,
    profile: &str,
    catalog: &str,
) -> Result<(Option<Vec<Value>>, Value)> {
    let policy = rows(
        &store.db,
        "SELECT definition FROM rendition_policies WHERE profile=? AND catalog=?",
        &[profile.to_owned().into(), catalog.to_owned().into()],
    )?;
    let Some(row) = policy.first() else {
        return Ok((None, json!({"configured":false,"admitted":0,"excluded":[]})));
    };
    let policy: Value = catabolic_core::decode_json(row["definition"].as_str().unwrap())?;
    if policy["mode"] == "preferred"
        && rows(
            &store.db,
            "SELECT 1 FROM copy_policies WHERE catalog=?",
            &[catalog.to_owned().into()],
        )?
        .is_empty()
    {
        return Err(Error(
            "preferred rendition mode requires a catalog copy policy".into(),
        ));
    }
    let mut clauses = vec!["m.profile=?".to_owned()];
    let mut values = vec![profile.to_owned().into()];
    for key in ["purpose", "definition_id"] {
        if let Some(value) = policy.get(key) {
            clauses.push(format!("m.{key}=?"));
            values.push(crate::sql::scalar(value)?);
        }
    }
    if let Some(id) = policy.get("rule_id") {
        clauses.push("EXISTS (SELECT 1 FROM rule_jobs rj JOIN processing_artifacts a ON a.job_id=rj.job_id WHERE rj.rule_id=? AND a.id=m.artifact_id)".into());
        values.push(crate::sql::scalar(id)?);
    }
    let outputs = rows(
        &store.db,
        &format!(
            "SELECT m.* FROM ({}) m WHERE {} ORDER BY m.id LIMIT 10001",
            view_sql_for(store, "catalog_renditions"),
            clauses.join(" AND ")
        ),
        &values,
    )?;
    if outputs.len() > 10000 {
        return Err(Error(
            "publication exceeds 10000 renditions; narrow the policy".into(),
        ));
    }
    let decisions = rows(
        &store.db,
        "SELECT * FROM rendition_decisions WHERE profile=? AND catalog=? AND excluded=1",
        &[profile.to_owned().into(), catalog.to_owned().into()],
    )?
    .into_iter()
    .map(|r| {
        (
            r["output_id"].as_str().unwrap().to_owned(),
            r["reason"].clone(),
        )
    })
    .collect::<BTreeMap<_, _>>();
    let mut admitted = vec![];
    let mut excluded = vec![];
    for output in outputs {
        let reason = decisions
            .get(output["id"].as_str().unwrap())
            .cloned()
            .or_else(|| {
                ready(store, profile, &output, &[])
                    .err()
                    .map(|e| json!(e.0))
            });
        if let Some(reason) = reason {
            excluded.push(json!({"output_id":output["id"],"reason":reason}));
            continue;
        }
        admitted.extend(rows(&store.db,"SELECT a.*,f.path,f.location FROM item_files a JOIN files f ON f.id=a.file_id JOIN output_definitions d ON d.id=? WHERE a.file_id=? AND a.item_id=? AND a.role=json_extract(d.definition,'$.role')",&[crate::sql::scalar(&output["definition_id"])?,crate::sql::scalar(&output["file_id"])?,crate::sql::scalar(&output["item_id"])?])?);
    }
    let count = admitted.len();
    Ok((
        Some(admitted),
        json!({"configured":true,"admitted":count,"excluded":excluded,"definition":policy}),
    ))
}

/// Bounded recorded lineage evidence for the fallback owner. This function does
/// not open files or imply an ancestor is live; callers explicitly probe checks.
pub fn fallback_evidence(
    store: &Store,
    profile: &str,
    output: &Value,
    mode: &str,
    ancestors: &[String],
) -> Result<(Vec<Value>, Value)> {
    if !["current_source_revision", "accepted_source_revision"].contains(&mode) {
        return Err(Error("invalid source lineage mode".into()));
    }
    let id = output["id"]
        .as_str()
        .ok_or_else(|| Error("invalid rendition identity".into()))?;
    if ancestors.iter().any(|a| a == id) || ancestors.len() >= 64 {
        return Err(Error("rendition lineage cycle or depth limit".into()));
    }
    let (mut accepted, digest, source) = if let Some(artifact) = output["artifact_id"].as_str() {
        let mut row=rows(&store.db,"SELECT a.publication_snapshot,a.sha256,j.snapshot FROM processing_artifacts a JOIN processing_jobs j ON j.id=a.job_id WHERE a.id=? AND a.state='ready' AND j.state='complete'",&[artifact.to_owned().into()])?.into_iter().next().ok_or_else(||Error("rendition_not_ready".into()))?;
        if row["publication_snapshot"]
            .as_str()
            .is_none_or(str::is_empty)
        {
            row["publication_snapshot"] = json!("{}");
        }
        decode(&mut row, &["publication_snapshot", "snapshot"])?;
        (
            row["publication_snapshot"].clone(),
            row["sha256"].clone(),
            row["snapshot"].clone(),
        )
    } else {
        let mut row=rows(&store.db,"SELECT r.snapshot,r.sha256,e.payload FROM receipt_outputs r JOIN external_receipts e ON e.id=r.receipt_id WHERE r.output_id=?",&[id.to_owned().into()])?.into_iter().next().ok_or_else(||Error("missing_output_evidence".into()))?;
        decode(&mut row, &["snapshot", "payload"])?;
        let receipt = &row["payload"]["source"];
        let mut source = occurrence(
            &store.db,
            profile,
            receipt["file_id"]
                .as_str()
                .ok_or_else(|| Error("invalid receipt source".into()))?,
        )?;
        for (key, value) in receipt["revision"]
            .as_object()
            .ok_or_else(|| Error("invalid receipt revision".into()))?
        {
            source[key] = value.clone();
        }
        let expected = match output["purpose"].as_str() {
            Some("transcode" | "remux" | "preview" | "thumbnail") => Some("video"),
            Some("audio") => Some("audio"),
            Some("subtitle") => Some("subtitle"),
            _ => None,
        };
        let probe = fact(store, profile, output["file_id"].as_str().unwrap(), "probe")?;
        if let Some(expected) = expected
            && (probe["current"] != true
                || !probe["data"]["streams"]
                    .as_array()
                    .is_some_and(|streams| streams.iter().any(|s| s["codec_type"] == expected)))
        {
            return Err(Error("output_probe_required".into()));
        }
        (row["snapshot"].clone(), row["sha256"].clone(), source)
    };
    let mut observed = occurrence(&store.db, profile, output["file_id"].as_str().unwrap())?;
    let keys = [
        "size",
        "mtime_ns",
        "device",
        "inode",
        "root",
        "root_device",
        "root_inode",
    ];
    if accepted.as_object().is_none_or(|o| o.is_empty())
        || keys.iter().any(|k| observed[*k] != accepted[*k])
    {
        let verified = fact(
            store,
            profile,
            output["file_id"].as_str().unwrap(),
            "verify",
        )?;
        if verified["current"] != true || verified["data"]["digest"] != digest {
            return Err(Error("output_verification_required".into()));
        }
        accepted = verified["snapshot"].clone();
    }
    if accepted.get("ctime_ns").is_none() {
        return Err(Error("output_revision_evidence_missing".into()));
    }
    observed["ctime_ns"] = accepted["ctime_ns"].clone();
    let mut checks = vec![observed];
    let mut current = occurrence(
        &store.db,
        profile,
        output["source_file_id"].as_str().unwrap(),
    )?;
    let currentness = if keys.iter().any(|k| current[*k] != source[*k]) {
        "known_changed"
    } else {
        "not_live_checked"
    };
    if mode == "current_source_revision" {
        if currentness == "known_changed" || current["status"] != "present" {
            return Err(Error("source_currentness_unknown_or_changed".into()));
        }
        if let Some(ctime) = source.get("ctime_ns") {
            current["ctime_ns"] = ctime.clone();
        }
        checks.push(current);
        let mut path = ancestors.to_vec();
        path.push(id.into());
        for parent in rows(
            &store.db,
            &format!(
                "SELECT * FROM ({}) WHERE profile=? AND file_id=?",
                view_sql_for(store, "catalog_renditions")
            ),
            &[
                profile.to_owned().into(),
                output["source_file_id"].as_str().unwrap().to_owned().into(),
            ],
        )? {
            checks.extend(fallback_evidence(store, profile, &parent, mode, &path)?.0);
        }
    }
    Ok((
        checks,
        json!({"lineage_requirement":mode,"source_currentness":currentness,"accepted_source_revision":source,"output_digest":digest}),
    ))
}
