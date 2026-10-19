// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Filesystem timestamp granularity and digest verification for revisions.
//!
//! Metadata revisions (size, mtime, ctime, device, inode) cannot detect a
//! same-size rewrite within one timestamp tick. HFS+ records whole seconds,
//! FAT/exFAT are coarser still, and network filesystems report server clocks.
//! On those filesystems a stored artifact digest, not metadata, proves bytes.
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::{
    cell::Cell,
    collections::{HashMap, HashSet, VecDeque},
    fs::File,
    os::unix::fs::{FileExt, MetadataExt},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

/// Conservative tick for coarse local filesystems: FAT records two seconds.
const COARSE_TICK_NS: i128 = 2_000_000_000;
/// Bound the process-local verification cache; clearing only costs a rehash.
const VERIFIED_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Granularity {
    /// Nanosecond timestamps (APFS, XFS, Btrfs, tmpfs, F2FS, ZFS).
    Fine,
    /// Local filesystems with a kernel-maintained ctime but whole-second or
    /// per-inode-dependent precision (HFS+, ext2/3/4).
    CoarseLocal,
    /// Network, layered, FAT-family or unrecognized filesystems. Their
    /// timestamps may come from another clock or be set explicitly (FAT has no
    /// independent ctime), so even settled metadata is never trusted.
    Untrusted,
}

thread_local! {
    static FORCED: Cell<Option<Granularity>> = const { Cell::new(None) };
    static CLOCK_OFFSET_NS: Cell<i128> = const { Cell::new(0) };
    static DIGESTS: Cell<usize> = const { Cell::new(0) };
}

/// Classify the filesystem holding `file`.
pub(crate) fn granularity(file: &File) -> Granularity {
    if let Some(forced) = FORCED.with(Cell::get) {
        return forced;
    }
    classify(file).unwrap_or(Granularity::Untrusted)
}

#[cfg(target_os = "macos")]
fn classify(file: &File) -> Option<Granularity> {
    use std::os::fd::AsRawFd;
    // SAFETY: statfs is plain data; fstatfs fills it for a live descriptor.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: valid descriptor and writable statfs buffer.
    if unsafe { libc::fstatfs(file.as_raw_fd(), &mut stat) } != 0 {
        return None;
    }
    let name = stat
        .f_fstypename
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect::<Vec<_>>();
    Some(match name.as_slice() {
        b"apfs" | b"tmpfs" => Granularity::Fine,
        b"hfs" => Granularity::CoarseLocal,
        _ => Granularity::Untrusted,
    })
}

#[cfg(target_os = "linux")]
fn classify(file: &File) -> Option<Granularity> {
    use std::os::fd::AsRawFd;
    // SAFETY: statfs is plain data; fstatfs fills it for a live descriptor.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: valid descriptor and writable statfs buffer.
    if unsafe { libc::fstatfs(file.as_raw_fd(), &mut stat) } != 0 {
        return None;
    }
    // f_type is signed on some targets; magic numbers are 32-bit patterns.
    Some(match stat.f_type as u32 {
        // XFS, Btrfs, tmpfs, F2FS, ZFS.
        0x5846_5342 | 0x9123_683E | 0x0102_1994 | 0xF2F5_2010 | 0x2FC1_2FC1 => Granularity::Fine,
        // ext2/3/4, HFS+. FAT and exFAT derive ctime from settable mtime.
        0xEF53 | 0x482B => Granularity::CoarseLocal,
        _ => Granularity::Untrusted,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn classify(_file: &File) -> Option<Granularity> {
    None
}

/// Run `operation` with every filesystem classified as fine or coarse-local on
/// this thread. Test-only: real classification is restored afterwards.
#[doc(hidden)]
pub fn with_forced_granularity<T>(coarse: bool, operation: impl FnOnce() -> T) -> T {
    forced(
        if coarse {
            Granularity::CoarseLocal
        } else {
            Granularity::Fine
        },
        operation,
    )
}

fn forced<T>(granularity: Granularity, operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<Granularity>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FORCED.with(|forced| forced.set(self.0));
        }
    }
    let _restore = Restore(FORCED.with(|forced| forced.replace(Some(granularity))));
    operation()
}

/// Number of full digests computed on this thread. Test-only observability.
#[doc(hidden)]
pub fn digests_computed() -> usize {
    DIGESTS.with(Cell::get)
}

fn nanoseconds(seconds: i64, nanos: i64) -> i128 {
    i128::from(seconds) * 1_000_000_000 + i128::from(nanos)
}

fn now() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos() as i128)
        + CLOCK_OFFSET_NS.with(Cell::get)
}

type Key = (u64, u64, u64, i128, i128, String);

fn key(stat: &std::fs::Metadata, expected: &str) -> Key {
    (
        stat.dev(),
        stat.ino(),
        stat.size(),
        nanoseconds(stat.mtime(), stat.mtime_nsec()),
        nanoseconds(stat.ctime(), stat.ctime_nsec()),
        expected.to_owned(),
    )
}

/// Tolerated disagreement between wall-clock and monotonic elapsed time.
const CLOCK_SLACK_NS: i128 = 1_000_000_000;

/// Verified keys with the wall-clock time their verification started, plus
/// the wall and monotonic clocks at the last lookup, used to detect steps.
struct Verified {
    entries: HashMap<Key, i128>,
    wall: i128,
    monotonic: Instant,
}

#[cfg(not(test))]
static VERIFIED: std::sync::Mutex<Option<Verified>> = std::sync::Mutex::new(None);

/// Run `operation` on the verification cache. Unit tests use a per-thread cache
/// so parallel tests with simulated clocks cannot observe each other's steps.
fn with_cache<T>(operation: impl FnOnce(&mut Option<Verified>) -> T) -> T {
    #[cfg(not(test))]
    return operation(
        &mut VERIFIED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    );
    #[cfg(test)]
    {
        thread_local! {
            static VERIFIED: std::cell::RefCell<Option<Verified>> =
                const { std::cell::RefCell::new(None) };
        }
        VERIFIED.with(|verified| operation(&mut verified.borrow_mut()))
    }
}

/// Discard every cached verification if the wall clock stepped relative to
/// monotonic time since the previous lookup. A step backwards could let a
/// later rewrite reuse an earlier ctime tick. Deliberate clock manipulation
/// between two lookups requires privileges outside this threat model.
fn observe_clock(verified: &mut Verified, wall: i128) {
    let elapsed = verified.monotonic.elapsed().as_nanos() as i128;
    if (wall - (verified.wall + elapsed)).abs() > CLOCK_SLACK_NS {
        verified.entries.clear();
    }
    verified.wall = wall;
    verified.monotonic = Instant::now();
}

/// Bound the process-local record of revisions whose streamed bytes did not
/// match their digest. The oldest record is evicted first; an evicted revision
/// is still hashed on every complete response.
const MISMATCHED_LIMIT: usize = 4096;

/// Revisions (exact metadata key and recorded digest) whose bytes were proven
/// not to match while a complete response streamed (work plan A4).
struct Mismatched {
    keys: HashSet<Key>,
    order: VecDeque<Key>,
}

static MISMATCHED: std::sync::Mutex<Option<Mismatched>> = std::sync::Mutex::new(None);

fn with_mismatched<T>(operation: impl FnOnce(&mut Mismatched) -> T) -> T {
    let mut guard = MISMATCHED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    operation(guard.get_or_insert_with(|| Mismatched {
        keys: HashSet::new(),
        order: VecDeque::new(),
    }))
}

/// Quarantine one exact revision of a generated output after a streamed
/// response proved that its bytes differ from the recorded digest. Like the
/// rest of this module it never changes the catalog: the quarantine is
/// process-local, any cached verification of the revision is discarded, and
/// every later use of the same revision is refused on every filesystem class.
/// A changed file has a different key, which the metadata revision refuses.
pub(crate) fn record_mismatch(stat: &std::fs::Metadata, expected: &str) {
    let entry = key(stat, expected);
    with_cache(|verified| {
        if let Some(verified) = verified.as_mut() {
            verified.entries.remove(&entry);
        }
    });
    with_mismatched(|mismatched| {
        if mismatched.keys.insert(entry.clone()) {
            mismatched.order.push_back(entry);
            while mismatched.order.len() > MISMATCHED_LIMIT {
                if let Some(oldest) = mismatched.order.pop_front() {
                    mismatched.keys.remove(&oldest);
                }
            }
        }
    });
}

fn quarantined(stat: &std::fs::Metadata, expected: &str) -> bool {
    let entry = key(stat, expected);
    with_mismatched(|mismatched| mismatched.keys.contains(&entry))
}

/// Verify `file` against its recorded SHA-256 when metadata cannot prove its
/// bytes. Fine-grained filesystems keep the cheap metadata revision, except
/// for a revision already proven wrong by a streamed response.
pub(crate) fn verify_if_coarse(file: &File, expected: &str) -> Result<()> {
    if quarantined(&file.metadata()?, expected) {
        return Err(Error(
            "artifact checksum mismatch was observed while streaming this revision".into(),
        ));
    }
    match granularity(file) {
        Granularity::Fine => Ok(()),
        Granularity::CoarseLocal => verify_digest(file, expected, true),
        Granularity::Untrusted => verify_digest(file, expected, false),
    }
}

/// Full digest verification. On coarse local filesystems a result is cached
/// only once the file's ctime precedes verification by more than one tick.
/// The kernel sets ctime from the local clock on every data or metadata change
/// and it cannot be backdated, so any later rewrite must change ctime and miss
/// the cache. Entries are discarded if the local clock moves backwards.
fn verify_digest(file: &File, expected: &str, cacheable: bool) -> Result<()> {
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(Error("artifact is not a regular file".into()));
    }
    let entry = key(&before, expected);
    let started = now();
    let hit = with_cache(|verified| {
        let Some(verified) = verified.as_mut() else {
            return false;
        };
        observe_clock(verified, started);
        match verified.entries.get(&entry) {
            Some(&at) if cacheable && at <= started => true,
            Some(_) => {
                verified.entries.remove(&entry);
                false
            }
            None => false,
        }
    });
    if hit {
        return Ok(());
    }
    DIGESTS.with(|count| count.set(count.get() + 1));
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut offset = 0;
    while offset < before.len() {
        if crate::embedding::poll_interrupt() {
            return Err(Error("artifact verification cancelled".into()));
        }
        let remaining = (before.len() - offset).min(buffer.len() as u64) as usize;
        let n = file.read_at(&mut buffer[..remaining], offset)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
        offset += n as u64;
    }
    if offset != before.len() || format!("{:x}", digest.finalize()) != expected {
        return Err(Error(
            "artifact checksum changed; metadata revision cannot prove its bytes".into(),
        ));
    }
    if key(&file.metadata()?, expected) != entry {
        return Err(Error(
            "artifact changed while verifying its checksum".into(),
        ));
    }
    if cacheable && entry.4 + COARSE_TICK_NS < started && entry.3 + COARSE_TICK_NS < started {
        with_cache(|verified| {
            let verified = verified.get_or_insert_with(|| Verified {
                entries: HashMap::new(),
                wall: started,
                monotonic: Instant::now(),
            });
            observe_clock(verified, now());
            if verified.entries.len() >= VERIFIED_LIMIT {
                verified.entries.clear();
            }
            verified.entries.insert(entry, started);
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;
    use std::time::Duration;

    fn file(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact.bin");
        std::fs::write(&path, bytes).unwrap();
        (dir, path, format!("{:x}", Sha256::digest(bytes)))
    }

    fn with_clock_offset<T>(offset: Duration, operation: impl FnOnce() -> T) -> T {
        let previous = CLOCK_OFFSET_NS.with(|clock| clock.replace(offset.as_nanos() as i128));
        let result = operation();
        CLOCK_OFFSET_NS.with(|clock| clock.set(previous));
        result
    }

    #[test]
    fn coarse_filesystems_detect_same_size_rewrites_by_digest() {
        let (_dir, path, digest) = file(b"published bytes");
        for granularity in [Granularity::CoarseLocal, Granularity::Untrusted] {
            forced(granularity, || {
                verify_if_coarse(&File::open(&path).unwrap(), &digest).unwrap();
            });
        }
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, b"published byteZ").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        for granularity in [Granularity::CoarseLocal, Granularity::Untrusted] {
            forced(granularity, || {
                let error = verify_if_coarse(&File::open(&path).unwrap(), &digest).unwrap_err();
                assert!(error.0.contains("checksum changed"), "{error:?}");
            });
        }
        // Fine-grained filesystems keep the metadata-only revision contract.
        forced(Granularity::Fine, || {
            verify_if_coarse(&File::open(&path).unwrap(), &digest).unwrap();
        });
    }

    #[test]
    fn only_settled_local_revisions_are_cached() {
        let (_dir, path, digest) = file(b"settled bytes");
        let verify = |granularity| {
            forced(granularity, || {
                verify_if_coarse(&File::open(&path).unwrap(), &digest).unwrap()
            })
        };
        let settled = Duration::from_secs(10);
        let start = digests_computed();
        // A just-changed file may still be rewritten within its tick.
        verify(Granularity::CoarseLocal);
        verify(Granularity::CoarseLocal);
        assert_eq!(digests_computed() - start, 2);
        // Untrusted clocks are never cached, even when apparently settled.
        with_clock_offset(settled, || {
            verify(Granularity::Untrusted);
            verify(Granularity::Untrusted);
        });
        assert_eq!(digests_computed() - start, 4);
        // Once ctime has settled locally, the verified revision is reused...
        with_clock_offset(settled, || {
            verify(Granularity::CoarseLocal);
            verify(Granularity::CoarseLocal);
        });
        assert_eq!(digests_computed() - start, 5);
        // ...until the wall clock steps backwards relative to monotonic time.
        verify(Granularity::CoarseLocal);
        assert_eq!(digests_computed() - start, 6);
        // A forward step also discards earlier verifications.
        with_clock_offset(settled, || verify(Granularity::CoarseLocal));
        with_clock_offset(settled * 3, || verify(Granularity::CoarseLocal));
        assert_eq!(digests_computed() - start, 8);
    }

    #[test]
    fn classification_matches_known_system_filesystems() {
        #[cfg(target_os = "macos")]
        {
            // Supported macOS releases boot from APFS; devfs is not classified.
            assert_eq!(classify(&File::open("/").unwrap()), Some(Granularity::Fine));
            assert_eq!(
                granularity(&File::open("/dev").unwrap()),
                Granularity::Untrusted
            );
        }
        #[cfg(target_os = "linux")]
        assert_eq!(
            granularity(&File::open("/proc").unwrap()),
            Granularity::Untrusted
        );
    }
}
