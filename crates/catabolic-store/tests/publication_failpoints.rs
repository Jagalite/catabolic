// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Durable-boundary crash matrix for symlink and hardlink publication (track C1).
//!
//! Each case builds a disposable catalog whose next apply has exactly one
//! journal operation, then applies it in a child process that exits with
//! status 86 at one failpoint. Exiting skips destructors, rollbacks and syncs,
//! as a real crash would. The parent then runs ordinary recovery and a fresh
//! apply, and checks the invariants listed on [`converged`].
//!
//! Destructive kinds run under the debug-only `disposable_fixture` exception,
//! so this file is compiled only with debug assertions. Release gates are not
//! exercised or relaxed here.
#![cfg(debug_assertions)]
use catabolic_store::{Store, filesystem_mutation, migration, observation};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

const MARKER: &str = ".catabolic-owner.json";
const RETAINED: &str = ".catabolic-retained";
const LEAF: &str = "folder/one.txt";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Prepare,
    Create,
    Replace,
    Remove,
    Forget,
}

#[derive(Clone, Copy, Debug)]
struct Case {
    hardlink: bool,
    kind: Kind,
    stage: &'static str,
}

/// Crash points each one-operation plan reaches, in execution order.
/// Symlink replacement uses atomic exchange where the filesystem supports
/// it, and otherwise quarantines the previous link before publishing.
fn stages(hardlink: bool, kind: Kind, exchange: bool) -> &'static [&'static str] {
    use Kind::*;
    match (hardlink, kind) {
        (_, Prepare) => &[
            "after_journal",
            "after_marker_create",
            "after_marker_write",
            "after_filesystem",
            "before_commit",
            "after_commit",
        ],
        (false, Create | Forget) | (true, Forget) => &[
            "after_journal",
            "after_filesystem",
            "before_commit",
            "after_commit",
        ],
        (false, Remove) => &[
            "after_journal",
            "after_quarantine",
            "after_filesystem",
            "before_commit",
            "after_commit",
        ],
        (false, Replace) if exchange => &[
            "after_journal",
            "after_temporary",
            "after_exchange",
            "after_filesystem",
            "before_commit",
            "after_commit",
        ],
        (false, Replace) => &[
            "after_journal",
            "after_temporary",
            "after_quarantine",
            "after_publish",
            "after_filesystem",
            "before_commit",
            "after_commit",
        ],
        (true, Create) => &[
            "after_journal",
            "before_link",
            "after_staged_link",
            "after_link",
            "after_filesystem",
            "before_commit",
            "after_commit",
        ],
        (true, Replace) => &[
            "after_journal",
            "after_retention_marker_create",
            "after_retention_marker_write",
            "before_retention_rename",
            "after_retention",
            "before_link",
            "after_staged_link",
            "after_link",
            "after_filesystem",
            "before_commit",
            "after_commit",
        ],
        (true, Remove) => &[
            "after_journal",
            "after_retention_marker_create",
            "after_retention_marker_write",
            "before_retention_rename",
            "after_retention",
            "after_filesystem",
            "before_commit",
            "after_commit",
        ],
    }
}

fn matrix(exchange: bool) -> Vec<Case> {
    use Kind::*;
    let mut cases = Vec::new();
    for hardlink in [false, true] {
        for kind in [Prepare, Create, Replace, Remove, Forget] {
            for stage in stages(hardlink, kind, exchange) {
                cases.push(Case {
                    hardlink,
                    kind,
                    stage,
                });
            }
        }
    }
    cases
}

struct Fixture {
    _root: tempfile::TempDir,
    database: PathBuf,
    source: PathBuf,
    output: PathBuf,
}

fn options(extra: Value) -> Value {
    let mut value = json!({"catalog":"global","disposable_fixture":true});
    for (key, item) in extra.as_object().unwrap() {
        value[key] = item.clone();
    }
    value
}

fn execute(store: &Store, action: &str, extra: Value) -> catabolic_store::Result<Value> {
    filesystem_mutation::execute(store, "default", action, &options(extra))
}

fn sql(store: &Store, statement: &str) {
    store
        .transaction(|db| {
            db.execute(statement, [])?;
            Ok(())
        })
        .unwrap();
}

fn count(store: &Store, query: &str) -> i64 {
    store.db.query_row(query, [], |r| r.get(0)).unwrap()
}

fn generation(store: &Store) -> i64 {
    count(
        store,
        "SELECT coalesce(max(generation),0) FROM publication_generations WHERE profile='default' AND catalog='global'",
    )
}

/// A catalog whose next apply plans exactly one operation of `kind`.
fn fixture(base: &Path, hardlink: bool, kind: Kind) -> Fixture {
    let root = tempfile::tempdir_in(base).unwrap();
    let database = root.path().join("catalog.sqlite3");
    let source = root.path().join("source");
    let output = root.path().join("output");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&output).unwrap();
    std::fs::write(source.join("one.txt"), "original source").unwrap();
    std::fs::write(source.join("two.txt"), "next source").unwrap();
    migration::initialize(&database).unwrap();
    let store = Store::open(&database, true, false).unwrap();
    observation::execute(
        &store,
        "default",
        "bind",
        &json!({"source":"media","root":source}),
    )
    .unwrap();
    observation::execute(
        &store,
        "default",
        "bind",
        &json!({"kind":"output","owner":"global","root":output}),
    )
    .unwrap();
    observation::execute(&store, "default", "scan", &json!({"source":"media"})).unwrap();
    sql(
        &store,
        "INSERT INTO items(id,kind,metadata) VALUES('item','movie','{}')",
    );
    sql(
        &store,
        "INSERT INTO mappings(id,catalog,file_id,item_id,path) SELECT 'mapping','global',id,'item','folder/one.txt' FROM files WHERE path='one.txt'",
    );
    if hardlink {
        sql(
            &store,
            "INSERT INTO catalog_link_modes VALUES('global','hardlink')",
        );
    }
    let apply = |store: &Store| {
        let report = execute(store, "apply", json!({})).unwrap();
        assert_eq!(report["safe"], true, "{report}");
    };
    match kind {
        Kind::Prepare => sql(&store, "UPDATE mappings SET active=0"),
        Kind::Create => {
            sql(&store, "UPDATE mappings SET active=0");
            apply(&store);
            sql(&store, "UPDATE mappings SET active=1");
        }
        Kind::Replace => {
            apply(&store);
            sql(
                &store,
                "UPDATE mappings SET file_id=(SELECT id FROM files WHERE path='two.txt')",
            );
        }
        Kind::Remove => {
            apply(&store);
            sql(&store, "UPDATE mappings SET active=0");
        }
        Kind::Forget => {
            apply(&store);
            std::fs::remove_file(output.join(LEAF)).unwrap();
            sql(&store, "UPDATE mappings SET active=0");
        }
    }
    let plan = execute(&store, "preview", json!({})).unwrap();
    let kinds: Vec<_> = plan["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].as_str().unwrap().to_owned())
        .filter(|k| !matches!(k.as_str(), "unchanged" | "blocked_source"))
        .collect();
    let expected = match (hardlink, kind) {
        (_, Kind::Prepare) => "prepare".to_owned(),
        (false, kind) => format!("{kind:?}").to_lowercase(),
        (true, kind) => format!("hard_{}", format!("{kind:?}").to_lowercase()),
    };
    assert_eq!(kinds, [expected], "{plan}");
    assert_eq!(plan["safe"], true, "{plan}");
    Fixture {
        _root: root,
        database,
        source,
        output,
    }
}

// The parent launches only this helper in a separate process.
#[test]
fn publication_failpoint_worker() {
    let Ok(database) = std::env::var("CATABOLIC_PUBLICATION_FAILPOINT_DB") else {
        return;
    };
    let stage = std::env::var("CATABOLIC_PUBLICATION_FAILPOINT_STAGE").unwrap();
    let store = Store::open(Path::new(&database), true, false).unwrap();
    let result = execute(&store, "apply", json!({"failpoint":stage,"crash":true}));
    panic!("failpoint {stage} was not reached: {result:?}");
}

/// Copy the running test executable once. Concurrent Cargo invocations can
/// unlink it while the matrix runs.
fn worker_executable(directory: &Path) -> PathBuf {
    let executable = directory.join("publication-failpoint-worker");
    #[cfg(target_os = "linux")]
    let loaded = PathBuf::from("/proc/self/exe");
    #[cfg(not(target_os = "linux"))]
    let loaded = std::env::current_exe().unwrap();
    std::fs::copy(loaded, &executable).unwrap();
    executable
}

fn crash(executable: &Path, fixture: &Fixture, stage: &str) {
    let output = std::process::Command::new(executable)
        .args(["--exact", "publication_failpoint_worker", "--nocapture"])
        .env("CATABOLIC_PUBLICATION_FAILPOINT_DB", &fixture.database)
        .env("CATABOLIC_PUBLICATION_FAILPOINT_STAGE", stage)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(86),
        "stage {stage} not reached\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Every name under the output root, relative and sorted.
fn tree(root: &Path) -> Vec<String> {
    fn walk(base: &Path, directory: &Path, names: &mut Vec<String>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            names.push(
                path.strip_prefix(base)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned(),
            );
            if entry.file_type().unwrap().is_dir() {
                walk(base, &path, names);
            }
        }
    }
    let mut names = Vec::new();
    walk(root, root, &mut names);
    names.sort();
    names
}

/// The crash states that block explicitly instead of converging. A crash
/// between creating an ownership marker and writing it leaves an empty marker.
/// Ownership checks refuse it rather than delete a file they cannot prove is
/// theirs. The documented operator action is to inspect and remove it.
fn blocked_marker(fixture: &Fixture, case: Case) -> Option<PathBuf> {
    match case.stage {
        "after_marker_create" => Some(fixture.output.join(MARKER)),
        "after_retention_marker_create" => Some(fixture.output.join(RETAINED).join(MARKER)),
        _ => None,
    }
}

/// Converged state after recovery and a fresh apply:
/// - the catalog verifies healthy, the journal is empty and the fresh apply
///   has nothing left to do;
/// - source bytes are unchanged;
/// - the published leaf is exactly the desired one, or absent;
/// - retained hardlink data is the previous inode, recorded exactly once;
/// - no private temporary or staged name survives;
/// - the publication generation advanced exactly once for effects that
///   change published names, and not at all otherwise.
fn converged(store: &Store, fixture: &Fixture, case: Case, before: i64, recovered: &Value) {
    let label = format!("{case:?}");
    assert_eq!(
        count(store, "SELECT count(*) FROM journal"),
        0,
        "{label}: {recovered}"
    );
    let fresh = execute(store, "apply", json!({})).unwrap();
    assert_eq!(fresh["safe"], true, "{label}: {fresh}");
    assert_eq!(fresh["applied"], json!([]), "{label}: {fresh}");
    assert_eq!(fresh["healthy"], true, "{label}: {fresh}");
    let verified = execute(store, "verify", json!({})).unwrap();
    assert_eq!(verified["healthy"], true, "{label}: {verified}");
    assert_eq!(
        std::fs::read(fixture.source.join("one.txt")).unwrap(),
        b"original source",
        "{label}"
    );
    assert_eq!(
        std::fs::read(fixture.source.join("two.txt")).unwrap(),
        b"next source",
        "{label}"
    );
    let leaf = fixture.output.join(LEAF);
    let wanted = match case.kind {
        Kind::Create => Some("one.txt"),
        Kind::Replace => Some("two.txt"),
        Kind::Prepare | Kind::Remove | Kind::Forget => None,
    };
    let owned_table = if case.hardlink {
        "owned_hardlinks"
    } else {
        "owned_links"
    };
    let owned = count(store, &format!("SELECT count(*) FROM {owned_table}"));
    match wanted {
        Some(name) => {
            let metadata = std::fs::symlink_metadata(&leaf).unwrap();
            if case.hardlink {
                assert!(metadata.file_type().is_file(), "{label}");
                let source = std::fs::metadata(fixture.source.join(name)).unwrap();
                assert_eq!(
                    (metadata.dev(), metadata.ino()),
                    (source.dev(), source.ino()),
                    "{label}"
                );
            } else {
                assert!(metadata.file_type().is_symlink(), "{label}");
                assert_eq!(
                    std::fs::read_link(&leaf).unwrap(),
                    Path::new("../../source").join(name),
                    "{label}"
                );
            }
            assert_eq!(owned, 1, "{label}");
        }
        None => {
            assert!(std::fs::symlink_metadata(&leaf).is_err(), "{label}");
            assert_eq!(owned, 0, "{label}");
        }
    }
    let retiring = case.hardlink && matches!(case.kind, Kind::Replace | Kind::Remove);
    let retained = count(store, "SELECT count(*) FROM retained_hardlinks");
    let mut allowed = BTreeSet::from([MARKER.to_owned()]);
    if wanted.is_some() || matches!(case.kind, Kind::Remove | Kind::Forget) {
        allowed.insert("folder".to_owned());
    }
    if wanted.is_some() {
        allowed.insert(LEAF.to_owned());
    }
    if retiring {
        assert_eq!(retained, 1, "{label}");
        let id: String = store
            .db
            .query_row("SELECT id FROM retained_hardlinks", [], |r| r.get(0))
            .unwrap();
        let data = fixture.output.join(RETAINED).join(&id).join("data");
        assert_eq!(std::fs::read(&data).unwrap(), b"original source", "{label}");
        let original = std::fs::metadata(fixture.source.join("one.txt")).unwrap();
        assert_eq!(
            std::fs::metadata(&data).unwrap().ino(),
            original.ino(),
            "{label}"
        );
        for name in [
            RETAINED.to_owned(),
            format!("{RETAINED}/{MARKER}"),
            format!("{RETAINED}/{id}"),
            format!("{RETAINED}/{id}/data"),
        ] {
            allowed.insert(name);
        }
    } else {
        assert_eq!(retained, 0, "{label}");
    }
    assert_eq!(
        tree(&fixture.output),
        allowed.into_iter().collect::<Vec<_>>(),
        "{label}"
    );
    let advanced = matches!(case.kind, Kind::Create | Kind::Replace | Kind::Remove);
    assert_eq!(
        generation(store),
        before + i64::from(advanced),
        "{label}: generation must advance exactly once"
    );
}

fn run(executable: &Path, base: &Path, case: Case) {
    let fixture = fixture(base, case.hardlink, case.kind);
    let before = {
        let store = Store::open(&fixture.database, true, false).unwrap();
        generation(&store)
    };
    crash(executable, &fixture, case.stage);
    let store = Store::open(&fixture.database, true, false).unwrap();
    let mut recovered = execute(&store, "recover", json!({}));
    if let Some(marker) = blocked_marker(&fixture, case) {
        let error = recovered.unwrap_err();
        assert!(
            error.0.contains("invalid output ownership marker"),
            "{case:?}: {error:?}"
        );
        // Nothing was removed, and the intent is still pending.
        assert_eq!(std::fs::read(&marker).unwrap(), b"", "{case:?}");
        assert_eq!(count(&store, "SELECT count(*) FROM journal"), 1, "{case:?}");
        assert_eq!(
            std::fs::read(fixture.source.join("one.txt")).unwrap(),
            b"original source"
        );
        // The documented operator action, after inspection.
        std::fs::remove_file(&marker).unwrap();
        recovered = execute(&store, "recover", json!({}));
    }
    let recovered = recovered.unwrap_or_else(|e| panic!("{case:?}: recovery failed: {e:?}"));
    converged(&store, &fixture, case, before, &recovered);
}

/// Whether `base` supports atomic exchange of two directory entries.
fn exchange_supported(base: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let probe = tempfile::tempdir_in(base).unwrap();
    std::os::unix::fs::symlink("a", probe.path().join("a")).unwrap();
    std::os::unix::fs::symlink("b", probe.path().join("b")).unwrap();
    let a = std::ffi::CString::new(probe.path().join("a").as_os_str().as_bytes()).unwrap();
    let b = std::ffi::CString::new(probe.path().join("b").as_os_str().as_bytes()).unwrap();
    #[cfg(target_os = "macos")]
    // SAFETY: both paths are live NUL-terminated strings.
    let result = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_SWAP,
        )
    };
    #[cfg(target_os = "linux")]
    // SAFETY: both paths are live NUL-terminated strings.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    result == 0
}

fn run_matrix(base: &Path) -> usize {
    let exchange = exchange_supported(base);
    let directory = tempfile::tempdir_in(base).unwrap();
    let executable = worker_executable(directory.path());
    let cases = matrix(exchange);
    for case in &cases {
        run(&executable, base, *case);
    }
    eprintln!(
        "publication crash matrix ({} mode) under {}: {} cases passed",
        if exchange { "exchange" } else { "quarantine" },
        base.display(),
        cases.len()
    );
    cases.len()
}

#[test]
fn every_publication_crash_point_recovers_or_blocks_explicitly() {
    run_matrix(&std::env::temp_dir());
}

/// The other replacement mode, on a filesystem chosen explicitly. Run with
/// `--ignored` and `CATABOLIC_TEST_EXCHANGE_ROOT` naming a directory whose
/// filesystem uses the other mode from `TMPDIR`'s (HFS+ lacks exchange;
/// APFS has it).
#[test]
#[ignore = "needs CATABOLIC_TEST_EXCHANGE_ROOT on a filesystem of the other replacement mode"]
fn publication_crash_points_in_the_other_replacement_mode() {
    let base = PathBuf::from(
        std::env::var("CATABOLIC_TEST_EXCHANGE_ROOT")
            .expect("CATABOLIC_TEST_EXCHANGE_ROOT is required"),
    );
    assert_ne!(
        exchange_supported(&base),
        exchange_supported(&std::env::temp_dir()),
        "CATABOLIC_TEST_EXCHANGE_ROOT must use the other replacement mode"
    );
    run_matrix(&base);
}

/// Every failpoint literal in the two mutation sources.
fn source_failpoints() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for source in [
        include_str!("../src/filesystem_mutation.rs"),
        include_str!("../src/hardlink_mutation.rs"),
    ] {
        for (index, _) in source.match_indices("failpoint(args,") {
            let rest = source[index + "failpoint(args,".len()..].trim_start();
            let Some(rest) = rest.strip_prefix('"') else {
                continue;
            };
            found.insert(rest[..rest.find('"').unwrap()].to_owned());
        }
    }
    found
}

#[test]
fn every_failpoint_in_the_source_is_in_the_matrix() {
    let declared: BTreeSet<String> = filesystem_mutation::PUBLICATION_FAILPOINTS
        .iter()
        .map(|s| s.to_string())
        .collect();
    let covered: BTreeSet<String> = matrix(true)
        .into_iter()
        .chain(matrix(false))
        .map(|c| c.stage.to_owned())
        .collect();
    let in_source = source_failpoints();
    assert!(in_source.len() >= 17, "{in_source:?}");
    assert_eq!(in_source, declared, "source and declared list differ");
    assert_eq!(covered, declared, "matrix misses a declared failpoint");
}
