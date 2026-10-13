// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Ordinary fallback values forwarded to the existing resolution/projection owners.
use crate::{
    Error, Result, Store,
    fallback_projection::PreparedProjection,
    fallback_resolution::{Context, Probe, Resolver},
    selection::Evaluation,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
};

pub fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .ok_or_else(|| Error(format!("{key} must be text")))
}

type PreparedValues = BTreeMap<u64, (String, PreparedProjection)>;

/// Adapter handles are process-local evidence, never persisted catalog state.
#[derive(Default)]
pub struct Plans {
    next: u64,
    releases: PlanReleases,
}
/// Release-only access does not borrow the Session or call Python. The registry
/// mutex never spans callbacks, Store access, or filesystem effects.
#[derive(Clone, Default)]
pub struct PlanReleases {
    values: Arc<Mutex<PreparedValues>>,
}
impl PlanReleases {
    fn values(&self) -> Result<MutexGuard<'_, PreparedValues>> {
        self.values
            .lock()
            .map_err(|_| Error("native prepared projection registry unavailable".into()))
    }
    pub fn forget(&self, token: u64) -> Result<()> {
        self.values()?.remove(&token);
        Ok(())
    }
}
impl Plans {
    pub fn releases(&self) -> PlanReleases {
        self.releases.clone()
    }
    pub fn remember(&mut self, profile: &str, plan: PreparedProjection) -> Result<Value> {
        let mut values = self.releases.values()?;
        if values.len() >= 256 {
            return Err(Error(
                "native prepared projection handle limit; release unused plans".into(),
            ));
        }
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| Error("native projection handle exhausted".into()))?;
        let result = json!({"token":self.next,"plan":plan.full_plan()});
        values.insert(self.next, (profile.into(), plan));
        Ok(result)
    }
    pub fn checked(&self, profile: &str, args: &Value) -> Result<PreparedProjection> {
        if args["plan_owner_matches"] != true {
            return Err(Error("native prepared projection identity mismatch".into()));
        }
        let token = args["plan_token"]
            .as_u64()
            .ok_or_else(|| Error("native prepared projection identity mismatch".into()))?;
        let values = self.releases.values()?;
        let (owner, plan) = values
            .get(&token)
            .ok_or_else(|| Error("native prepared projection identity mismatch".into()))?;
        if owner != profile || plan.full_plan() != &args["plan"] {
            return Err(Error(
                "native prepared projection changed or belongs to another profile".into(),
            ));
        }
        Ok(plan.clone())
    }
    pub fn forget(&mut self, token: u64) -> Result<()> {
        self.releases.forget(token)
    }
}

/// Serialize metadata-only bind/dry-run without rejecting a caller transaction.
/// This is the same publication lock name; it grants no filesystem admission.
pub fn serialization_lock(store: &Store) -> Result<crate::WriterLock> {
    let mut path = store.path.as_os_str().to_os_string();
    path.push(".publication");
    crate::writer_lock(std::path::Path::new(&path))
}

pub fn binding(store: &Store, profile: &str, catalog: &str) -> Result<Value> {
    if store.schema_version < 23 {
        return Ok(Value::Null);
    }
    Ok(crate::fallback_projection::binding(store, profile, catalog)?.unwrap_or(Value::Null))
}

/// Match the frozen preflight before allocating the lazy lifetime evaluation.
pub fn resolver_options(store: &Store, profile: &str, args: &Value) -> Result<Value> {
    if !store.owns_writer_lock() || !store.db.is_autocommit() {
        return Err(Error(
            "resolution requires a short writable session outside a transaction".into(),
        ));
    }
    let policy =
        crate::fallback_mutation::get(store, profile, text(args, "policy_id")?)?["definition"]
            .clone();
    let entries = args["entries"]
        .as_array()
        .ok_or_else(|| Error("entries must be an array".into()))?;
    if entries.is_empty()
        || entries.len() > policy["budgets"]["max_entries"].as_u64().unwrap() as usize
    {
        return Err(Error("resolution_entry_budget".into()));
    }
    for entry in entries {
        crate::fallback_resolution::logical_entry(entry)?;
    }
    Ok(policy["budgets"].clone())
}

pub fn resolve(evaluation: &mut Evaluation, args: &Value, probe: &mut Probe<'_>) -> Result<Value> {
    let entries = args["entries"]
        .as_array()
        .ok_or_else(|| Error("entries must be an array".into()))?;
    let previous: BTreeMap<String, Value> = if args["previous"].is_null() {
        BTreeMap::new()
    } else {
        serde_json::from_value(args["previous"].clone())
            .map_err(|_| Error("previous must be an object".into()))?
    };
    let mut resolver = Resolver::new(
        Context::Local(evaluation),
        probe,
        args["operation_id"].as_str().map(str::to_owned),
    );
    state(&mut resolver, args)?;
    let result = resolver.resolve(
        entries,
        text(args, "policy_id")?,
        args["catalog"].as_str().unwrap_or(""),
        &previous,
        args["manual_failback"] == true,
    )?;
    Ok(
        json!({"result":result,"total_checks":resolver.total_checks,"primary_choices":resolver.primary_choices}),
    )
}
fn state(resolver: &mut Resolver<'_, '_>, args: &Value) -> Result<()> {
    resolver.primary_choices =
        serde_json::from_value(args.get("primary_choices").cloned().unwrap_or(json!({})))
            .map_err(|_| Error("primary_choices must map item IDs to file IDs".into()))?;
    resolver.total_checks = args
        .get("total_checks")
        .map_or(Some(0), Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| Error("total_checks must be nonnegative".into()))?;
    Ok(())
}
pub fn health(store: &Store, profile: &str, args: &Value) -> Result<Value> {
    let mut no_probe = |_: &std::path::Path, _: &str, _: &str, _: &Value, _: u64| {
        Err(Error("health does not probe".into()))
    };
    let resolver = Resolver::new(Context::Borrowed(store, profile), &mut no_probe, None);
    resolver.health(
        text(args, "entry")?,
        &args["candidate"],
        text(args, "catalog")?,
    )
}
pub fn capture(evaluation: &mut Evaluation, args: &Value, probe: &mut Probe<'_>) -> Result<Value> {
    let policy = catabolic_core::fallback::definition(&args["policy"])?;
    if !args["row"].is_object() {
        return Err(Error("candidate row must be an object".into()));
    }
    let mut resolver = Resolver::new(
        Context::Local(evaluation),
        probe,
        args["operation_id"].as_str().map(str::to_owned),
    );
    state(&mut resolver, args)?;
    Ok(resolver
        .capture_with_checks(&args["row"], &policy, text(args, "catalog")?, false)?
        .unwrap_or(Value::Null))
}

pub fn configure_validate(store: &Store, profile: &str, args: &Value) -> Result<()> {
    if binding(store, profile, text(args, "catalog")?)?.is_null() {
        return Err(Error("bind an explicit fallback projection first".into()));
    }
    for (key, low, high) in [
        ("interval", 1., 86400.),
        ("max_changes", 1., 10000.),
        ("max_removals", 0., 10000.),
    ] {
        let number = args[key]
            .as_f64()
            .or_else(|| args[key].as_bool().map(|value| if value { 1. } else { 0. }));
        if !number.is_some_and(|n| n >= low && n <= high) {
            return Err(Error("invalid fallback maintenance limits".into()));
        }
    }
    Ok(())
}
pub fn configure(store: &Store, profile: &str, args: &Value) -> Result<Value> {
    configure_validate(store, profile, args)?;
    let enabled = args["enabled"]
        .as_i64()
        .ok_or_else(|| Error("enabled must be a SQLite integer".into()))?;
    let catalog = text(args, "catalog")?;
    store.transaction(|db| {
        db.execute("UPDATE fallback_bindings SET enabled=?,interval_seconds=?,max_changes=?,max_removals=?,next_attempt=0 WHERE profile=? AND catalog=?", rusqlite::params![enabled,crate::sql::scalar(&args["interval"])?,crate::sql::scalar(&args["max_changes"])?,crate::sql::scalar(&args["max_removals"])?,profile,catalog])?;
        db.execute("INSERT INTO catalog_refresh_settings VALUES (?,?,?,?) ON CONFLICT(profile,catalog) DO UPDATE SET enabled=excluded.enabled,max_removals=excluded.max_removals",rusqlite::params![profile,catalog,enabled,crate::sql::scalar(&args["max_removals"])?])?;
        Ok(())
    })?;
    binding(store, profile, catalog)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poisoned_plan_release_registry_refuses_further_access() {
        let plans = Plans::default();
        let released = plans.releases();
        let poisoned = released.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = poisoned.values.lock().unwrap();
                panic!("release registry poison witness");
            })
            .join()
            .is_err()
        );
        assert_eq!(
            released.forget(1).unwrap_err().0,
            "native prepared projection registry unavailable"
        );
        assert_eq!(
            plans
                .checked(
                    "default",
                    &json!({
                        "plan_owner_matches":true,"plan_token":1,"plan":{}
                    })
                )
                .err()
                .unwrap()
                .0,
            "native prepared projection registry unavailable"
        );
    }
}
