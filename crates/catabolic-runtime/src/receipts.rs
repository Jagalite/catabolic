// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Bounded external receipt input. Store owns validation and atomic provenance.
use catabolic_store::{Error, Result, Store, receipt_mutation};
pub use receipt_mutation::source_receipt;
use serde_json::Value;
use std::{io::Read, path::Path};

pub fn import_receipt(store: &Store, profile: &str, value: &Value) -> Result<Value> {
    receipt_mutation::import_receipt(store, profile, value)
}
pub fn import_bytes(store: &Store, profile: &str, bytes: &[u8]) -> Result<Value> {
    if bytes.len() > receipt_mutation::MAX_RECEIPT_BYTES {
        return Err(Error("receipt exceeds 1 MiB".into()));
    }
    let value = catabolic_core::decode_json_bytes(bytes)?;
    import_receipt(store, profile, &value)
}
pub fn import_file(store: &Store, profile: &str, path: &Path) -> Result<Value> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(Error("receipt input must be a regular file".into()));
    }
    let mut bytes = Vec::new();
    file.take(receipt_mutation::MAX_RECEIPT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    import_bytes(store, profile, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_parser_rejects_nonfinite_invalid_utf8_and_private_number_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.db");
        catabolic_store::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        assert!(
            import_bytes(
                &store,
                "default",
                &vec![b' '; receipt_mutation::MAX_RECEIPT_BYTES + 1]
            )
            .unwrap_err()
            .0
            .contains("1 MiB")
        );
        for raw in [
            b"NaN".as_slice(),
            b"1e999",
            b"\xff",
            b"{\"$serde_json::private::Number\":\"1\"}",
            b"{\"version\":{\"$serde_json::private::Number\":\"1\"}}",
        ] {
            assert!(import_bytes(&store, "default", raw).is_err());
        }
    }
    #[test]
    fn receipt_input_is_bounded_regular_nonsymlink_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.db");
        catabolic_store::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let oversized = dir.path().join("large.json");
        std::fs::write(
            &oversized,
            vec![b' '; receipt_mutation::MAX_RECEIPT_BYTES + 1],
        )
        .unwrap();
        assert!(
            import_file(&store, "default", &oversized)
                .unwrap_err()
                .0
                .contains("1 MiB")
        );
        let symlink = dir.path().join("link.json");
        std::os::unix::fs::symlink(&oversized, &symlink).unwrap();
        assert!(import_file(&store, "default", &symlink).is_err());
        assert!(
            import_file(&store, "default", dir.path())
                .unwrap_err()
                .0
                .contains("regular")
        );
        let fifo = dir.path().join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: the temporary directory and NUL-terminated name are live.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(
            import_file(&store, "default", &fifo)
                .unwrap_err()
                .0
                .contains("regular")
        );
    }
}
