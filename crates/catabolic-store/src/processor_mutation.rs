// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Durable external jobs and fenced worker leases. Receipt ingestion alone owns
//! output registration. No network or credential lookup occurs in this module.
use crate::{Error, Result, Store, migration::rows, mutation, output_mutation, receipt_mutation};
use catabolic_core::{
    encode,
    execution::{self, Effect, Input, Lease, Phase, Proof, State},
    sha256,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::BTreeSet;
fn now(db: &Connection) -> Result<i64> {
    Ok(db.query_row("SELECT unixepoch('now')", [], |r| r.get(0))?)
}
fn seconds(value: i64) -> Result<()> {
    if !(30..=3600).contains(&value) {
        Err(Error("lease seconds must be 30..3600".into()))
    } else {
        Ok(())
    }
}
pub fn definition(value: &Value) -> Result<Value> {
    let fields = value
        .as_object()
        .filter(|o| {
            o.keys().all(|k| {
                [
                    "endpoint",
                    "credential_env",
                    "capacity",
                    "timeout",
                    "version",
                ]
                .contains(&k.as_str())
            })
        })
        .ok_or_else(|| Error("invalid processor definition".into()))?;
    let mut result = json!({"version":1,"capacity":1,"timeout":15});
    for (key, value) in fields {
        result[key] = value.clone();
    }
    let endpoint = result["endpoint"]
        .as_str()
        .filter(|s| s.chars().count() <= 4096)
        .ok_or_else(|| Error("processor requires an HTTP(S) endpoint".into()))?;
    if crate::notifications::callback_origin(endpoint).is_err()
        || endpoint
            .split_once('?')
            .is_some_and(|(_, s)| !s.split('#').next().unwrap_or("").is_empty())
        || endpoint.split_once('#').is_some_and(|(_, s)| !s.is_empty())
    {
        return Err(Error(
            "processor endpoint must be HTTP(S) without credentials, query or fragment".into(),
        ));
    }
    if result["version"].as_u64() != Some(1)
        || result["capacity"]
            .as_u64()
            .is_none_or(|n| !(1..=1000).contains(&n))
        || result["timeout"]
            .as_u64()
            .is_none_or(|n| !(1..=60).contains(&n))
    {
        return Err(Error(
            "invalid processor version, capacity (1..1000) or timeout (1..60)".into(),
        ));
    }
    if let Some(value) = result.get("credential_env") {
        crate::notifications::credential_reference(
            value
                .as_str()
                .ok_or_else(|| Error("credential_env must name an environment variable".into()))?,
        )
        .map_err(|_| Error("credential_env must name an environment variable".into()))?;
    }
    result["endpoint"] = json!(endpoint.trim_end_matches('/'));
    Ok(result)
}
pub fn get(store: &Store, profile: &str, id: &str) -> Result<Value> {
    let raw: Option<String> = store
        .db
        .query_row(
            "SELECT definition FROM processors WHERE profile=? AND id=?",
            params![profile, id],
            |r| r.get(0),
        )
        .optional()?;
    let mut value = catabolic_core::decode_json(
        &raw.ok_or_else(|| Error("unknown processor in this profile".into()))?,
    )?;
    value["id"] = json!(id);
    Ok(value)
}
pub fn put(store: &Store, profile: &str, id: &str, value: &Value) -> Result<Value> {
    mutation::require_recovered(&store.db)?;
    mutation::profile(store, profile)?;
    crate::query::name(id)?;
    let serialized = encode(&definition(value)?);
    store.transaction(|db| {
        let old: Option<String> = db
            .query_row(
                "SELECT definition FROM processors WHERE profile=? AND id=?",
                params![profile, id],
                |r| r.get(0),
            )
            .optional()?;
        if old.is_some_and(|s| s != serialized) {
            return Err(Error(
                "processor definitions are immutable; choose a new name".into(),
            ));
        }
        db.execute(
            "INSERT INTO processors VALUES (?,?,?) ON CONFLICT DO NOTHING",
            params![id, profile, serialized],
        )?;
        Ok(())
    })?;
    get(store, profile, id)
}
#[allow(clippy::too_many_arguments)]
pub fn prepare(
    store: &Store,
    profile: &str,
    processor: &str,
    file: &str,
    item: &str,
    definition_id: &str,
    configuration: &Value,
    operation: Option<&str>,
) -> Result<(Value, String)> {
    mutation::require_recovered(&store.db)?;
    get(store, profile, processor)?;
    let definition = output_mutation::get_definition(store, definition_id)?;
    if !configuration.is_object() {
        return Err(Error("processor configuration must be an object".into()));
    }
    catabolic_core::validate_finite(configuration)?;
    let serialized = encode(configuration);
    if serialized.len() > 65536 {
        return Err(Error("processor configuration exceeds 64 KiB".into()));
    }
    let captured = receipt_mutation::source_receipt(store, profile, file, item)?;
    let location: Value = rows(
        &store.db,
        "SELECT location,path FROM files WHERE id=?",
        &[file.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("unknown source file".into()))?;
    let payload = json!({"database_id":captured["database_id"],"profile":captured["profile"],"source":captured["source"],"version":1,"source_location":location,"definition_id":definition_id,"output_definition":definition["definition"],"configuration":configuration,"configuration_digest":sha256(serialized)});
    if let Some(id) = operation {
        let saved = crate::media_mutation::operation_get(store, id)?;
        if saved["operation_kind"] != "external"
            || saved["preset"] != processor
            || saved["definition"]["options"] != *configuration
            || saved["definition"]["output_definition_id"] != definition_id
        {
            return Err(Error(
                "processor request differs from its immutable operation".into(),
            ));
        }
    }
    let digest = sha256(if let Some(operation) = operation {
        encode(&json!([payload, operation]))
    } else {
        encode(&payload)
    });
    Ok((payload, digest))
}
#[allow(clippy::too_many_arguments)]
pub fn enqueue(
    store: &Store,
    profile: &str,
    processor: &str,
    file: &str,
    item: &str,
    definition: &str,
    configuration: &Value,
    operation: Option<&str>,
) -> Result<Value> {
    let (payload, digest) = prepare(
        store,
        profile,
        processor,
        file,
        item,
        definition,
        configuration,
        operation,
    )?;
    let id=store.transaction(|db|{db.execute("INSERT INTO processor_jobs(id,profile,processor,request,digest,operation_id) VALUES (?,?,?,?,?,?) ON CONFLICT(profile,processor,digest) DO NOTHING",params![uuid::Uuid::new_v4().simple().to_string(),profile,processor,encode(&payload),digest,operation])?;
        Ok(db.query_row("SELECT id FROM processor_jobs WHERE profile=? AND processor=? AND digest=?",params![profile,processor,digest],|r|r.get::<_,String>(0))?)})?;
    job(store, profile, &id)
}
fn private_job(db: &Connection, profile: &str, id: &str) -> Result<Value> {
    let mut value = rows(
        db,
        "SELECT * FROM processor_jobs WHERE profile=? AND id=?",
        &[profile.to_owned().into(), id.to_owned().into()],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error("unknown processor job in this profile".into()))?;
    value["request"] = catabolic_core::decode_json(
        value["request"]
            .as_str()
            .ok_or_else(|| Error("invalid processor request".into()))?,
    )?;
    Ok(value)
}
pub fn job(store: &Store, profile: &str, id: &str) -> Result<Value> {
    let mut row = private_job(&store.db, profile, id)?;
    row.as_object_mut().unwrap().remove("lease_token");
    Ok(row)
}
pub fn jobs(
    store: &Store,
    profile: &str,
    processor: &str,
    limit: usize,
    after: &str,
) -> Result<Value> {
    get(store, profile, processor)?;
    if !(1..=1000).contains(&limit) {
        return Err(Error("limit must be 1..1000".into()));
    }
    let ids = rows(
        &store.db,
        "SELECT id FROM processor_jobs WHERE profile=? AND processor=? AND id>? ORDER BY id LIMIT ?",
        &[
            profile.to_owned().into(),
            processor.to_owned().into(),
            after.to_owned().into(),
            ((limit + 1) as i64).into(),
        ],
    )?;
    let list = ids
        .iter()
        .take(limit)
        .map(|row| job(store, profile, row["id"].as_str().unwrap()))
        .collect::<Result<Vec<_>>>()?;
    Ok(
        json!({"jobs":list,"next_after":if ids.len()>limit {ids[limit-1]["id"].clone()}else{Value::Null}}),
    )
}
fn observed(row: &Value) -> Result<State> {
    let phase = match row["state"].as_str() {
        Some("leased" | "submitted") => Phase::Running,
        Some(value) => {
            Phase::parse(value).ok_or_else(|| Error("invalid processor state".into()))?
        }
        None => return Err(Error("invalid processor state".into())),
    };
    let generation = row["generation"]
        .as_i64()
        .ok_or_else(|| Error("invalid processor generation".into()))?;
    let snapshot_digest = sha256(encode(&row["request"]["source"]));
    let options_digest = sha256(encode(&row["request"]));
    let proof = Proof {
        generation,
        attempt: generation,
        token_digest: sha256(row["lease_token"].as_str().unwrap_or("")),
        snapshot_digest: snapshot_digest.clone(),
        options_digest: options_digest.clone(),
    };
    // External worker liveness is established solely by the remote lease expiry;
    // it has no local PID. Retain a prior generation even after explicit retry.
    let claim = (generation > 0).then(|| Lease {
        proof,
        until: row["lease_until"].as_f64().unwrap_or(0.0),
        worker_alive: matches!(phase, Phase::Running)
            && row["lease_token"].as_str().is_some_and(|s| !s.is_empty()),
    });
    Ok(State {
        phase,
        attempts: generation,
        snapshot_digest,
        options_digest,
        claim,
    })
}
fn proof(state: &State, token: &str) -> Proof {
    let mut proof = state
        .claim
        .as_ref()
        .map(|c| c.proof.clone())
        .unwrap_or(Proof {
            generation: 0,
            attempt: 0,
            token_digest: String::new(),
            snapshot_digest: state.snapshot_digest.clone(),
            options_digest: state.options_digest.clone(),
        });
    proof.token_digest = sha256(token);
    proof
}
fn fence_db(db: &Connection, profile: &str, id: &str, token: &str) -> Result<Value> {
    let fail = || Error("worker lease is expired, replaced or no longer active".into());
    let row = private_job(db, profile, id).map_err(|_| fail())?;
    let state = observed(&row)?;
    execution::reduce(
        &state,
        &Input::Fence {
            now: now(db)? as f64,
            proof: proof(&state, token),
        },
    )
    .map_err(|_| fail())?;
    Ok(row)
}
pub fn fence(store: &Store, profile: &str, id: &str, token: &str) -> Result<Value> {
    fence_db(&store.db, profile, id, token)
}
pub fn claim(
    store: &Store,
    profile: &str,
    processor: &str,
    worker: &str,
    lease_seconds: i64,
    resume: bool,
    job_ids: Option<&[String]>,
) -> Result<Value> {
    mutation::require_recovered(&store.db)?;
    let config = get(store, profile, processor)?;
    receipt_mutation::text(&json!(worker), "worker", 255)?;
    seconds(lease_seconds)?;
    if job_ids.is_some_and(|ids| ids.len() > 1000) {
        return Err(Error("job_ids must contain at most 1000 IDs".into()));
    }
    let scope = job_ids.map(|ids| encode(&json!(ids.iter().collect::<BTreeSet<_>>())));
    store.transaction(|db|{
        let timestamp=now(db)?;
        let old:Option<String>=if resume {db.query_row("SELECT id FROM processor_jobs WHERE profile=? AND processor=? AND worker=? AND state IN ('leased','submitted') AND lease_until>? AND (? IS NULL OR id IN (SELECT value FROM json_each(?))) ORDER BY updated_at,id LIMIT 1",params![profile,processor,worker,timestamp,scope,scope],|r|r.get(0)).optional()?}else{None};
        let id=if let Some(id)=old {
            let row=private_job(db,profile,&id)?;let state=observed(&row)?;let effect=execution::reduce(&state,&Input::Renew{now:timestamp as f64,duration:lease_seconds as f64,proof:proof(&state,row["lease_token"].as_str().unwrap_or(""))}).map_err(|_|Error("worker lease is expired, replaced or no longer active".into()))?;
            let Effect::Renew{until}=effect else {return Err(Error("invalid processor renewal effect".into()));};db.execute("UPDATE processor_jobs SET lease_until=? WHERE id=?",params![until as i64,id])?;id
        }else{
            let active:i64=db.query_row("SELECT count(*) FROM processor_jobs WHERE profile=? AND processor=? AND state IN ('leased','submitted') AND lease_until>?",params![profile,processor,timestamp],|r|r.get(0))?;
            if active>=config["capacity"].as_i64().unwrap_or(1){return Ok(json!({"claimed":false,"reason":"processor capacity is leased"}));}
            let id:Option<String>=db.query_row("SELECT id FROM processor_jobs WHERE profile=? AND processor=? AND (state='queued' OR (state IN ('leased','submitted') AND lease_until<=?)) AND (? IS NULL OR id IN (SELECT value FROM json_each(?))) ORDER BY created_at,id LIMIT 1",params![profile,processor,timestamp,scope,scope],|r|r.get(0)).optional()?;
            let Some(id)=id else{return Ok(json!({"claimed":false,"reason":"no queued or expired work"}));};let row=private_job(db,profile,&id)?;
            let effect=execution::reduce(&observed(&row)?,&Input::Start{now:timestamp as f64,duration:lease_seconds as f64}).map_err(|error|Error(format!("processor acquisition rejected: {error:?}")))?;
            let Effect::Acquire{generation,attempt,until}=effect else{return Err(Error("invalid processor acquisition effect".into()));};if attempt!=generation{return Err(Error("processor attempt and generation diverged".into()));}
            db.execute("UPDATE processor_attempts SET state='expired',finished_at=? WHERE job_id=? AND generation=? AND finished_at IS NULL",params![timestamp,id,row["generation"].as_i64()])?;
            let token=format!("{}{}",uuid::Uuid::new_v4().simple(),uuid::Uuid::new_v4().simple());
            db.execute("UPDATE processor_jobs SET state='leased',generation=?,worker=?,lease_token=?,lease_until=?,error=NULL,updated_at=CURRENT_TIMESTAMP WHERE id=?",params![generation,worker,token,until as i64,id])?;
            db.execute("INSERT INTO processor_attempts VALUES (?,?,?,'leased',?,NULL)",params![id,generation,worker,timestamp])?;id
        };let mut row=private_job(db,profile,&id)?;row["claimed"]=json!(true);Ok(row)
    })
}
pub fn heartbeat(
    store: &Store,
    profile: &str,
    id: &str,
    token: &str,
    lease_seconds: i64,
) -> Result<Value> {
    seconds(lease_seconds)?;
    store.transaction(|db| {
        let row = fence_db(db, profile, id, token)?;
        let state = observed(&row)?;
        let effect = execution::reduce(
            &state,
            &Input::Renew {
                now: now(db)? as f64,
                duration: lease_seconds as f64,
                proof: proof(&state, token),
            },
        )
        .map_err(|_| Error("worker lease is expired, replaced or no longer active".into()))?;
        let Effect::Renew { until } = effect else {
            return Err(Error("invalid processor renewal effect".into()));
        };
        db.execute(
            "UPDATE processor_jobs SET lease_until=?,updated_at=CURRENT_TIMESTAMP WHERE id=?",
            params![until as i64, id],
        )?;
        Ok(())
    })?;
    job(store, profile, id)
}
pub fn transition(
    store: &Store,
    profile: &str,
    id: &str,
    token: &str,
    state: &str,
) -> Result<Value> {
    if !["submitted", "failed"].contains(&state) {
        return Err(Error("invalid worker transition".into()));
    }
    store.transaction(|db| {
        let row = fence_db(db, profile, id, token)?;
        let timestamp = now(db)?;
        if state == "failed" {
            let observed = observed(&row)?;
            execution::reduce(
                &observed,
                &Input::Finish {
                    now: timestamp as f64,
                    proof: proof(&observed, token),
                    outcome: Phase::Failed,
                },
            )
            .map_err(|_| Error("worker lease is expired, replaced or no longer active".into()))?;
        }
        db.execute(
            "UPDATE processor_jobs SET state=?,updated_at=CURRENT_TIMESTAMP WHERE id=?",
            params![state, id],
        )?;
        db.execute(
            "UPDATE processor_attempts SET state=?,finished_at=? WHERE job_id=? AND generation=?",
            params![
                state,
                if state == "failed" {
                    Some(timestamp)
                } else {
                    None
                },
                id,
                row["generation"].as_i64()
            ],
        )?;
        Ok(())
    })?;
    job(store, profile, id)
}
pub fn retry(store: &Store, profile: &str, id: &str) -> Result<Value> {
    store.transaction(|db|{let row=private_job(db,profile,id)?;execution::reduce(&observed(&row)?,&Input::Retry{pending_artifact:false}).map_err(|_|Error("only failed or cancelled jobs can be explicitly retried".into()))?;db.execute("UPDATE processor_jobs SET state='queued',lease_token=NULL,lease_until=NULL,updated_at=CURRENT_TIMESTAMP WHERE id=?",[id])?;Ok(())})?;
    job(store, profile, id)
}
pub fn cancel(store: &Store, profile: &str, id: &str) -> Result<Value> {
    store.transaction(|db|{let row=private_job(db,profile,id)?;if row["state"]=="complete"{return Err(Error("a completed receipt cannot be cancelled".into()));}
    // Python permits explicit cancellation of failed jobs as well as live jobs.
    execution::reduce(&observed(&row)?,&Input::Cancel{publication_intent:false}).map_err(|_|Error("processor cancellation rejected".into()))?;
    db.execute("UPDATE processor_jobs SET state='cancelled',lease_token=NULL,lease_until=NULL,updated_at=CURRENT_TIMESTAMP WHERE id=?",[id])?;db.execute("UPDATE processor_attempts SET state='cancelled',finished_at=? WHERE job_id=? AND generation=? AND finished_at IS NULL",params![now(db)?,id,row["generation"].as_i64()])?;Ok(())})?;
    job(store, profile, id)
}
pub fn complete(
    store: &Store,
    profile: &str,
    id: &str,
    token: &str,
    receipt: &Value,
) -> Result<Value> {
    let row = fence(store, profile, id, token)?;
    let payload = &row["request"];
    let expected = json!({"database_id":payload["database_id"],"profile":profile,"producer":row["processor"],"instance":row["worker"],"job_id":id,"attempt":row["generation"].as_i64().unwrap().to_string(),"source":payload["source"],"configuration_digest":payload["configuration_digest"]});
    if !receipt.is_object()
        || expected
            .as_object()
            .unwrap()
            .iter()
            .any(|(key, value)| receipt.get(key) != Some(value))
    {
        return Err(Error(
            "processor receipt does not match the leased request".into(),
        ));
    }
    if !receipt["outputs"].as_array().is_some_and(|a| {
        !a.is_empty()
            && a.iter()
                .all(|o| o.is_object() && o["definition_id"] == payload["definition_id"])
    }) {
        return Err(Error(
            "processor output definition differs from the request".into(),
        ));
    }
    receipt_mutation::import_with_acceptance(store, profile, receipt, |db, receipt_id| {
        let current = fence_db(db, profile, id, token)?;
        let state = observed(&current)?;
        if current["request"] != row["request"]
            || current["worker"] != row["worker"]
            || current["generation"] != row["generation"]
        {
            return Err(Error("processor request changed during completion".into()));
        }
        execution::reduce(
            &state,
            &Input::Finish {
                now: now(db)? as f64,
                proof: proof(&state, token),
                outcome: Phase::Complete,
            },
        )
        .map_err(|_| Error("worker lease is expired, replaced or no longer active".into()))?;
        db.execute("UPDATE processor_jobs SET state='complete',receipt_id=?,lease_token=NULL,lease_until=NULL,updated_at=CURRENT_TIMESTAMP WHERE id=?",params![receipt_id,id])?;
        db.execute("UPDATE processor_attempts SET state='complete',finished_at=? WHERE job_id=? AND generation=?",params![now(db)?,id,current["generation"].as_i64()])?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receipt_mutation::tests::Fixture;
    fn setup(f: &Fixture, configuration: &Value) -> Value {
        put(
            &f.store,
            "default",
            "fixture",
            &json!({"endpoint":"http://127.0.0.1:9"}),
        )
        .unwrap();
        enqueue(
            &f.store,
            "default",
            "fixture",
            "source.mkv",
            "item",
            &f.definition,
            configuration,
            None,
        )
        .unwrap()
    }
    fn receipt(f: &Fixture, lease: &Value) -> Value {
        let mut value = f.receipt();
        for key in ["database_id", "source", "configuration_digest"] {
            value[key] = lease["request"][key].clone();
        }
        value["producer"] = lease["processor"].clone();
        value["instance"] = lease["worker"].clone();
        value["job_id"] = lease["id"].clone();
        value["attempt"] = json!(lease["generation"].as_i64().unwrap().to_string());
        value
    }
    fn token(lease: &Value) -> &str {
        lease["lease_token"].as_str().unwrap()
    }
    fn id(lease: &Value) -> &str {
        lease["id"].as_str().unwrap()
    }
    #[test]
    fn immutable_definitions_validate_settings_and_enqueue_has_exact_identity() {
        let f = Fixture::new();
        let original = setup(&f, &json!({"preset":"café"}));
        let same = setup(&f, &json!({"preset":"café"}));
        assert_eq!(original["id"], same["id"]);
        assert_eq!(original["id"].as_str().unwrap().len(), 32);
        assert!(original.get("lease_token").is_none());
        let changed = setup(&f, &json!({"preset":"different"}));
        assert_ne!(changed["id"], same["id"]);
        assert_eq!(
            original["request"]["configuration_digest"],
            sha256(encode(&json!({"preset":"café"})))
        );
        assert_eq!(original["digest"], sha256(encode(&original["request"])));
        assert!(
            put(
                &f.store,
                "default",
                "fixture",
                &json!({"endpoint":"http://127.0.0.1:10"})
            )
            .unwrap_err()
            .0
            .contains("immutable")
        );
        for value in [
            json!({"endpoint":"file:///tmp/x"}),
            json!({"endpoint":"http://name:secret@example.com"}),
            json!({"endpoint":"https://example.com?q=1"}),
            json!({"endpoint":"https://example.com/#x"}),
            json!({"endpoint":"https://example.com","capacity":true}),
            json!({"endpoint":"https://example.com","timeout":61}),
            json!({"endpoint":"https://example.com","credential_env":"bad-name"}),
            json!({"endpoint":"https://example.com","unexpected":1}),
        ] {
            assert!(definition(&value).is_err(), "{value}");
        }
        let normalized = definition(&json!({"endpoint":"http://127.0.0.1:9///"})).unwrap();
        assert_eq!(normalized["endpoint"], "http://127.0.0.1:9");
    }
    #[test]
    fn expired_generation_fences_old_worker_and_preserves_attempts() {
        let f = Fixture::new();
        let queued = setup(&f, &json!({}));
        let first = claim(&f.store, "default", "fixture", "worker-a", 300, false, None).unwrap();
        assert_eq!(first["id"], queued["id"]);
        assert_eq!(
            claim(&f.store, "default", "fixture", "worker-b", 300, false, None).unwrap()["claimed"],
            false
        );
        f.store
            .db
            .execute("UPDATE processor_jobs SET lease_until=0", [])
            .unwrap();
        let second = claim(&f.store, "default", "fixture", "worker-b", 300, false, None).unwrap();
        assert_eq!(second["generation"], 2);
        assert_ne!(first["lease_token"], second["lease_token"]);
        assert!(
            heartbeat(&f.store, "default", id(&first), token(&first), 300)
                .unwrap_err()
                .0
                .contains("lease")
        );
        assert!(
            complete(
                &f.store,
                "default",
                id(&first),
                token(&first),
                &receipt(&f, &first)
            )
            .is_err()
        );
        let result = complete(
            &f.store,
            "default",
            id(&second),
            token(&second),
            &receipt(&f, &second),
        )
        .unwrap();
        assert_eq!(result["reused"], false);
        assert_eq!(
            job(&f.store, "default", id(&second)).unwrap()["state"],
            "complete"
        );
        let states = f
            .store
            .db
            .prepare("SELECT state FROM processor_attempts ORDER BY generation")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(states, vec!["expired", "complete"]);
        assert!(
            cancel(&f.store, "default", id(&second))
                .unwrap_err()
                .0
                .contains("completed")
        );
        assert_eq!(
            std::fs::read(f.dir.path().join("media/source.mkv")).unwrap(),
            b"source.mkv"
        );
    }
    #[test]
    fn actor_request_definition_mismatch_and_late_expiry_cannot_publish() {
        let f = Fixture::new();
        setup(&f, &json!({}));
        let lease = claim(&f.store, "default", "fixture", "worker", 300, false, None).unwrap();
        for (key, value) in [
            ("attempt", json!("999")),
            ("instance", json!("impostor")),
            ("producer", json!("other")),
            ("configuration_digest", json!("0".repeat(64))),
        ] {
            let mut bad = receipt(&f, &lease);
            bad[key] = value;
            assert!(
                complete(&f.store, "default", id(&lease), token(&lease), &bad)
                    .unwrap_err()
                    .0
                    .contains("match")
            );
        }
        let mut bad = receipt(&f, &lease);
        bad["outputs"][0]["definition_id"] = json!("other");
        assert!(
            complete(&f.store, "default", id(&lease), token(&lease), &bad)
                .unwrap_err()
                .0
                .contains("definition differs")
        );
        f.store.db.execute_batch("CREATE TRIGGER test_expiry AFTER INSERT ON media_outputs BEGIN UPDATE processor_jobs SET lease_until=0; END;").unwrap();
        assert!(
            complete(
                &f.store,
                "default",
                id(&lease),
                token(&lease),
                &receipt(&f, &lease)
            )
            .unwrap_err()
            .0
            .contains("lease")
        );
        for table in ["media_outputs", "external_receipts", "receipt_outputs"] {
            assert_eq!(
                f.store
                    .db
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        assert_eq!(
            job(&f.store, "default", id(&lease)).unwrap()["state"],
            "leased"
        );
    }
    #[test]
    fn explicit_retry_cancel_resume_and_scope_preserve_generation() {
        let f = Fixture::new();
        let first = setup(&f, &json!({"number":1}));
        let second = setup(&f, &json!({"number":2}));
        assert_eq!(
            claim(
                &f.store,
                "default",
                "fixture",
                "worker",
                300,
                false,
                Some(&[])
            )
            .unwrap()["claimed"],
            false
        );
        let selected = vec![id(&second).to_owned()];
        let lease = claim(
            &f.store,
            "default",
            "fixture",
            "worker",
            300,
            false,
            Some(&selected),
        )
        .unwrap();
        assert_eq!(lease["id"], second["id"]);
        transition(&f.store, "default", id(&lease), token(&lease), "submitted").unwrap();
        let resumed = claim(&f.store, "default", "fixture", "worker", 300, true, None).unwrap();
        assert_eq!(resumed["state"], "submitted");
        assert_eq!(resumed["lease_token"], lease["lease_token"]);
        transition(&f.store, "default", id(&lease), token(&lease), "failed").unwrap();
        retry(&f.store, "default", id(&lease)).unwrap();
        let new = claim(
            &f.store,
            "default",
            "fixture",
            "worker",
            300,
            false,
            Some(&selected),
        )
        .unwrap();
        assert_eq!(new["generation"], 2);
        cancel(&f.store, "default", id(&new)).unwrap();
        assert!(
            complete(
                &f.store,
                "default",
                id(&new),
                token(&new),
                &receipt(&f, &new)
            )
            .is_err()
        );
        retry(&f.store, "default", id(&new)).unwrap();
        let next = claim(
            &f.store,
            "default",
            "fixture",
            "worker",
            300,
            false,
            Some(&selected),
        )
        .unwrap();
        assert_eq!(next["generation"], 3);
        assert_eq!(
            job(&f.store, "default", id(&first)).unwrap()["state"],
            "queued"
        );
        let page = jobs(&f.store, "default", "fixture", 1, "").unwrap();
        assert_eq!(page["jobs"].as_array().unwrap().len(), 1);
        assert!(page["next_after"].is_string());
        assert!(page["jobs"][0].get("lease_token").is_none());
    }
    #[test]
    fn accepted_receipt_can_atomically_acknowledge_still_live_processor_job() {
        let f = Fixture::new();
        setup(&f, &json!({}));
        let lease = claim(&f.store, "default", "fixture", "worker", 300, false, None).unwrap();
        let receipt = receipt(&f, &lease);
        let accepted = receipt_mutation::import_receipt(&f.store, "default", &receipt).unwrap();
        let completed = complete(&f.store, "default", id(&lease), token(&lease), &receipt).unwrap();
        assert_eq!(completed["reused"], true);
        assert_eq!(completed["receipt_id"], accepted["receipt_id"]);
        assert_eq!(
            job(&f.store, "default", id(&lease)).unwrap()["state"],
            "complete"
        );
    }
    #[test]
    fn independent_writers_cannot_lease_the_same_job() {
        let f = Fixture::new();
        setup(&f, &json!({}));
        let path = f.store.path.clone();
        drop(f.store);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut handles = vec![];
        for worker in ["one", "two"] {
            let path = path.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move||{barrier.wait();match Store::open(&path,true,false){Ok(store)=>claim(&store,"default","fixture",worker,300,false,None).unwrap()["claimed"]==true,Err(error)=>{assert!(error.0.contains("another Catabolic writer"));false}}}));
        }
        assert_eq!(
            handles
                .into_iter()
                .map(|h| usize::from(h.join().unwrap()))
                .sum::<usize>(),
            1
        );
        let store = Store::open(&path, true, false).unwrap();
        assert_eq!(
            claim(
                &store,
                "default",
                "fixture",
                "retry-after-contention",
                300,
                false,
                None
            )
            .unwrap()["claimed"],
            false
        );
    }
}
