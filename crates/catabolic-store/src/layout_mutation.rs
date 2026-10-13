// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Reviewed complete desired-layout application; filesystem publication stays separate.
use crate::{Error, Result, Store, definitions, layout, selection::Evaluation};
use catabolic_core::{encode, sha256};
use rusqlite::params;
use serde_json::{Value, json};

pub struct PreparedLayout {
    pub report: Value,
    database_id: String,
    profile: String,
    epoch: i64,
    prepared: layout::PreparedPlan,
    digest: String,
}
impl PreparedLayout {
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn preview(&self, limit: usize) -> Result<Value> {
        valid_limit(limit)?;
        Ok(layout::bounded_report(&self.report, limit, false))
    }
}
fn valid_limit(limit: usize) -> Result<()> {
    if !(1..=1000).contains(&limit) {
        return Err(Error("limit must be between 1 and 1000".into()));
    }
    Ok(())
}
fn epoch(store: &Store) -> Result<i64> {
    Ok(store.db.query_row(
        "SELECT generation FROM fallback_epoch WHERE id=1",
        [],
        |r| r.get(0),
    )?)
}
fn digest(database: &str, profile: &str, epoch: i64, prepared: &layout::PreparedPlan) -> String {
    sha256(encode(&json!([
        database,
        profile,
        epoch,
        prepared.report,
        prepared.desired,
        prepared.owned,
        prepared.rendition_associations
    ])))
}
pub fn prepare(
    store: &Store,
    profile: &str,
    id: &str,
    catalog: &str,
    replace_layout: bool,
    allow_empty: bool,
) -> Result<PreparedLayout> {
    if store.writable && !store.db.is_autocommit() {
        return Err(Error(
            "layout preparation requires a fresh snapshot outside a transaction".into(),
        ));
    }
    let snapshot = Store::open(&store.path, false, false)?;
    if snapshot.database_id != store.database_id {
        return Err(Error(
            "database identity changed before layout planning".into(),
        ));
    }
    let generation = epoch(&snapshot)?;
    let mut evaluation = Evaluation::new(snapshot, profile, 60000, 100000, false)?;
    let prepared = layout::prepare(&mut evaluation, id, catalog, replace_layout, allow_empty)?;
    let seal = digest(&store.database_id, profile, generation, &prepared);
    Ok(PreparedLayout {
        report: prepared.report.clone(),
        database_id: store.database_id.clone(),
        profile: profile.into(),
        epoch: generation,
        prepared,
        digest: seal,
    })
}
/// Adapter-only preparation on an already borrowed embedding owner. Unlike
/// `prepare`, this does not reopen the database or discard uncommitted state.
/// The same native plan and seal are consumed by the unchanged apply owner.
pub(crate) fn prepare_evaluated(
    evaluation: &mut Evaluation,
    id: &str,
    catalog: &str,
    flags: &mut impl FnMut(layout::RunFlag) -> Result<bool>,
) -> Result<PreparedLayout> {
    let database_id = evaluation.store().database_id.clone();
    let profile = evaluation.profile.clone();
    let generation = epoch(evaluation.store())?;
    let prepared = layout::prepare_with_flags(evaluation, id, catalog, flags)?;
    let seal = digest(&database_id, &profile, generation, &prepared);
    Ok(PreparedLayout {
        report: prepared.report.clone(),
        database_id,
        profile,
        epoch: generation,
        prepared,
        digest: seal,
    })
}
/// Can be nested in a caller transaction that rolls back mappings on a blocked
/// filesystem preview. The complete mapping set never comes from display JSON.
pub fn apply(
    store: &Store,
    profile: &str,
    prepared: &PreparedLayout,
    limit: usize,
) -> Result<Value> {
    valid_limit(limit)?;
    store.transaction(|db|{
        definitions::require_recovered(store)?;
        if store.database_id!=prepared.database_id||profile!=prepared.profile||epoch(store)?!=prepared.epoch||prepared.report!=prepared.prepared.report||digest(&store.database_id,profile,prepared.epoch,&prepared.prepared)!=prepared.digest{return Err(Error("stale layout plan; prepare the current catalog again before applying".into()));}
        let plan=&prepared.prepared;
        if plan.report["safe"]!=true{return Ok(layout::bounded_report(&plan.report,limit,false));}
        let catalog=plan.report["catalog"].as_str().unwrap();
        for change in plan.report["changes"].as_array().unwrap(){if change["action"]=="disable"{db.execute("UPDATE mappings SET active=0 WHERE id=?",[change["id"].as_str().unwrap()])?;}}
        for row in &plan.desired{db.execute("INSERT INTO mappings(id,catalog,file_id,item_id,path,active) VALUES (?,?,?,?,?,1) ON CONFLICT(id) DO UPDATE SET active=1",params![row["id"].as_str(),catalog,row["file_id"].as_str(),row["item_id"].as_str(),row["path"].as_str()])?;}
        let mut owned=plan.owned.clone();for row in &plan.desired{owned.insert(row["id"].as_str().unwrap().to_owned());}
        let state=json!({"layout":plan.report["layout"],"definition_sha256":plan.report["definition_sha256"],"mapping_ids":owned,"rendition_associations":plan.rendition_associations});
        db.execute("INSERT INTO meta(key,value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![format!("layout-catalog:{catalog}"),encode(&state)])?;
        let mut stats=plan.report["selection_statistics"].clone();stats["evaluated_at"]=json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros,false));
        let key=format!("statistics:{}",encode(&json!([profile,"selection",catalog])));
        db.execute("INSERT INTO meta(key,value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,encode(&stats)])?;
        Ok(layout::bounded_report(&plan.report,limit,true))
    })
}
pub fn dispatch(store: &Store, profile: &str, action: &str, args: &Value) -> Result<Value> {
    let id = args["id"]
        .as_str()
        .ok_or_else(|| Error("id must be text".into()))?;
    let catalog = args
        .get("catalog")
        .and_then(Value::as_str)
        .unwrap_or("global");
    let limit = args
        .get("limit")
        .map(Value::as_u64)
        .unwrap_or(Some(100))
        .filter(|n| (1..=1000).contains(n))
        .ok_or_else(|| Error("limit must be between 1 and 1000".into()))? as usize;
    let prepared = prepare(
        store,
        profile,
        id,
        catalog,
        args["replace_layout"] == true,
        args["allow_empty"] == true,
    )?;
    if !args["expected_plan"].is_null() {
        let expected = args["expected_plan"]
            .as_str()
            .ok_or_else(|| Error("expected_plan must be text".into()))?;
        if expected != prepared.digest() {
            return Err(Error(
                "stale layout plan; prepare the current catalog again before applying".into(),
            ));
        }
    }
    match action {
        "preview" => prepared.preview(limit),
        "prepare" => Ok(json!({"plan":prepared.preview(limit)?,"plan_digest":prepared.digest()})),
        "apply" => apply(store, profile, &prepared, limit),
        _ => Err(Error("unknown layout mutation operation".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.sqlite3");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        store.transaction(|db|{
            db.execute_batch("INSERT INTO locations(id) VALUES ('media'); INSERT INTO files(id,location,path) VALUES ('file','media','book.txt'); INSERT INTO items(id,kind,metadata) VALUES ('book','book','{\"title\":\"Book\"}'); INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES ('association','file','book','primary','explicit');")?;
            Ok(())
        }).unwrap();
        definitions::dispatch(&store,"default","layout-put",&json!({"name":"flat","value":{"version":1,"rules":[{"name":"all","path":"{item.kind}/{item.id}/{file.name}"}]}})).unwrap();
        (root, store)
    }
    #[test]
    fn outer_failure_rolls_back_complete_mapping_owner_and_statistics() {
        let (_root, store) = fixture();
        let prepared = prepare(&store, "default", "flat", "global", false, false).unwrap();
        let generation = epoch(&store).unwrap();
        let result: Result<()> = store.transaction(|db| {
            assert_eq!(apply(&store, "default", &prepared, 1)?["applied"], true);
            assert_eq!(
                db.query_row("SELECT count(*) FROM mappings", [], |r| r.get::<_, i64>(0))?,
                1
            );
            Err(Error("injected blocked filesystem preview".into()))
        });
        assert!(result.is_err());
        assert_eq!(epoch(&store).unwrap(), generation);
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM mappings", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(store.db.query_row("SELECT count(*) FROM meta WHERE key LIKE 'layout-catalog:%' OR key LIKE 'statistics:%'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        assert_eq!(
            apply(&store, "default", &prepared, 1).unwrap()["applied"],
            true
        );
    }
    #[test]
    fn stale_or_tampered_prepared_plan_refuses_before_mutation() {
        let (_root, store) = fixture();
        let mut prepared = prepare(&store, "default", "flat", "global", false, false).unwrap();
        prepared.report["safe"] = json!(false);
        assert!(
            apply(&store, "default", &prepared, 100)
                .unwrap_err()
                .0
                .contains("stale layout plan")
        );
        let prepared = prepare(&store, "default", "flat", "global", false, false).unwrap();
        store
            .transaction(|db| {
                db.execute("UPDATE items SET metadata='{}' WHERE id='book'", [])?;
                Ok(())
            })
            .unwrap();
        assert!(
            apply(&store, "default", &prepared, 100)
                .unwrap_err()
                .0
                .contains("stale layout plan")
        );
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM mappings", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
