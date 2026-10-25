//! Relocation-only, no-child volume identity probe. This does not enroll a UUID.
//! ABI/semantics: Linux UAPI fsuuid2/FS_IOC_GETFSUUID and fs/ioctl.c at
//! kernel/common commit 5484a4ea7a1f208b886b58dd55cc55f418930f8a.
//! Only canonical 16-byte same-volume enrollment is supported. Host/kernel and
//! genuine enrolled-volume positives require qualification independently of units.
//! The fixed syscall remains owned until it returns; cancellation/deadline checks
//! bracket it, but cannot preempt kernel or security-hook execution.
use crate::{
    Error, Result,
    operation_budget::{BudgetCharge, OperationBudget},
};
use std::{
    fs::File,
    sync::atomic::{AtomicBool, Ordering},
};

pub(crate) fn canonical_enrollment(text: &str) -> Result<uuid::Uuid> {
    if text.len() != 36 {
        return Err(Error("unsupported relocation UUID encoding".into()));
    }
    let id = uuid::Uuid::parse_str(text)
        .map_err(|_| Error("unsupported relocation UUID encoding".into()))?;
    let mut canonical = [0_u8; 36];
    if id.is_nil() || id.hyphenated().encode_lower(&mut canonical) != text {
        return Err(Error("unsupported relocation UUID encoding".into()));
    }
    Ok(id)
}
fn checkpoint(budget: &OperationBudget, cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        return Err(Error("relocation UUID probe cancelled".into()));
    }
    budget
        .charge(BudgetCharge {
            work: 1,
            ..Default::default()
        })
        .map_err(|e| Error(format!("relocation UUID probe budget: {e:?}")))
}
#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Default)]
struct FsUuid2 {
    len: u8,
    uuid: [u8; 16],
}
#[cfg(target_os = "linux")]
fn decode(result: FsUuid2, expected: uuid::Uuid) -> Result<()> {
    if result.len != 16 || result.uuid == [0; 16] || result.uuid != *expected.as_bytes() {
        return Err(Error("bound volume identity changed or unavailable".into()));
    }
    Ok(())
}
/// Only callers retaining the existing blocking owner may use this. The passed
/// budget is the current operation, never a new per-probe deadline. No retry or
/// path-based fallback is allowed. Existing root Files are O_RDONLY directories,
/// not O_PATH; permission/unsupported errors remain refusals.
pub(crate) fn validate(
    root: &File,
    expected: uuid::Uuid,
    budget: &OperationBudget,
    cancel: &AtomicBool,
) -> Result<()> {
    checkpoint(budget, cancel)?;
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let mut result = FsUuid2::default();
        // SAFETY: root owns a live descriptor throughout this synchronous call;
        // repr(C) matches Linux fsuuid2 (17 bytes/alignment1), and _IOR derives
        // the architecture-specific request and exact buffer size from it.
        let rc = unsafe {
            libc::ioctl(
                root.as_raw_fd(),
                libc::_IOR::<FsUuid2>(0x15, 0),
                &mut result as *mut FsUuid2,
            )
        };
        let error = (rc < 0).then(std::io::Error::last_os_error);
        // Check even on a failed syscall, before any result can authorize use.
        checkpoint(budget, cancel)?;
        if let Some(error) = error {
            return Err(Error(format!(
                "relocation UUID descriptor probe unavailable: {error}"
            )));
        }
        decode(result, expected)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (root, expected);
        Err(Error(
            "required UUID needs bounded relocation probe owner on this platform".into(),
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enrollment_requires_exact_canonical_non_nil_volume_uuid() {
        let text = "00010203-0405-0607-0809-0a0b0c0d0e0f";
        assert_eq!(
            *canonical_enrollment(text).unwrap().as_bytes(),
            std::array::from_fn(|i| i as u8)
        );
        for value in [
            "",
            "ABCD-1234",
            "00000000-0000-0000-0000-000000000000",
            "00010203-0405-0607-0809-0A0B0C0D0E0F",
            "000102030405060708090a0b0c0d0e0f",
        ] {
            assert!(canonical_enrollment(value).is_err());
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_uuid_abi_and_result_length_are_exact() {
        assert_eq!(std::mem::size_of::<FsUuid2>(), 17);
        assert_eq!(std::mem::align_of::<FsUuid2>(), 1);
        assert_eq!(std::mem::offset_of!(FsUuid2, uuid), 1);
        assert_eq!(
            libc::_IOR::<FsUuid2>(0x15, 0),
            libc::_IOR::<[u8; 17]>(0x15, 0)
        );
        let id = canonical_enrollment("00010203-0405-0607-0809-0a0b0c0d0e0f").unwrap();
        assert!(
            decode(
                FsUuid2 {
                    len: 16,
                    uuid: *id.as_bytes()
                },
                id
            )
            .is_ok()
        );
        for len in [0, 4, 15, 17, 255] {
            assert!(
                decode(
                    FsUuid2 {
                        len,
                        uuid: *id.as_bytes()
                    },
                    id
                )
                .is_err()
            );
        }
        assert!(
            decode(
                FsUuid2 {
                    len: 16,
                    uuid: [0; 16]
                },
                id
            )
            .is_err()
        );
        assert!(
            decode(
                FsUuid2 {
                    len: 16,
                    uuid: [1; 16]
                },
                id
            )
            .is_err()
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_unsupported_descriptor_cancel_and_sticky_parent_refuse_without_children() {
        use crate::operation_budget::BudgetLimits;
        use std::{os::unix::fs::OpenOptionsExt, time::Duration};
        let budget = OperationBudget::new(
            Duration::from_secs(5),
            BudgetLimits {
                items: 10,
                values: 10,
                bytes: 128,
                work: 4,
            },
        )
        .unwrap();
        let cancel = AtomicBool::new(true);
        let root = tempfile::tempdir().unwrap();
        // Deliberately unsupported O_PATH descriptor, not a production root or
        // enrolled-volume positive. ioctl must fail closed without fallback.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
            .open(root.path())
            .unwrap();
        let id = canonical_enrollment("00010203-0405-0607-0809-0a0b0c0d0e0f").unwrap();
        crate::source::without_children(|| {
            assert!(
                validate(&file, id, &budget, &cancel)
                    .unwrap_err()
                    .0
                    .contains("cancelled")
            );
            assert_eq!(budget.usage().work, 0);
            cancel.store(false, Ordering::Release);
            assert!(
                validate(&file, id, &budget, &cancel)
                    .unwrap_err()
                    .0
                    .contains("descriptor probe unavailable")
            );
            assert_eq!(budget.usage().work, 2);
            budget
                .charge(BudgetCharge {
                    work: 3,
                    ..Default::default()
                })
                .unwrap_err();
            assert!(
                validate(&file, id, &budget, &cancel)
                    .unwrap_err()
                    .0
                    .contains("budget")
            );
            Ok(())
        })
        .unwrap();
    }
}
