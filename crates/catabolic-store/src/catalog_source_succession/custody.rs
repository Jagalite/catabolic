//! Retained old-and-candidate byte equivalence for planned-copy succession.
//! No caller hash is accepted. This is not a command or Motion admission grant.
//! The old root must remain present; already-moved/offline relocation is separate.
use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

// This checks enrollment, not filesystem identity. The retained transaction and
// the surrounding structural/continuity checks own the exact binding row.
// An absent row is the existing `not_enrolled` policy state, not UUID bypass.
fn admit_uuid_policy_in(
    db: &rusqlite::Connection,
    profile: &str,
    source: &str,
    policy: &Value,
    budget: &mut impl StructuralReadBudget,
) -> Result<Option<uuid::Uuid>> {
    let mode = policy["settings"]["uuid"]
        .as_str()
        .filter(|mode| matches!(*mode, "check" | "skip"))
        .ok_or_else(|| refuse("invalid source UUID policy"))?;
    budget.charge(BudgetCharge {
        work: 1,
        values: 1,
        ..Default::default()
    })?;
    let mut statement = db.prepare(
        "SELECT volume_uuid FROM main.binding_volumes WHERE profile=? AND kind='source' AND owner=?",
    )?;
    let mut rows = statement.query([profile, source])?;
    let enrolled = if let Some(row) = rows.next()? {
        let rusqlite::types::ValueRef::Text(raw) = row.get_ref(0)? else {
            return Err(refuse("malformed source UUID enrollment"));
        };
        budget.charge(BudgetCharge {
            bytes: raw.len(),
            work: 1 + raw.len().div_ceil(64),
            ..Default::default()
        })?;
        if raw.is_empty() || std::str::from_utf8(raw).is_err() {
            return Err(refuse("malformed source UUID enrollment"));
        }
        if mode == "check" {
            #[cfg(target_os = "linux")]
            {
                Some(crate::source::relocation_uuid::canonical_enrollment(
                    std::str::from_utf8(raw).expect("checked UTF-8"),
                )?)
            }
            #[cfg(not(target_os = "linux"))]
            {
                return Err(refuse(
                    "required UUID needs bounded relocation probe owner on this platform",
                ));
            }
        } else {
            None
        }
    } else {
        None
    };
    Ok(enrolled)
}

struct HeldFile {
    file: File,
    snapshot: Value,
}
struct EqualFile {
    old: HeldFile,
    candidate: HeldFile,
    digest: String,
}
/// Nonserde, nonclone, owned descriptors. Binding this to a command requires the
/// separate immutable Motion admission and identity-owned composite.
pub(crate) struct VerifiedContent {
    enrolled_uuid: Option<uuid::Uuid>,
    old_root: File,
    old_root_path: std::path::PathBuf,
    candidate_root_path: std::path::PathBuf,
    candidate_root: File,
    files: BTreeMap<String, EqualFile>,
    commitment: String,
}
fn check(store: &Store, cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        return Err(refuse("content verification cancelled"));
    }
    store
        .catalog_operation_budget()?
        .charge(BudgetCharge {
            work: 1,
            ..Default::default()
        })
        .map_err(|e| refuse(&format!("content verification budget: {e:?}")))
}
fn stat(file: &File) -> Result<Value> {
    let m = file.metadata()?;
    Ok(
        json!({"size":m.len(),"mtime_ns":i128::from(m.mtime())*1_000_000_000+i128::from(m.mtime_nsec()),"ctime_ns":i128::from(m.ctime())*1_000_000_000+i128::from(m.ctime_nsec()),"device":m.dev(),"inode":m.ino()}),
    )
}
// Physical comparison is independent of file count and UUID enrollment. The
// caller supplies the exact binding read under the retained source writer and
// admits these two fixed fields before this read. This is not a new binding.
fn validate_old_root_binding(root: &File, binding: &Value) -> Result<()> {
    let metadata = root.metadata()?;
    if !metadata.is_dir()
        || binding["device"].as_u64() != Some(metadata.dev())
        || binding["inode"].as_u64() != Some(metadata.ino())
    {
        return Err(refuse("old root differs from retained source binding"));
    }
    Ok(())
}
fn retain_and_hash(
    store: &Store,
    snapshot: Value,
    cancel: &AtomicBool,
    buffer: &mut [u8],
) -> Result<(HeldFile, String)> {
    check(store, cancel)?;
    // Reuse exact native O_NOFOLLOW traversal and before/after named/held checks.
    let (file, digest) = crate::source::without_children(|| {
        crate::source::with_validated_source_relocation(
            &snapshot,
            &store.catalog_operation_budget()?,
            cancel,
            |file| {
                let mut digest = Sha256::new();
                let mut bytes = 0_u64;
                loop {
                    check(store, cancel)?;
                    let n = file.read(buffer)?;
                    if n == 0 {
                        break;
                    }
                    bytes = bytes
                        .checked_add(n as u64)
                        .ok_or_else(|| refuse("content length overflow"))?;
                    if Some(bytes) > snapshot["size"].as_u64() {
                        return Err(refuse("content grew during hash"));
                    }
                    digest.update(&buffer[..n]);
                }
                if Some(bytes) != snapshot["size"].as_u64() {
                    return Err(refuse("content length changed"));
                }
                file.seek(SeekFrom::Start(0))?;
                Ok((file.try_clone()?, format!("{:x}", digest.finalize())))
            },
        )
    })?;
    Ok((HeldFile { file, snapshot }, digest))
}
impl HeldFile {
    fn revalidate(&self, store: &Store, cancel: &AtomicBool) -> Result<()> {
        check(store, cancel)?;
        let actual = stat(&self.file)?;
        if ["size", "mtime_ns", "ctime_ns", "device", "inode"]
            .iter()
            .any(|k| actual[k] != self.snapshot[k])
        {
            return Err(refuse("retained content descriptor changed"));
        }
        crate::source::without_children(|| {
            crate::source::with_validated_source_relocation(
                &self.snapshot,
                &store.catalog_operation_budget()?,
                cancel,
                |_| Ok(()),
            )
        })?;
        check(store, cancel)
    }
}
impl VerifiedContent {
    pub(super) fn root_binding(&self, old: &Value) -> Result<Value> {
        let mut next = old.clone();
        let stat = self.candidate_root.metadata()?;
        next["root"] = json!(
            self.candidate_root_path
                .to_str()
                .ok_or_else(|| refuse("candidate root encoding"))?
        );
        next["device"] = json!(stat.dev());
        next["inode"] = json!(stat.ino());
        Ok(next)
    }
    pub(super) fn complete_files(&self) -> impl Iterator<Item = (&str, &Value, &Value, &str)> {
        self.files.iter().map(|(id, f)| {
            (
                id.as_str(),
                &f.old.snapshot,
                &f.candidate.snapshot,
                f.digest.as_str(),
            )
        })
    }
    pub(crate) fn replacement(&self, file: &str) -> Option<(&Value, &Value)> {
        self.files
            .get(file)
            .map(|f| (&f.old.snapshot, &f.candidate.snapshot))
    }

    pub(crate) fn revisions(&self) -> BTreeMap<String, (String, String)> {
        self.files
            .iter()
            .map(|(id, f)| {
                (
                    id.clone(),
                    (
                        crate::components::revision(&f.old.snapshot),
                        crate::components::revision(&f.candidate.snapshot),
                    ),
                )
            })
            .collect()
    }

    pub(super) fn commitment(&self) -> &str {
        &self.commitment
    }
    pub(super) fn revalidate(&self, store: &Store, cancel: &AtomicBool) -> Result<()> {
        check(store, cancel)?;
        for (held, path) in [
            (&self.old_root, &self.old_root_path),
            (&self.candidate_root, &self.candidate_root_path),
        ] {
            if path.canonicalize()? != *path {
                return Err(refuse("content root locator changed"));
            }
            let named = crate::source::open_root(
                path.to_str()
                    .ok_or_else(|| refuse("content root is not UTF-8"))?,
            )?;
            if held.metadata()?.dev() != named.metadata()?.dev()
                || held.metadata()?.ino() != named.metadata()?.ino()
            {
                return Err(refuse("content root replaced"));
            }
            if let Some(expected) = self.enrolled_uuid {
                let budget = store.catalog_operation_budget()?;
                crate::source::relocation_uuid::validate(held, expected, &budget, cancel)?;
                crate::source::relocation_uuid::validate(&named, expected, &budget, cancel)?;
            }
        }
        for file in self.files.values() {
            file.old.revalidate(store, cancel)?;
            file.candidate.revalidate(store, cancel)?;
            for (root, snapshot) in [
                (&self.old_root, &file.old.snapshot),
                (&self.candidate_root, &file.candidate.snapshot),
            ] {
                let m = root.metadata()?;
                if Some(m.dev()) != snapshot["root_device"].as_u64()
                    || Some(m.ino()) != snapshot["root_inode"].as_u64()
                {
                    return Err(refuse("retained content root changed"));
                }
            }
        }
        Ok(())
    }
}
/// Called only under the retained writer with a freshly complete native capture.
/// Unavailable members refuse. Linux enrolled UUIDs require exact descriptor
/// probes of the unchanged volume identity; other platforms remain refused.
/// No file is revived by pathname and no saved policy is weakened.
pub(super) fn verify_in(
    store: &Store,
    captured: &CapturedSuccession,
    candidate_root: &Path,
    cancel: &AtomicBool,
    budget: &mut impl StructuralReadBudget,
) -> Result<VerifiedContent> {
    #[cfg(test)]
    super::history::trace_history("custody_before_full_recapture");
    captured.revalidate_in(store, budget)?;
    #[cfg(test)]
    super::history::trace_history("custody_after_full_recapture");
    #[cfg(test)]
    super::history::trace_history("custody_before_descriptor_hashing");
    let verified = verify_content_in(store, captured, candidate_root, cancel, budget)?;
    #[cfg(test)]
    super::history::trace_history("custody_before_full_recapture");
    captured.revalidate_in(store, budget)?;
    #[cfg(test)]
    super::history::trace_history("custody_after_full_recapture");
    #[cfg(test)]
    super::history::trace_history("custody_verify_complete");
    Ok(verified)
}
pub(super) fn verify_pass_in(
    store: &Store,
    command: &catabolic_core::catalog_command::CommandEnvelope,
    pass: &PreEffectSourcePass<'_, '_>,
    candidate_root: &Path,
    cancel: &AtomicBool,
    budget: &mut crate::catalog_nfo_attribution::ProofBudget,
) -> Result<VerifiedContent> {
    let captured = pass.check(store, command, budget)?;
    let verified = verify_content_in(store, captured, candidate_root, cancel, budget)?;
    pass.check(store, command, budget)?;
    Ok(verified)
}
// Only a fixed admitted preparation borrows this pass. Generic verify_in above
// deliberately retains full recaptures and does not acquire reusable authority.
pub(super) fn verify_preparation_in(
    store: &Store,
    pass: &SourcePreparationPass<'_, '_>,
    profile: &str,
    source: &str,
    candidate_root: &Path,
    cancel: &AtomicBool,
    budget: &mut crate::catalog_nfo_attribution::ProofBudget,
) -> Result<VerifiedContent> {
    let captured = pass.check(store, profile, source, candidate_root, budget)?;
    let verified = verify_content_in(store, captured, candidate_root, cancel, budget)?;
    pass.check(store, profile, source, candidate_root, budget)?;
    Ok(verified)
}
fn verify_content_in(
    store: &Store,
    captured: &CapturedSuccession,
    candidate_root: &Path,
    cancel: &AtomicBool,
    budget: &mut impl StructuralReadBudget,
) -> Result<VerifiedContent> {
    check(store, cancel)?;
    if !candidate_root.is_absolute() {
        return Err(refuse("candidate root must be absolute"));
    }
    let candidate_name = candidate_root
        .to_str()
        .ok_or_else(|| refuse("candidate root must be UTF-8"))?;
    let binding =
        crate::observation::binding(store, &captured.profile, "source", &captured.source)?;
    let old_name = binding["root"]
        .as_str()
        .ok_or_else(|| refuse("source root missing"))?;
    let policy = crate::observation::trust_policy(store, &captured.profile, &captured.source)?;
    let enrolled_uuid = admit_uuid_policy_in(
        &store.db,
        &captured.profile,
        &captured.source,
        &policy,
        budget,
    )?;
    // Allocate once, not once per file or proportional to file size. Existing
    // cumulative work/deadline and cancellation checks apply to every read.
    budget.charge(BudgetCharge {
        bytes: 65536,
        items: captured.files.len(),
        ..Default::default()
    })?;
    let mut buffer = vec![0_u8; 65536];
    if candidate_root.canonicalize()? != candidate_root
        || Path::new(old_name).canonicalize()? != Path::new(old_name)
    {
        return Err(refuse("content roots require canonical locators"));
    }
    let old_root = crate::source::open_root(old_name)?;
    budget.charge(BudgetCharge {
        values: 2,
        bytes: 16,
        work: 1,
        ..Default::default()
    })?;
    validate_old_root_binding(&old_root, &binding)?;
    let next_root = crate::source::open_root(candidate_name)?;
    if let Some(expected) = enrolled_uuid {
        let operation = store.catalog_operation_budget()?;
        crate::source::relocation_uuid::validate(&old_root, expected, &operation, cancel)?;
        crate::source::relocation_uuid::validate(&next_root, expected, &operation, cancel)?;
    }
    let next_root_stat = next_root.metadata()?;
    if !next_root_stat.is_dir() {
        return Err(refuse("candidate root is not a directory"));
    }
    let mut files = BTreeMap::new();
    for (id, path) in &captured.files {
        check(store, cancel)?;
        crate::layout::relative_path(path)?;
        let mut original = crate::components::occurrence(&store.db, &captured.profile, id)?;
        let ctime: i64 = store.db.query_row(
            "SELECT c.ctime_ns FROM native_source_ctimes c JOIN observations o ON o.profile=c.profile AND o.file_id=c.file_id AND o.size=c.size AND o.mtime_ns=c.mtime_ns AND o.device=c.device AND o.inode=c.inode WHERE c.profile=? AND c.file_id=? AND o.status='present'",
            [&captured.profile, id],
            |r| r.get(0),
        )?;
        original["ctime_ns"] = json!(ctime);
        #[cfg(test)]
        super::history::trace_history("descriptor_before_occurrence_copy");
        let retained_original = super::allocation::clone_value(&original, budget)?;
        let (old, digest) = retain_and_hash(store, retained_original, cancel, &mut buffer)?;
        let mut next = original;
        let candidate_stat = std::fs::symlink_metadata(candidate_root.join(path))?;
        if !candidate_stat.is_file() {
            return Err(refuse("candidate is not a regular file"));
        }
        next["root"] = json!(candidate_name);
        next["root_device"] = json!(next_root_stat.dev());
        next["root_inode"] = json!(next_root_stat.ino());
        next["device"] = json!(candidate_stat.dev());
        next["inode"] = json!(candidate_stat.ino());
        next["size"] = json!(candidate_stat.len());
        next["mtime_ns"] = json!(
            i128::from(candidate_stat.mtime()) * 1_000_000_000
                + i128::from(candidate_stat.mtime_nsec())
        );
        next["ctime_ns"] = json!(
            i128::from(candidate_stat.ctime()) * 1_000_000_000
                + i128::from(candidate_stat.ctime_nsec())
        );
        let (candidate, next_digest) = retain_and_hash(store, next, cancel, &mut buffer)?;
        if digest != next_digest {
            return Err(refuse("candidate content differs from retained original"));
        }
        // Admit the actual descriptor-map node and copied identifier before
        // allocation. Snapshots remain owned here; no commitment Value clones
        // either one merely to calculate its serialized size.
        // File identifiers are retained native strings, not guaranteed UUIDs.
        // Charge their copy and conservative string comparisons: a BTree node
        // search has bounded fanout; sixteen comparisons per binary depth
        // conservatively covers its fixed-node search and insertion path.
        let depth = (usize::BITS - (files.len() + 1).leading_zeros()) as usize;
        let map_work = id
            .len()
            .checked_add(1)
            .map(|n| n.div_ceil(64))
            .and_then(|n| n.checked_mul(16 * depth))
            .and_then(|n| n.checked_add(id.len().div_ceil(64)))
            .and_then(|n| n.checked_add(1))
            .ok_or_else(|| refuse("descriptor map work overflow"))?;
        budget.charge(BudgetCharge {
            bytes: id
                .len()
                .checked_add(
                    16 * std::mem::size_of::<(String, EqualFile)>()
                        + 8 * std::mem::size_of::<usize>(),
                )
                .ok_or_else(|| refuse("descriptor map allocation overflow"))?,
            values: 1,
            work: map_work,
            ..Default::default()
        })?;
        files.insert(
            id.clone(),
            EqualFile {
                old,
                candidate,
                digest,
            },
        );
    }
    // This commitment describes native observation and independently read bytes,
    // not current Motion actor/lifecycle authority or successful mutation.
    // Both maps have identical keys and binary ordering: this private loop
    // inserted exactly one EqualFile for every captured file, with no removals.
    // Zip borrows the retained snapshots without allocating an aggregate tree.
    let commitment = super::history::content_digest(
        next_root_stat.dev(),
        next_root_stat.ino(),
        candidate_name,
        &captured.catalog_id,
        captured
            .files
            .iter()
            .zip(files.iter())
            .map(|((id, path), (retained_id, file))| {
                debug_assert_eq!(id, retained_id);
                super::history::BorrowedContentFile {
                    id,
                    path,
                    old: &file.old.snapshot,
                    candidate: &file.candidate.snapshot,
                    sha256: &file.digest,
                }
            }),
        captured.generation,
        &captured.incarnation,
        &binding,
        &captured.profile,
        &captured.source,
        budget,
    )?;
    // Independent frozen encoder oracle on the actual descriptor cohort.
    // This extra aggregate exists only in tests, never in the production owner.
    #[cfg(test)]
    {
        let entries: Vec<Value> = captured
            .files
            .iter()
            .zip(files.iter())
            .map(|((id, path), (_, file))| {
                json!({"id": id, "path": path, "old": file.old.snapshot,
                "candidate": file.candidate.snapshot, "sha256": file.digest})
            })
            .collect();
        let old = json!({"catalog":captured.catalog_id,"incarnation":captured.incarnation,
            "generation":captured.generation,"profile":captured.profile,"source":captured.source,
            "old_binding":binding,"candidate_root":candidate_name,
            "candidate_device":next_root_stat.dev(),"candidate_inode":next_root_stat.ino(),
            "files":entries});
        assert_eq!(
            commitment,
            catabolic_core::sha256(catabolic_core::encode(&old))
        );
    }
    budget.charge(BudgetCharge {
        bytes: old_name
            .len()
            .checked_add(candidate_name.len())
            .ok_or_else(|| refuse("retained root path allocation overflow"))?,
        values: 2,
        work: 1 + old_name.len().div_ceil(64) + candidate_name.len().div_ceil(64),
        ..Default::default()
    })?;
    let verified = VerifiedContent {
        enrolled_uuid,
        old_root,
        old_root_path: old_name.into(),
        candidate_root_path: candidate_root.into(),
        candidate_root: next_root,
        files,
        commitment,
    };
    verified.revalidate(store, cancel)?;
    Ok(verified)
}

#[cfg(test)]
mod root_tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    #[test]
    fn uuid_policy_distinguishes_absence_enrollment_and_malformed_storage_without_children() {
        struct Meter;
        impl StructuralReadBudget for Meter {
            fn charge(&mut self, _: BudgetCharge) -> Result<()> {
                Ok(())
            }
            fn raw_json(&mut self, _: &str) -> Result<()> {
                Err(refuse("UUID admission does not parse JSON"))
            }
        }
        // Deliberately weak table solely tests malformed storage, never mints
        // native catalog admission or filesystem authority.
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE binding_volumes(profile TEXT,kind TEXT,owner TEXT,volume_uuid);",
        )
        .unwrap();
        let check = json!({"settings":{"uuid":"check"}});
        let skip = json!({"settings":{"uuid":"skip"}});
        crate::source::without_children(|| {
            admit_uuid_policy_in(&db, "p", "s", &check, &mut Meter)?;
            admit_uuid_policy_in(&db, "p", "s", &skip, &mut Meter)?;
            db.execute(
                "INSERT INTO binding_volumes VALUES('p','source','s','00010203-0405-0607-0809-0a0b0c0d0e0f')",
                [],
            )?;
            #[cfg(target_os = "linux")]
            assert!(admit_uuid_policy_in(&db, "p", "s", &check, &mut Meter)?.is_some());
            #[cfg(not(target_os = "linux"))]
            assert!(admit_uuid_policy_in(&db, "p", "s", &check, &mut Meter).is_err());
            admit_uuid_policy_in(&db, "p", "s", &skip, &mut Meter)?;
            for expression in ["NULL", "123", "x'6162'", "''", "CAST(x'ff' AS TEXT)"] {
                db.execute(
                    &format!("UPDATE binding_volumes SET volume_uuid={expression}"),
                    [],
                )?;
                assert!(
                    admit_uuid_policy_in(&db, "p", "s", &check, &mut Meter)
                        .unwrap_err()
                        .0
                        .contains("malformed")
                );
            }
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn empty_root_binding_rejects_replacement_before_uuid_or_file_validation() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("empty");
        std::fs::create_dir(&path).unwrap();
        let held = crate::source::open_root(path.to_str().unwrap()).unwrap();
        let metadata = held.metadata().unwrap();
        let binding = json!({"device":metadata.dev(),"inode":metadata.ino()});
        validate_old_root_binding(&held, &binding).unwrap();
        std::fs::rename(&path, parent.path().join("retained-old")).unwrap();
        std::fs::create_dir(&path).unwrap();
        let replacement = crate::source::open_root(path.to_str().unwrap()).unwrap();
        assert!(
            validate_old_root_binding(&replacement, &binding)
                .unwrap_err()
                .0
                .contains("retained source binding")
        );
        // The retained descriptor still names the original empty directory;
        // the ordinary terminal named-vs-held check separately rejects reuse.
        validate_old_root_binding(&held, &binding).unwrap();
        assert!(
            validate_old_root_binding(&held, &json!({"device":null,"inode":metadata.ino()}))
                .is_err()
        );
    }
    #[test]
    fn retained_root_walk_refuses_replacement_symlink_and_fifo() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let held = crate::source::open_root(root.to_str().unwrap()).unwrap();
        let old = parent.path().join("old");
        std::fs::rename(&root, &old).unwrap();
        std::os::unix::fs::symlink(&old, &root).unwrap();
        assert!(crate::source::open_root(root.to_str().unwrap()).is_err());
        std::fs::remove_file(&root).unwrap();
        let name = std::ffi::CString::new(root.as_os_str().as_bytes()).unwrap();
        // A FIFO at the former root must be refused without a blocking open.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(crate::source::open_root(root.to_str().unwrap()).is_err());
        assert!(held.metadata().unwrap().is_dir());
        std::fs::remove_file(&root).unwrap();
        std::fs::create_dir(&root).unwrap();
        let replacement = crate::source::open_root(root.to_str().unwrap()).unwrap();
        assert_ne!(
            held.metadata().unwrap().ino(),
            replacement.metadata().unwrap().ino()
        );
    }
}
