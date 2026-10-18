// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Narrow ordinary Reconciler adapter. Reads borrow the caller's Store; no
//! reopen, transaction, lock, or probe owner. Apply and recover go through
//! [`publish`] and the shared journal executor, never through [`call`].
use crate::{Error, Result, Store, filesystem_mutation};
use serde_json::{Value, json};

/// Refusal of apply/recover through the generic read dispatcher [`call`].
/// Publication uses [`publish`], which carries the `after_filesystem`
/// callback and the shared executor's gates.
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

/// Frozen preview budget validation, which also precedes apply's catalog
/// lookup because frozen apply starts with a preview.
fn removal_budget(args: &Value) -> Result<()> {
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
    Ok(())
}

/// `Reconciler.apply`/`recover` through the shared journal executor. Only
/// public fields are forwarded, so private options such as
/// `disposable_fixture` or failpoints cannot relax any gate. Symlink
/// replace/remove, hardlink retirement and fallback-bound catalogs keep the
/// executor's existing refusals. `after_filesystem` is called with each
/// operation after its filesystem effect and before its acknowledgement.
pub fn publish(
    store: &Store,
    profile: &str,
    operation: &str,
    args: &Value,
    after_filesystem: &dyn Fn(&Value) -> Result<()>,
) -> Result<Value> {
    if !matches!(operation, "apply" | "recover") {
        return Err(Error(format!(
            "unknown native Reconciler publication operation: {operation}"
        )));
    }
    if operation == "apply" {
        removal_budget(args)?;
    }
    let selected = catalog(args)?;
    catalogs(store, selected)?;
    let notify = args.get("notify_consumers").cloned().unwrap_or(json!(true));
    let request = if operation == "apply" {
        json!({
            "catalog":selected,
            "notify_consumers":notify,
            "max_removals":args["max_removals"],
            "max_removal_percent":args["max_removal_percent"],
            "max_changes":args["max_changes"],
        })
    } else {
        json!({
            "catalog":selected,
            "notify_consumers":notify,
            "cancel_unapplied":args.get("cancel_unapplied").cloned().unwrap_or(json!(false)),
        })
    };
    filesystem_mutation::execute_observed(store, profile, operation, &request, after_filesystem)
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
        removal_budget(args)?;
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

    /// A disposable catalog with one source file mapped to `folder/one.txt`.
    fn published_catalog() -> (tempfile::TempDir, Store, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.db");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let source = root.path().join("source");
        let output = root.path().join("output");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&output).unwrap();
        std::fs::write(source.join("one.txt"), "original source").unwrap();
        crate::observation::execute(
            &store,
            "default",
            "bind",
            &json!({"source":"media","root":source}),
        )
        .unwrap();
        crate::observation::execute(
            &store,
            "default",
            "bind",
            &json!({"kind":"output","owner":"global","root":output}),
        )
        .unwrap();
        crate::observation::execute(&store, "default", "scan", &json!({"source":"media"})).unwrap();
        store.transaction(|db|{db.execute("INSERT INTO items(id,kind,metadata) VALUES('item','movie','{}')",[])?;db.execute("INSERT INTO mappings(id,catalog,file_id,item_id,path) SELECT 'mapping','global',id,'item','folder/one.txt' FROM files WHERE path='one.txt'",[])?;Ok(())}).unwrap();
        (root, store, output)
    }

    #[test]
    fn publish_applies_through_the_shared_executor_and_observes_each_effect() {
        let (_root, store, output) = published_catalog();
        let seen = std::cell::RefCell::new(Vec::new());
        let observe = |operation: &Value| {
            // The effect has happened; the acknowledgement has not.
            let link = output.join("folder/one.txt");
            seen.borrow_mut().push((
                operation["kind"].clone(),
                std::fs::symlink_metadata(&link).is_ok(),
                operation["id"].is_string() && operation["profile"] == "default",
            ));
            Ok(())
        };
        let result = publish(
            &store,
            "default",
            "apply",
            &json!({"catalog":"global","disposable_fixture":true,"failpoint":"after_journal"}),
            &observe,
        )
        .unwrap();
        assert_eq!(result["healthy"], true, "{result}");
        assert_eq!(
            *seen.borrow(),
            [
                (json!("prepare"), false, true),
                (json!("create"), true, true)
            ]
        );
        // Frozen projection_stats execution snapshot.
        let key = format!(
            "statistics:{}",
            catabolic_core::encode(&json!(["default", "execution", "global"]))
        );
        let raw: String = store
            .db
            .query_row("SELECT value FROM meta WHERE key=?", [&key], |r| r.get(0))
            .unwrap();
        let snapshot: Value = catabolic_core::decode_json(&raw).unwrap();
        assert_eq!(snapshot["status"], "complete");
        assert_eq!(snapshot["planned_actions"], json!({"create":1,"prepare":1}));
        assert_eq!(snapshot["applied_actions"], json!({"create":1,"prepare":1}));
        assert_eq!(snapshot["verification"]["verified_links"], 1);
        assert_eq!(snapshot["verification"]["issue_count"], 0);
        assert_eq!(snapshot["inputs"]["desired_entries"], 1);
        // Unknown catalogs and budget errors keep the frozen order.
        assert_eq!(
            publish(
                &store,
                "default",
                "apply",
                &json!({"catalog":"missing","max_removals":-1}),
                &observe
            )
            .unwrap_err()
            .0,
            "max removals must be a nonnegative integer"
        );
        assert_eq!(
            publish(
                &store,
                "default",
                "recover",
                &json!({"catalog":"missing"}),
                &observe
            )
            .unwrap_err()
            .0,
            "unknown catalog: missing"
        );
        assert_eq!(
            publish(&store, "default", "preview", &json!({}), &observe)
                .unwrap_err()
                .0,
            "unknown native Reconciler publication operation: preview"
        );
    }

    #[test]
    fn callback_failure_after_the_effect_leaves_the_operation_recoverable() {
        let (_root, store, output) = published_catalog();
        let fail = |operation: &Value| {
            if operation["kind"] == "create" {
                Err(Error("callback raised".into()))
            } else {
                Ok(())
            }
        };
        let error = publish(&store, "default", "apply", &json!({}), &fail).unwrap_err();
        assert_eq!(error.0, "callback raised");
        // The link exists, but neither ownership nor the journal moved on.
        assert!(std::fs::symlink_metadata(output.join("folder/one.txt")).is_ok());
        let pending: i64 = store
            .db
            .query_row("SELECT count(*) FROM journal", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pending, 1);
        let owned: i64 = store
            .db
            .query_row("SELECT count(*) FROM owned_links", [], |r| r.get(0))
            .unwrap();
        assert_eq!(owned, 0);
        let recovered = std::cell::RefCell::new(Vec::new());
        let result = publish(
            &store,
            "default",
            "recover",
            &json!({"catalog":null}),
            &|operation: &Value| {
                recovered.borrow_mut().push(operation["kind"].clone());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(result["recovered"].as_array().unwrap().len(), 1, "{result}");
        assert_eq!(*recovered.borrow(), [json!("create")]);
        let pending: i64 = store
            .db
            .query_row("SELECT count(*) FROM journal", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pending, 0);
        assert_eq!(
            call(&store, "default", "verify", &json!({})).unwrap()["healthy"],
            true
        );
    }

    #[test]
    fn publish_refuses_inside_a_caller_transaction() {
        let (_root, store, output) = published_catalog();
        store.db.execute_batch("BEGIN").unwrap();
        let error = publish(&store, "default", "apply", &json!({}), &|_| Ok(())).unwrap_err();
        assert!(
            error.0.contains("outside an active transaction"),
            "{error:?}"
        );
        assert!(std::fs::read_dir(&output).unwrap().next().is_none());
        store.db.execute_batch("ROLLBACK").unwrap();
    }
}
