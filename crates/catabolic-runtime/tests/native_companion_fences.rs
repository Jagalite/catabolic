//! Guard every catalog-bearing companion route before recovery or source access.
#![cfg(unix)]
use catabolic_runtime::{fallback, helper, watchers};
use catabolic_store::{Store, migration};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
};
fn catalog(schema: u32) -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.sqlite3");
    if schema == 32 {
        migration::initialize(&path).unwrap();
    } else {
        migration::initialize_native_at(&path, schema).unwrap();
    }
    let store = Store::open(&path, true, false).unwrap();
    (root, store)
}
fn envelope(store: &Store) -> Value {
    let md = fs::symlink_metadata(&store.path).unwrap();
    json!({"_native_catalog_fence":{"version":1,"schema":store.schema_version,"catalog_id":store.database_id,"incarnation":store.catalog_incarnation,"device":md.dev(),"inode":md.ino()}})
}
fn due(store: &Store) {
    store.transaction(|db| {
        db.execute("INSERT INTO watcher_definitions VALUES('definition','default','watch',1,?, 'digest',1)",[json!({"schedule":{"kind":"manual"}}).to_string()])?;
        db.execute("INSERT INTO watchers(profile,name,definition_id,requested_generation) VALUES('default','watch','definition',1)",[])?;Ok(())
    }).unwrap();
}
#[test]
fn native_watcher_and_probe_fence_rejection_precedes_recovery_or_source_io() {
    let (_root, store) = catalog(34);
    let path = store.path.clone();
    let mut valid = envelope(&store);
    valid["database"] = json!(path);
    valid["database_id"] = json!(store.database_id);
    valid["profile"] = json!("default");
    valid["snapshots"] = json!([{}]);
    let before = migration::snapshot(&store.db).unwrap();
    drop(store);
    let mut stale = valid.clone();
    stale["_native_catalog_fence"]["incarnation"] = json!("f".repeat(32));
    for request in [json!({"database":path,"snapshots":[{}]}), stale] {
        assert!(
            watchers::worker_with_options(&path, "default", "absent", &request)
                .unwrap_err()
                .0
                .contains("native")
        );
        assert!(
            fallback::probe_check(&request)
                .unwrap_err()
                .0
                .contains("native")
        );
    }
    assert!(
        watchers::worker_v1(&path, "default", "absent", &valid)
            .unwrap_err()
            .0
            .contains("unknown watcher")
    );
    // A correctly fenced request reaches the existing source-validation owner.
    assert_eq!(
        fallback::probe_check_v1(&valid).unwrap()["reason"],
        "source_unavailable_or_changed"
    );
    assert_eq!(
        migration::snapshot(&Store::open(&path, false, false).unwrap().db).unwrap(),
        before
    );
}
#[test]
fn versioned_companion_routes_reject_missing_fences_before_open() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("absent");
    assert!(watchers::worker_v1(&path, "default", "absent", &json!({})).is_err());
    assert!(fallback::probe_check_v1(&json!({"database":path})).is_err());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}
#[test]
fn native_watcher_and_probe_select_versioned_commands_with_no_old_helper_fallback() {
    let helper_root = tempfile::tempdir().unwrap();
    let binary = helper_root.path().join("old-helper");
    let commands = helper_root.path().join("commands");
    let observed = helper_root.path().join("legacy-source-work");
    fs::write(
        &binary,
        format!(
            r#"#!/bin/sh
printf '%s %s\n' "$1" "$5" >> '{}'
case "$1" in
  native-fallback-probe-v1) exit 64;;
esac
if [ "$5" = native-watcher-worker-v1 ]; then
  printf '%s' "$8" > "$2.envelope"
  exit 64
fi
printf 'legacy source work' > '{}'
printf '{{"complete":true,"usable":true}}'
"#,
            commands.display(),
            observed.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    helper::register(&binary, None).unwrap();
    let (_root, mut store) = catalog(34);
    due(&store);
    let path = store.path.clone();
    let id = store.database_id.clone();
    let before = migration::snapshot(&store.db).unwrap();
    let result = watchers::execute(
        &mut store,
        "default",
        "tick",
        &json!({"limit":1,"workers":1}),
    )
    .unwrap();
    assert_eq!(result["complete"], false);
    assert_eq!(migration::snapshot(&store.db).unwrap(), before);
    let child: Value =
        serde_json::from_slice(&fs::read(format!("{}.envelope", path.display())).unwrap()).unwrap();
    assert_eq!(
        child["_native_catalog_fence"],
        envelope(&store)["_native_catalog_fence"]
    );
    drop(store);
    assert_eq!(
        fallback::probe(&path, "default", &id, &json!([{}]), 1000).unwrap()["usable"],
        false
    );
    assert!(!observed.exists());
    assert_eq!(
        fs::read_to_string(commands).unwrap(),
        "--db native-watcher-worker-v1\nnative-fallback-probe-v1 \n"
    );
    let store = Store::open(&path, false, false).unwrap();
    assert_eq!(store.db.query_row("SELECT count(*) FROM fallback_probe_slots WHERE owner IS NOT NULL OR pid IS NOT NULL",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}
#[test]
fn legacy_watcher_and_probe_accept_unfenced_envelopes() {
    let (_root, store) = catalog(32);
    let path = store.path.clone();
    let id = store.database_id.clone();
    drop(store);
    assert!(
        watchers::worker_with_options(&path, "default", "absent", &json!({}))
            .unwrap_err()
            .0
            .contains("unknown watcher")
    );
    assert_eq!(
        fallback::probe_check(
            &json!({"database":path,"database_id":id,"profile":"default","snapshots":[{}]})
        )
        .unwrap()["reason"],
        "source_unavailable_or_changed"
    );
}
#[test]
#[ignore = "requires explicit pre-protocol artifact in CATABOLIC_OLD_HOST_TEST_HELPER"]
fn old_real_helper_rejects_native_watcher_and_fallback_routes() {
    let binary = PathBuf::from(std::env::var_os("CATABOLIC_OLD_HOST_TEST_HELPER").unwrap());
    helper::register(
        &binary,
        Some(&catabolic_core::sha256(fs::read(&binary).unwrap())),
    )
    .unwrap();
    let (_root, mut store) = catalog(34);
    due(&store);
    let path = store.path.clone();
    let id = store.database_id.clone();
    let before = migration::snapshot(&store.db).unwrap();
    assert_eq!(
        watchers::execute(
            &mut store,
            "default",
            "tick",
            &json!({"limit":1,"workers":1})
        )
        .unwrap()["complete"],
        false
    );
    assert_eq!(migration::snapshot(&store.db).unwrap(), before);
    drop(store);
    assert_eq!(
        fallback::probe(&path, "default", &id, &json!([{}]), 1000).unwrap()["usable"],
        false
    );
    assert_eq!(
        Store::open(&path, false, false)
            .unwrap()
            .db
            .query_row("SELECT count(*) FROM observations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
#[ignore = "requires rebuilt artifact in CATABOLIC_HOST_TEST_HELPER"]
fn rebuilt_helper_accepts_native_watcher_and_fallback_protocols() {
    real_companion_protocols(34);
}
#[test]
#[ignore = "requires rebuilt artifact in CATABOLIC_HOST_TEST_HELPER"]
fn rebuilt_helper_accepts_schema36_watcher_and_fallback_protocols() {
    real_companion_protocols(36);
}
fn real_companion_protocols(schema: u32) {
    use std::io::Write;
    let binary = PathBuf::from(std::env::var_os("CATABOLIC_HOST_TEST_HELPER").unwrap());
    let (_root, store) = catalog(schema);
    let path = store.path.clone();
    let mut request = envelope(&store);
    request["database"] = json!(path);
    request["database_id"] = json!(store.database_id);
    request["profile"] = json!("default");
    request["snapshots"] = json!([{}]);
    drop(store);
    let result = std::process::Command::new(&binary)
        .arg("--db")
        .arg(&path)
        .args([
            "--profile",
            "default",
            watchers::NATIVE_WORKER_COMMAND,
            "absent",
            "--options",
            &request.to_string(),
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("unknown watcher"));
    let mut child = std::process::Command::new(&binary)
        .args([fallback::NATIVE_PROBE_COMMAND, "check"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["reason"], "source_unavailable_or_changed");
}
