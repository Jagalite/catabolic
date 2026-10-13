// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Narrow ordinary Reconciler adapter. Publication remains explicitly gated.
//! Reads borrow the caller's Store; no reopen, transaction, lock, or probe owner.
use crate::{Error, Result, Store, filesystem_mutation};
use serde_json::{Value, json};

pub const PUBLICATION_GATE: &str = "native Reconciler apply/recover is blocked: filesystem publication and recovery are not yet qualified";

/// The same marker value used by the production filesystem owner. Kept pure so
/// the Python owner accessor can still read cached identity after Store.close().
pub fn owner(database_id: &str, profile: &str, catalog: &str) -> Value {
    json!({"format":1,"database_id":database_id,"profile":profile,"catalog":catalog})
}

fn catalog(args: &Value) -> Result<Option<&str>> {
    match args.get("catalog") {
        Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        None => Ok(Some("global")),
        _ => Err(Error("catalog must be text or null".into())),
    }
}

fn catalogs(store: &Store, selected: Option<&str>) -> Result<Value> {
    if let Some(selected) = selected {
        if !store
            .db
            .prepare("SELECT id FROM catalogs WHERE id=?")?
            .exists([selected])?
        {
            return Err(Error(format!("unknown catalog: {selected}")));
        }
        Ok(json!([selected]))
    } else {
        Ok(json!(
            store
                .db
                .prepare("SELECT id FROM catalogs ORDER BY id")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        ))
    }
}

pub fn call(store: &Store, profile: &str, operation: &str, args: &Value) -> Result<Value> {
    // Always refuse before catalog resolution, callbacks, publication locking,
    // journaling, or effects. Unknown/private options cannot relax this boundary.
    if matches!(operation, "apply" | "recover") {
        return Err(Error(PUBLICATION_GATE.into()));
    }
    if !matches!(operation, "catalogs" | "preview" | "verify") {
        return Err(Error(format!(
            "unknown native Reconciler operation: {operation}"
        )));
    }
    // Preserve the public ordering: budget errors precede catalog lookup. JSON
    // cannot represent nonfinite floats; the Python adapter sends a text sentinel.
    if operation == "preview" {
        if args
            .get("max_removals")
            .is_some_and(|v| !v.is_null() && v.as_u64().is_none())
        {
            return Err(Error("max removals must be a nonnegative integer".into()));
        }
        if args.get("max_removal_percent").is_some_and(|v| {
            !v.is_null()
                && !v
                    .as_f64()
                    .is_some_and(|v| v.is_finite() && (0.0..=100.0).contains(&v))
        }) {
            return Err(Error(
                "max removal percent must be finite and between 0 and 100".into(),
            ));
        }
    }
    let selected = catalog(args)?;
    let known = catalogs(store, selected)?;
    if operation == "catalogs" {
        return Ok(known);
    }
    // Forward only public read fields. In particular, never forward a private
    // disposable fixture option to a production preview gate.
    let request = json!({
        "catalog":selected,
        "max_removals":args["max_removals"],
        "max_removal_percent":args["max_removal_percent"],
    });
    filesystem_mutation::execute(store, profile, operation, &request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_transaction_and_public_budget_order() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        store
            .db
            .execute_batch("BEGIN; INSERT INTO catalogs(id) VALUES ('staged')")
            .unwrap();
        assert_eq!(
            call(&store, "default", "catalogs", &json!({"catalog":"staged"})).unwrap(),
            json!(["staged"])
        );
        assert_eq!(
            call(
                &store,
                "default",
                "preview",
                &json!({"catalog":"missing", "max_removals":-1})
            )
            .unwrap_err()
            .0,
            "max removals must be a nonnegative integer"
        );
        assert_eq!(
            call(&store, "default", "verify", &json!({"catalog":"missing"}))
                .unwrap_err()
                .0,
            "unknown catalog: missing"
        );
        assert!(!store.db.is_autocommit());
        store.db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            call(&store, "default", "catalogs", &json!({"catalog":null})).unwrap(),
            json!(["global"])
        );
    }

    #[test]
    fn unconditional_public_gate_ignores_private_fixture_options() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        store.db.execute_batch("BEGIN; INSERT INTO journal(id,profile,catalog,path,kind) VALUES ('pending','default','global','','prepare')").unwrap();
        for operation in ["apply", "recover"] {
            for request in [
                json!({}),
                json!({"catalog":null,"disposable_fixture":true}),
                json!({"catalog":"missing","disposable_fixture":true,"cancel_unapplied":true}),
            ] {
                assert_eq!(
                    call(&store, "default", operation, &request).unwrap_err().0,
                    PUBLICATION_GATE
                );
            }
        }
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM journal", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(!store.db.is_autocommit());
        assert!(!root.path().join("catalog.db.publication").exists());
        store.db.execute_batch("ROLLBACK").unwrap();
    }
}
