//! Native observation lifetime-fence qualification on disposable catalogs.
//! The controllable helper only exercises ownership/envelopes; it is not a
//! replacement for the separately required real native-helper scan fixture.
#![cfg(unix)]
use catabolic_runtime::{helper, observation};
use catabolic_store::{Store, migration};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

fn register_helper() {
    static HELPER: OnceLock<tempfile::TempDir> = OnceLock::new();
    HELPER.get_or_init(|| {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native-fence-fixture");
        fs::write(
            &path,
            r#"#!/bin/sh
printf '%s' "$6" > "$2.envelope"
printf '%s\n' "$5" >> "$2.commands"
n=0
while [ ! -f "$2.release" ]; do
  n=$((n+1)); [ "$n" -lt 1000 ] || exit 99
  /bin/sleep 0.01
done
case "$6" in
  *fixture_old_helper*)
    if [ "$5" != observation-worker ]; then
      printf 'catabolic: unsupported helper command\n' >&2
      exit 64
    fi
    printf 'legacy dispatch' > "$2.legacy-work"
    ;;
esac
printf '{"complete":true}'
"#,
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        helper::register(&path, None).unwrap();
        root
    });
}
fn catalog(version: u32) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.sqlite3");
    if version == 32 {
        migration::initialize(&path).unwrap();
    } else {
        migration::initialize_native_at(&path, version).unwrap();
    }
    (root, path)
}
fn envelope(store: &Store) -> Value {
    let metadata = fs::symlink_metadata(&store.path).unwrap();
    json!({"_native_catalog_id":store.database_id,"_native_catalog_fence":{
        "version":1,"schema":store.schema_version,"catalog_id":store.database_id,
        "incarnation":store.catalog_incarnation,"device":metadata.dev(),"inode":metadata.ino()
    }})
}
fn assert_no_admission(path: &Path) {
    let db = catabolic_store::connect(path, false).unwrap();
    for table in ["observation_jobs", "observation_requests", "scans"] {
        assert_eq!(
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "{table}"
        );
    }
}
fn release(path: &Path) {
    fs::write(format!("{}.release", path.display()), "release").unwrap();
}
fn read_envelope(path: &Path) -> Value {
    let path = PathBuf::from(format!("{}.envelope", path.display()));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(data) = fs::read(&path)
            && let Ok(value) = serde_json::from_slice(&data)
        {
            return value;
        }
        assert!(Instant::now() < deadline, "helper did not publish envelope");
        std::thread::sleep(Duration::from_millis(10));
    }
}
struct Permit(Arc<AtomicUsize>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn native_helper_rejects_stale_missing_or_malformed_fences_before_admission() {
    for version in 33..=migration::NATIVE_SCHEMA_VERSION {
        let (_root, path) = catalog(version);
        let store = Store::open(&path, true, false).unwrap();
        let valid = envelope(&store);
        drop(store);
        let mut cases = vec![json!({}), json!({"_native_catalog_fence":null})];
        for (field, value) in [
            ("version", json!(0)),
            ("version", json!(2)),
            ("version", json!("1")),
            ("schema", json!(32)),
            ("catalog_id", json!("00000000-0000-0000-0000-000000000000")),
            ("incarnation", json!("f".repeat(32))),
            ("device", json!(u64::MAX)),
            ("inode", json!(u64::MAX)),
        ] {
            let mut request = valid.clone();
            request["_native_catalog_fence"][field] = value;
            cases.push(request);
        }
        let mut missing = valid.clone();
        missing["_native_catalog_fence"]
            .as_object_mut()
            .unwrap()
            .remove("inode");
        cases.push(missing);
        let mut extra = valid.clone();
        extra["_native_catalog_fence"]["unknown"] = json!(true);
        cases.push(extra);
        for request in cases {
            assert!(
                observation::worker(&path, "default", &request)
                    .unwrap_err()
                    .0
                    .contains("native"),
                "{request}"
            );
            assert_no_admission(&path);
        }
        assert_eq!(
            observation::worker(&path, "absent-profile", &valid)
                .unwrap_err()
                .0,
            // The native v1 worker remains valid only through schema 36.
            // Latest schema 37 intentionally refuses it before profile lookup;
            // its closed companion path is qualified by separate owner tests.
            if version >= 37 {
                "schema37 requires the closed observation worker protocol"
            } else {
                "unknown profile: absent-profile"
            }
        );
        assert_no_admission(&path);
    }
}
#[test]
fn schema32_keeps_unfenced_envelopes_and_refuses_native_downgrade_envelopes() {
    let (_root, path) = catalog(32);
    assert_eq!(
        observation::worker(&path, "absent-profile", &json!({}))
            .unwrap_err()
            .0,
        "unknown profile: absent-profile"
    );
    let (_native_root, native_path) = catalog(33);
    let native = Store::open(&native_path, true, false).unwrap();
    let request = envelope(&native);
    assert!(
        observation::worker(&path, "default", &request)
            .unwrap_err()
            .0
            .contains("native lifetime")
    );
    assert_no_admission(&path);
}
#[test]
fn supervised_spawn_overwrites_untrusted_fence_and_keeps_pending_permit() {
    register_helper();
    for version in [32, 33] {
        let (_root, path) = catalog(version);
        let mut store = Store::open(&path, true, false).unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let started = observation::start_supervised(
            &mut store,
            "default",
            &json!({"_native_catalog_id":"attacker","_native_catalog_fence":{"version":999}}),
            Permit(drops.clone()),
        )
        .unwrap();
        assert!(started.reconnect_error.is_none());
        let mut worker = started.worker;
        let request = read_envelope(&path);
        assert_eq!(request["_native_catalog_id"], store.database_id);
        assert_eq!(
            fs::read_to_string(format!("{}.commands", path.display()))
                .unwrap()
                .trim(),
            if version == 32 {
                "observation-worker"
            } else {
                observation::NATIVE_WORKER_COMMAND
            }
        );
        if version == 32 {
            assert!(request.get("_native_catalog_fence").is_none());
        } else {
            assert_eq!(
                request["_native_catalog_fence"],
                envelope(&store)["_native_catalog_fence"]
            );
        }
        assert_eq!(worker.wait(&mut store, Duration::ZERO).unwrap(), None);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        release(&path);
        assert_eq!(
            worker.wait(&mut store, Duration::from_secs(3)).unwrap(),
            Some(json!({"complete":true}))
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}
#[test]
fn same_catalog_id_and_incarnation_on_a_replaced_file_cannot_resume_supervisor_access() {
    register_helper();
    let (root, path) = catalog(33);
    let mut store = Store::open(&path, true, false).unwrap();
    let original = envelope(&store);
    let drops = Arc::new(AtomicUsize::new(0));
    let started =
        observation::start_supervised(&mut store, "default", &json!({}), Permit(drops.clone()))
            .unwrap();
    let mut worker = started.worker;
    read_envelope(&path);
    drop(store);
    // A synthetic disposable replacement probes stale-descriptor refusal. It is
    // not a restore API or an authorization to replace a real catalog.
    let replacement = root.path().join("replacement.sqlite3");
    fs::copy(&path, &replacement).unwrap();
    fs::rename(&replacement, &path).unwrap();
    let mut reopened = Store::open(&path, true, false).unwrap();
    assert_eq!(
        reopened.database_id,
        original["_native_catalog_id"].as_str().unwrap()
    );
    assert_eq!(
        reopened.catalog_incarnation.as_ref().unwrap(),
        original["_native_catalog_fence"]["incarnation"]
            .as_str()
            .unwrap()
    );
    assert!(
        worker
            .wait(&mut reopened, Duration::ZERO)
            .unwrap_err()
            .0
            .contains("native lifetime")
    );
    assert!(
        worker
            .request_id(&reopened)
            .unwrap_err()
            .0
            .contains("native lifetime")
    );
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(reopened);
    assert!(
        observation::worker(&path, "default", &original)
            .unwrap_err()
            .0
            .contains("native lifetime")
    );
    assert_no_admission(&path);
    release(&path);
    assert!(
        worker
            .wait_without_store(Duration::from_secs(3))
            .unwrap()
            .is_some()
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
#[test]
fn isolated_native_path_also_supplies_the_versioned_fence() {
    register_helper();
    let (_root, path) = catalog(33);
    let mut store = Store::open(&path, true, false).unwrap();
    release(&path);
    let result = observation::execute(
        &mut store,
        "default",
        "scan",
        &json!({"source":"media","isolate":true,"_native_catalog_fence":{"version":999}}),
    )
    .unwrap();
    assert_eq!(result["complete"], true);
    assert_eq!(
        read_envelope(&path)["_native_catalog_fence"],
        envelope(&store)["_native_catalog_fence"]
    );
}
#[test]
fn valid_native_helper_fence_reaches_real_scan_owner() {
    let (root, path) = catalog(33);
    let store = Store::open(&path, true, false).unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("one.txt"), "fixture").unwrap();
    catabolic_store::observation::execute(
        &store,
        "default",
        "bind",
        &json!({"source":"media","root":source}),
    )
    .unwrap();
    let mut request = envelope(&store);
    request["source"] = json!("media");
    drop(store);
    assert_eq!(
        observation::worker(&path, "default", &request).unwrap()["complete"],
        true
    );
}

#[test]
fn old_helper_rejects_native_command_without_fallback_and_permit_waits_for_exit() {
    register_helper();
    let (root, path) = catalog(33);
    let mut store = Store::open(&path, true, false).unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("one.txt"), "unchanged").unwrap();
    catabolic_store::observation::execute(
        &store,
        "default",
        "bind",
        &json!({"source":"media","root":source}),
    )
    .unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let started = observation::start_supervised(
        &mut store,
        "default",
        &json!({"source":"media","fixture_old_helper":true}),
        Permit(drops.clone()),
    )
    .unwrap();
    let mut worker = started.worker;
    read_envelope(&path);
    assert_eq!(
        worker.wait(&mut store, Duration::from_millis(10)).unwrap(),
        None
    );
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    release(&path);
    assert!(
        worker
            .wait(&mut store, Duration::from_secs(3))
            .unwrap_err()
            .0
            .contains("unsupported helper command")
    );
    assert!(worker.is_finished());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(
        fs::read_to_string(format!("{}.commands", path.display())).unwrap(),
        format!("{}\n", observation::NATIVE_WORKER_COMMAND)
    );
    assert!(!PathBuf::from(format!("{}.legacy-work", path.display())).exists());
    assert_eq!(
        fs::read_to_string(source.join("one.txt")).unwrap(),
        "unchanged"
    );
    assert_no_admission(&path);
}

#[test]
fn isolated_native_old_helper_failure_does_not_retry_legacy_command() {
    register_helper();
    let (_root, path) = catalog(33);
    let mut store = Store::open(&path, true, false).unwrap();
    release(&path);
    assert!(
        observation::execute(
            &mut store,
            "default",
            "scan",
            &json!({"isolate":true,"fixture_old_helper":true})
        )
        .unwrap_err()
        .0
        .contains("unsupported helper command")
    );
    assert_eq!(
        fs::read_to_string(format!("{}.commands", path.display())).unwrap(),
        format!("{}\n", observation::NATIVE_WORKER_COMMAND)
    );
    assert!(!PathBuf::from(format!("{}.legacy-work", path.display())).exists());
    assert_no_admission(&path);
}

#[test]
fn versioned_helper_rejects_missing_envelope_before_database_open() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("must-not-exist.sqlite3");
    assert!(
        observation::worker_v1(&path, "default", &json!({}))
            .unwrap_err()
            .0
            .contains("requires a versioned fence")
    );
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
#[ignore = "requires rebuilt digest-checked native helper in CATABOLIC_HOST_TEST_HELPER"]
fn native34_real_helper_scan_cancel_drain_and_reopen() {
    real_helper_scan_cancel_drain_and_reopen(34);
}
#[test]
#[ignore = "requires rebuilt digest-checked native helper in CATABOLIC_HOST_TEST_HELPER"]
fn native36_real_helper_scan_cancel_drain_and_reopen() {
    real_helper_scan_cancel_drain_and_reopen(36);
}
fn real_helper_scan_cancel_drain_and_reopen(schema: u32) {
    use catabolic_runtime::host::{CONTRACT_VERSION, ErrorCode, HostSession, Lifecycle};
    let authority = catabolic_core::catalog_host_lease::HostAuthorization::new(
        "native-helper-fixture".into(),
        "revision-1".into(),
    )
    .unwrap();
    let binary = PathBuf::from(
        std::env::var_os("CATABOLIC_HOST_TEST_HELPER").expect("explicit rebuilt helper required"),
    );
    let digest = catabolic_core::sha256(fs::read(&binary).unwrap());
    let (root, path) = catalog(schema);
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    for index in 0..400 {
        fs::write(
            source.join(format!("file-{index}.txt")),
            "native worker fixture\n",
        )
        .unwrap();
    }
    let mut store = Store::open(&path, true, false).unwrap();
    catabolic_store::observation::execute_detached(
        &mut store,
        "default",
        "bind",
        &json!({"source":"media","root":source}),
    )
    .unwrap();
    drop(store);
    let mut host =
        HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, authority.clone())
            .unwrap();
    assert_eq!(host.identity().unwrap()["schema_version"], schema);
    host.register_helper(&binary, &digest).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let handle = host.start_scan("media", Permit(drops.clone())).unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(host.close().unwrap_err().code, ErrorCode::Busy);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(result) = host.cancel_scan(&handle).unwrap() {
            assert_eq!(result["state"], "cancelled");
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    while !host.drain(Duration::from_millis(100)).unwrap() {
        assert!(Instant::now() < deadline);
    }
    assert_eq!(host.lifecycle(), Lifecycle::Closed);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let mut host =
        HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, authority.clone())
            .unwrap();
    host.register_helper(&binary, &digest).unwrap();
    let handle = host.start_scan("media", Permit(drops.clone())).unwrap();
    let result = loop {
        if let Some(result) = host.poll_scan(&handle, Duration::from_millis(100)).unwrap() {
            break result;
        }
        assert!(Instant::now() < deadline);
    };
    assert_eq!(result["complete"], true);
    assert_eq!(drops.load(Ordering::SeqCst), 2);
    assert!(host.drain(Duration::ZERO).unwrap());
    let mut failed =
        HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, authority.clone())
            .unwrap();
    failed.register_helper(&binary, &digest).unwrap();
    failed
        .start_scan("missing-source", Permit(drops.clone()))
        .unwrap();
    loop {
        match failed.drain(Duration::from_millis(100)) {
            Ok(false) => assert!(Instant::now() < deadline),
            Err(error) => {
                assert_eq!(error.code, ErrorCode::Internal);
                break;
            }
            Ok(true) => panic!("missing source succeeded"),
        }
    }
    assert_eq!(failed.lifecycle(), Lifecycle::Closed);
    assert_eq!(drops.load(Ordering::SeqCst), 3);
    let reopened = Store::open(&path, false, false).unwrap();
    assert_eq!(reopened.schema_version, schema);
    assert_eq!(
        migration::validate(&reopened.db, true).unwrap()["schema"],
        schema
    );
    assert_eq!(
        reopened
            .db
            .query_row(
                "SELECT count(*) FROM observations WHERE status='present'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        400
    );
    assert_eq!(
        reopened
            .db
            .query_row(
                "SELECT count(*) FROM observation_jobs WHERE state='running'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
#[ignore = "requires explicit pre-protocol artifact in CATABOLIC_OLD_HOST_TEST_HELPER"]
fn old_real_helper_rejects_native_versioned_route_without_side_effects() {
    let binary = PathBuf::from(
        std::env::var_os("CATABOLIC_OLD_HOST_TEST_HELPER").expect("explicit older helper required"),
    );
    helper::register(
        &binary,
        Some(&catabolic_core::sha256(fs::read(&binary).unwrap())),
    )
    .unwrap();
    let (root, path) = catalog(34);
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("one.txt"), "untouched").unwrap();
    let mut store = Store::open(&path, true, false).unwrap();
    catabolic_store::observation::execute(
        &store,
        "default",
        "bind",
        &json!({"source":"media","root":source}),
    )
    .unwrap();
    let before = migration::snapshot(&store.db).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let started = observation::start_supervised(
        &mut store,
        "default",
        &json!({"source":"media"}),
        Permit(drops.clone()),
    )
    .unwrap();
    let mut worker = started.worker;
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "exit must be observed before release"
    );
    assert!(worker.wait(&mut store, Duration::from_secs(5)).is_err());
    assert!(worker.is_finished());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(migration::snapshot(&store.db).unwrap(), before);
    assert_no_admission(&path);
    assert_eq!(
        fs::read_to_string(source.join("one.txt")).unwrap(),
        "untouched"
    );
}
