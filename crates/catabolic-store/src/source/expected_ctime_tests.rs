//! Real filesystem identity checks through the same borrowed-ctime validator.
use super::*;
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    directory: tempfile::TempDir,
    root: std::path::PathBuf,
    path: std::path::PathBuf,
    snapshot: Value,
    ctime: i64,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("root");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let path = root.join("sub/file.bin");
        std::fs::write(&path, b"original bytes").unwrap();
        let mut snapshot = revision(&File::open(&path).unwrap()).unwrap();
        let ctime = snapshot["ctime_ns"].as_i64().unwrap();
        snapshot.as_object_mut().unwrap().remove("ctime_ns");
        let root_stat = std::fs::metadata(&root).unwrap();
        snapshot["status"] = json!("present");
        snapshot["root"] = json!(root);
        snapshot["path"] = json!("sub/file.bin");
        snapshot["root_device"] = json!(root_stat.dev());
        snapshot["root_inode"] = json!(root_stat.ino());
        snapshot["source_policy"] = json!({"settings":{"uuid":"skip"}});
        Self {
            directory,
            root,
            path,
            snapshot,
            ctime,
        }
    }
}
fn change_ctime(path: &Path, old: i64) {
    // chmod changes ctime without changing the captured content size or mtime.
    // Allow coarse-timestamp filesystems a bounded interval, not guessed time.
    for attempt in 0..200 {
        let mode = if attempt % 2 == 0 { 0o600 } else { 0o640 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        if revision(&File::open(path).unwrap()).unwrap()["ctime_ns"].as_i64() != Some(old) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("filesystem did not record a ctime change within two seconds");
}

#[test]
fn borrowed_override_matches_injected_snapshot_and_leaves_ordinary_validation_unchanged() {
    let mut fixture = Fixture::new();
    // An obsolete ctime in the borrowed occurrence is replaced, not combined
    // with the separately retained expected scalar, just like the old clone.
    fixture.snapshot["ctime_ns"] = json!(fixture.ctime.checked_sub(1).unwrap());
    let before = fixture.snapshot.clone();
    assert!(validate(&fixture.snapshot).is_err());
    validate_expected_ctime(&fixture.snapshot, fixture.ctime).unwrap();
    assert_eq!(fixture.snapshot, before);
    let mut injected = fixture.snapshot.clone();
    injected["ctime_ns"] = json!(fixture.ctime);
    assert_eq!(
        validate(&injected).unwrap()["ctime_ns"].as_i64(),
        Some(fixture.ctime)
    );
    assert!(
        validate_expected_ctime(&fixture.snapshot, fixture.ctime.checked_add(1).unwrap()).is_err()
    );
}

#[test]
fn actual_ctime_change_refuses_before_operation_even_if_other_scan_fields_are_unchanged() {
    let fixture = Fixture::new();
    change_ctime(&fixture.path, fixture.ctime);
    let actual = revision(&File::open(&fixture.path).unwrap()).unwrap();
    for field in ["size", "mtime_ns", "device", "inode"] {
        assert_eq!(actual[field], fixture.snapshot[field]);
    }
    // Existing ordinary callers without a ctime retain their original contract.
    validate(&fixture.snapshot).unwrap();
    let called = std::cell::Cell::new(false);
    let error = with_validated_source_ctime(&fixture.snapshot, Some(fixture.ctime), |_| {
        called.set(true);
        Ok(())
    })
    .unwrap_err();
    assert!(!called.get());
    assert_eq!(
        error.0,
        "source revision changed; rescan and start a new operation"
    );
    assert!(validate_expected_ctime(&fixture.snapshot, fixture.ctime).is_err());
}

#[test]
fn borrowed_ctime_rewalk_refuses_a_parent_renamed_outside_the_root() {
    let fixture = Fixture::new();
    let error = with_validated_source_ctime(&fixture.snapshot, Some(fixture.ctime), |_| {
        std::fs::rename(
            fixture.root.join("sub"),
            fixture.directory.path().join("moved"),
        )?;
        Ok(())
    })
    .unwrap_err();
    assert_eq!(error.0, "source changed while reading; result discarded");
}

#[test]
fn borrowed_ctime_rechecks_root_identity_even_if_policy_skips_recorded_root_identity() {
    let mut fixture = Fixture::new();
    fixture.snapshot["source_policy"] =
        json!({"settings":{"device":"skip","inode":"skip","uuid":"skip"}});
    let error = with_validated_source_ctime(&fixture.snapshot, Some(fixture.ctime), |_| {
        std::fs::rename(&fixture.root, fixture.directory.path().join("old-root"))?;
        std::fs::create_dir_all(fixture.root.join("sub"))?;
        std::fs::write(&fixture.path, b"original bytes")?;
        Ok(())
    })
    .unwrap_err();
    assert_eq!(
        error.0,
        "source root changed while reading; result discarded"
    );
}

#[test]
fn borrowed_ctime_validates_both_held_descriptor_and_reopened_named_file() {
    for replace_name in [false, true] {
        let fixture = Fixture::new();
        let error = with_validated_source_ctime(&fixture.snapshot, Some(fixture.ctime), |held| {
            if replace_name {
                // The held descriptor still addresses the original inode, while
                // its former name now points at distinct equal-length content.
                std::fs::rename(&fixture.path, fixture.root.join("sub/old.bin"))?;
                std::fs::write(&fixture.path, b"replaced bytes")?;
            } else {
                // No path replacement: change the actual held file's metadata.
                held.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                change_ctime(&fixture.path, fixture.ctime);
            }
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.0, "source changed while reading; result discarded");
    }
}
