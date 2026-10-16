// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Operator lifecycle commands for the explicit native upgrade. These drive the
//! real executable against disposable catalogs; refusals must leave bytes intact.
use catabolic_store::{connect, migration};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

const QUIESCENT: [&str; 6] = [
    "--assert-quiescent",
    "admission-stopped",
    "--assert-quiescent",
    "readers-and-workers-stopped",
    "--assert-quiescent",
    "exclusive-operational-ownership",
];

struct Outcome {
    code: Option<i32>,
    stdout: Value,
    stderr: String,
}
fn cli(database: &Path, args: &[&str]) -> Outcome {
    let output = Command::new(env!("CARGO_BIN_EXE_catabolic-native"))
        .arg("--db")
        .arg(database)
        .args(args)
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    Outcome {
        code: output.status.code(),
        stdout: if stdout.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&stdout).unwrap()
        },
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}
fn refused(outcome: &Outcome, message: &str) {
    assert_eq!(outcome.code, Some(2), "{}", outcome.stderr);
    assert!(outcome.stdout.is_null());
    assert!(
        outcome.stderr.contains(message),
        "expected {message:?} in {:?}",
        outcome.stderr
    );
}
fn catalog() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog.sqlite3");
    migration::initialize(&path).unwrap();
    (root, path)
}
fn plan(root: &Path, database: &Path, name: &str) -> PathBuf {
    let out = root.join(name);
    let outcome = cli(
        database,
        &["db", "native-plan", "--out", out.to_str().unwrap()],
    );
    assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    out
}

#[test]
fn native_inspect_reports_pending_native_steps_without_claiming_quiescence() {
    let (_root, path) = catalog();
    let before = fs::read(&path).unwrap();
    let outcome = cli(&path, &["db", "native-inspect", "--full"]);
    assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    let info = outcome.stdout;
    assert_eq!(info["schema"], 32);
    assert_eq!(info["latest_schema"], migration::NATIVE_SCHEMA_VERSION);
    assert_eq!(
        info["pending_migrations"].as_array().unwrap().len(),
        (migration::NATIVE_SCHEMA_VERSION - migration::SCHEMA_VERSION) as usize
    );
    assert_eq!(info["journal_mode"], "delete");
    assert_eq!(info["work_settled"], true);
    assert_eq!(info["requires_quiescence"], true);
    assert_eq!(info["quiescence_verified"], false);
    assert_eq!(info["safe"], false);
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn native_plan_requires_a_new_private_output_file() {
    let (root, path) = catalog();
    refused(
        &cli(&path, &["db", "native-plan"]),
        "requires --out PLAN.json",
    );
    let out = root.path().join("plan.json");
    let outcome = cli(
        &path,
        &["db", "native-plan", "--out", out.to_str().unwrap()],
    );
    assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    let written: Value = serde_json::from_slice(&fs::read(&out).unwrap()).unwrap();
    assert_eq!(written["contract"], "catabolic-native-upgrade-v1");
    assert_eq!(
        outcome.stdout["plan_fingerprint"],
        written["plan_fingerprint"]
    );
    assert_eq!(
        outcome.stdout["input_fingerprint"],
        written["input_fingerprint"]
    );
    assert_eq!(outcome.stdout["from_schema"], 32);
    assert_eq!(
        outcome.stdout["to_schema"],
        migration::NATIVE_SCHEMA_VERSION
    );
    assert_eq!(outcome.stdout["plan"], out.to_str().unwrap());
    assert_eq!(
        fs::metadata(&out).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // An approved plan is never replaced, including through a symlink.
    let approved = fs::read(&out).unwrap();
    refused(
        &cli(
            &path,
            &["db", "native-plan", "--out", out.to_str().unwrap()],
        ),
        "cannot create",
    );
    assert_eq!(fs::read(&out).unwrap(), approved);
    let link = root.path().join("link.json");
    std::os::unix::fs::symlink(root.path().join("elsewhere.json"), &link).unwrap();
    refused(
        &cli(
            &path,
            &["db", "native-plan", "--out", link.to_str().unwrap()],
        ),
        "cannot create",
    );
    assert!(!root.path().join("elsewhere.json").exists());
}

#[test]
fn lifecycle_options_are_refused_outside_their_commands() {
    let (root, path) = catalog();
    let before = fs::read(&path).unwrap();
    let out = root.path().join("plan.json");
    refused(
        &cli(&path, &["db", "upgrade", "--out", out.to_str().unwrap()]),
        "--out is only accepted by db native-plan and db identity-manifest",
    );
    refused(
        &cli(
            &path,
            &["db", "native-plan", "--plan", out.to_str().unwrap()],
        ),
        "only accepted by db native-upgrade",
    );
    refused(
        &cli(
            &path,
            &[
                "db",
                "native-inspect",
                "--assert-quiescent",
                "admission-stopped",
            ],
        ),
        "only accepted by db native-upgrade",
    );
    assert!(!out.exists());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn native_upgrade_refusals_are_explicit_and_leave_the_catalog_unchanged() {
    let (root, path) = catalog();
    let plan = plan(root.path(), &path, "plan.json");
    let plan = plan.to_str().unwrap();
    let before = fs::read(&path).unwrap();
    refused(
        &cli(&path, &["db", "native-upgrade"]),
        "requires --plan PLAN.json",
    );
    refused(
        &cli(&path, &["db", "native-upgrade", "--plan", plan]),
        "explicit operator confirmation",
    );
    refused(
        &cli(
            &path,
            &[
                "db",
                "native-upgrade",
                "--plan",
                plan,
                "--assert-quiescent",
                "admission-stopped",
                "--assert-quiescent",
                "readers-and-workers-stopped",
            ],
        ),
        "explicit operator confirmation",
    );
    refused(
        &cli(
            &path,
            &[
                "db",
                "native-upgrade",
                "--plan",
                plan,
                "--assert-quiescent",
                "everything-is-fine",
            ],
        ),
        "unknown --assert-quiescent value",
    );
    let missing = root.path().join("missing.json");
    let mut args = vec!["db", "native-upgrade", "--plan", missing.to_str().unwrap()];
    args.extend(QUIESCENT);
    refused(&cli(&path, &args), "cannot read");
    let mut tampered: Value = serde_json::from_slice(&fs::read(plan).unwrap()).unwrap();
    tampered["input_fingerprint"] = "tampered".into();
    let tampered_path = root.path().join("tampered.json");
    fs::write(&tampered_path, serde_json::to_vec(&tampered).unwrap()).unwrap();
    let mut args = vec![
        "db",
        "native-upgrade",
        "--plan",
        tampered_path.to_str().unwrap(),
    ];
    args.extend(QUIESCENT);
    refused(&cli(&path, &args), "plan or inputs changed");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!PathBuf::from(format!("{}.backups", path.display())).exists());
}

#[test]
fn native_upgrade_failpoint_rolls_back_then_the_approved_plan_commits() {
    let (root, path) = catalog();
    let plan = plan(root.path(), &path, "plan.json");
    let backups = root.path().join("backups");
    let before = fs::read(&path).unwrap();
    let mut args = vec![
        "db",
        "native-upgrade",
        "--plan",
        plan.to_str().unwrap(),
        "--backup-dir",
        backups.to_str().unwrap(),
        "--migration-failpoint",
        "upgrade:before_commit:37",
    ];
    args.extend(QUIESCENT);
    refused(
        &cli(&path, &args),
        "injected migration failure: upgrade:before_commit:37",
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    let failed = fs::read_dir(&backups)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let receipt: Value =
        serde_json::from_slice(&fs::read(failed.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(receipt["status"], "failed");
    assert_eq!(receipt["quiescence"]["operator_asserted"], true);

    let mut args = vec![
        "db",
        "native-upgrade",
        "--plan",
        plan.to_str().unwrap(),
        "--backup-dir",
        backups.to_str().unwrap(),
    ];
    args.extend(QUIESCENT);
    let outcome = cli(&path, &args);
    assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    assert_eq!(outcome.stdout["upgraded"], true);
    assert_eq!(outcome.stdout["from_schema"], 32);
    assert_eq!(outcome.stdout["schema"], migration::NATIVE_SCHEMA_VERSION);
    let approved: Value = serde_json::from_slice(&fs::read(&plan).unwrap()).unwrap();
    assert_eq!(
        outcome.stdout["plan_fingerprint"],
        approved["plan_fingerprint"]
    );
    let backup = Path::new(outcome.stdout["backup"].as_str().unwrap());
    assert!(backup.starts_with(fs::canonicalize(&backups).unwrap()));
    let receipt: Value =
        serde_json::from_slice(&fs::read(backup.parent().unwrap().join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(receipt["status"], "committed");
    assert_eq!(
        receipt["sha256"],
        catabolic_core::sha256(fs::read(backup).unwrap())
    );

    let inspected = cli(&path, &["db", "native-inspect"]).stdout;
    assert_eq!(inspected["schema"], migration::NATIVE_SCHEMA_VERSION);
    assert_eq!(inspected["safe"], true);
    // The consumed plan described schema 32; it cannot authorize anything now.
    refused(&cli(&path, &args), "plan or inputs changed");
    let fresh = self::plan(root.path(), &path, "fresh.json");
    let mut args = vec!["db", "native-upgrade", "--plan", fresh.to_str().unwrap()];
    args.extend(QUIESCENT);
    let outcome = cli(&path, &args);
    assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    assert_eq!(outcome.stdout["upgraded"], false);
}

#[test]
fn pending_work_and_wal_mode_are_refused_by_the_cli() {
    let (root, path) = catalog();
    let db = connect(&path, true).unwrap();
    db.execute_batch("INSERT INTO locations VALUES('source'); INSERT INTO files VALUES('file','source','file.bin'); INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,state) VALUES('job','default','file','probe','source','{}','{}','key','queued')").unwrap();
    drop(db);
    let pending = plan(root.path(), &path, "pending.json");
    let mut args = vec!["db", "native-upgrade", "--plan", pending.to_str().unwrap()];
    args.extend(QUIESCENT);
    refused(&cli(&path, &args), "pending catalog work");
    assert_eq!(migration::inspect(&path, true).unwrap()["schema"], 32);

    let (root, path) = catalog();
    let writer = connect(&path, true).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    drop(writer);
    let wal = plan(root.path(), &path, "wal.json");
    let mut args = vec!["db", "native-upgrade", "--plan", wal.to_str().unwrap()];
    args.extend(QUIESCENT);
    refused(&cli(&path, &args), "requires rollback-journal mode");
    assert_eq!(migration::inspect(&path, true).unwrap()["schema"], 32);
}

#[test]
fn identity_manifest_and_diff_commands_report_preservation() {
    let (root, path) = catalog();
    let before = root.path().join("before.json");
    let outcome = cli(
        &path,
        &["db", "identity-manifest", "--out", before.to_str().unwrap()],
    );
    assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    assert_eq!(outcome.stdout["contract"], "catabolic-identity-manifest-v1");
    let written: Value = serde_json::from_slice(&fs::read(&before).unwrap()).unwrap();
    assert_eq!(written["fingerprint"], outcome.stdout["fingerprint"]);
    refused(
        &cli(
            &path,
            &["db", "identity-manifest", "--out", before.to_str().unwrap()],
        ),
        "cannot create",
    );
    refused(
        &cli(
            &path,
            &["db", "identity-diff", "--before", before.to_str().unwrap()],
        ),
        "requires --before MANIFEST.json and --after MANIFEST.json",
    );
    refused(
        &cli(
            &path,
            &["db", "native-inspect", "--before", before.to_str().unwrap()],
        ),
        "only accepted by db identity-diff",
    );

    let plan = plan(root.path(), &path, "plan.json");
    let mut args = vec!["db", "native-upgrade", "--plan", plan.to_str().unwrap()];
    args.extend(QUIESCENT);
    assert_eq!(cli(&path, &args).code, Some(0));
    let after = root.path().join("after.json");
    let outcome = cli(
        &path,
        &["db", "identity-manifest", "--out", after.to_str().unwrap()],
    );
    assert_eq!(outcome.stdout["schema"], migration::NATIVE_SCHEMA_VERSION);
    let preserved = cli(
        &path,
        &[
            "db",
            "identity-diff",
            "--before",
            before.to_str().unwrap(),
            "--after",
            after.to_str().unwrap(),
        ],
    );
    assert_eq!(preserved.code, Some(0), "{}", preserved.stderr);
    assert_eq!(preserved.stdout["preserved"], true);
    // Reversing the comparison is a downgrade and must be refused explicitly.
    refused(
        &cli(
            &path,
            &[
                "db",
                "identity-diff",
                "--before",
                after.to_str().unwrap(),
                "--after",
                before.to_str().unwrap(),
            ],
        ),
        "identity preservation failed",
    );
    // Incomplete manifests are refused rather than compared as preserved.
    let bare_before = root.path().join("bare-before.json");
    let bare_after = root.path().join("bare-after.json");
    fs::write(
        &bare_before,
        br#"{"contract":"catabolic-identity-manifest-v1","schema":32}"#,
    )
    .unwrap();
    fs::write(
        &bare_after,
        format!(
            r#"{{"contract":"catabolic-identity-manifest-v1","schema":{}}}"#,
            migration::NATIVE_SCHEMA_VERSION
        ),
    )
    .unwrap();
    refused(
        &cli(
            &path,
            &[
                "db",
                "identity-diff",
                "--before",
                bare_before.to_str().unwrap(),
                "--after",
                bare_after.to_str().unwrap(),
            ],
        ),
        "invalid_manifest",
    );
}

#[test]
fn convert_journal_command_unblocks_a_wal_catalog_for_a_fresh_plan() {
    let (root, path) = catalog();
    let writer = connect(&path, true).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    drop(writer);
    refused(
        &cli(&path, &["db", "convert-journal"]),
        "requires --to delete",
    );
    refused(
        &cli(&path, &["db", "convert-journal", "--to", "wal"]),
        "unsupported journal conversion target",
    );
    refused(
        &cli(&path, &["db", "native-inspect", "--to", "delete"]),
        "--to is only accepted by db convert-journal",
    );
    assert_eq!(
        cli(&path, &["db", "native-inspect"]).stdout["journal_mode"],
        "wal"
    );
    let converted = cli(&path, &["db", "convert-journal", "--to", "delete"]);
    assert_eq!(converted.code, Some(0), "{}", converted.stderr);
    assert_eq!(converted.stdout["converted"], true);
    assert_eq!(converted.stdout["from_journal_mode"], "wal");
    assert_eq!(converted.stdout["journal_mode"], "delete");
    let again = cli(&path, &["db", "convert-journal", "--to", "delete"]);
    assert_eq!(again.stdout["converted"], false);
    let plan = plan(root.path(), &path, "plan.json");
    let mut args = vec!["db", "native-upgrade", "--plan", plan.to_str().unwrap()];
    args.extend(QUIESCENT);
    let outcome = cli(&path, &args);
    assert_eq!(outcome.code, Some(0), "{}", outcome.stderr);
    assert_eq!(outcome.stdout["schema"], migration::NATIVE_SCHEMA_VERSION);
}

#[test]
fn dry_run_is_refused_by_commands_that_do_not_implement_it() {
    let (root, path) = catalog();
    let plan = plan(root.path(), &path, "plan.json");
    let writer = connect(&path, true).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    drop(writer);
    let before = fs::read(&path).unwrap();
    let backups = root.path().join("backups");
    let mut upgrade = vec![
        "db",
        "native-upgrade",
        "--dry-run",
        "--plan",
        plan.to_str().unwrap(),
        "--backup-dir",
        backups.to_str().unwrap(),
    ];
    upgrade.extend(QUIESCENT);
    let manifest = root.path().join("manifest.json");
    let out = root.path().join("dry-plan.json");
    for args in [
        upgrade,
        vec!["db", "convert-journal", "--to", "delete", "--dry-run"],
        vec![
            "--dry-run",
            "db",
            "native-plan",
            "--out",
            out.to_str().unwrap(),
        ],
        vec![
            "db",
            "identity-manifest",
            "--out",
            manifest.to_str().unwrap(),
            "--dry-run",
        ],
        vec!["db", "native-inspect", "--dry-run"],
        vec!["init", "--dry-run"],
    ] {
        refused(
            &cli(&path, &args),
            "--dry-run is only implemented by db upgrade",
        );
    }
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        cli(&path, &["db", "native-inspect"]).stdout["journal_mode"],
        "wal"
    );
    assert!(!backups.exists());
    assert!(!out.exists());
    assert!(!manifest.exists());
    // The implemented legacy dry run remains a read-only preview.
    let preview = cli(&path, &["db", "upgrade", "--dry-run"]);
    assert_eq!(preview.code, Some(0), "{}", preview.stderr);
    assert_eq!(preview.stdout["dry_run"], true);
    assert_eq!(fs::read(&path).unwrap(), before);
}
