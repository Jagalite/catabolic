//! Owned isolated image admission is distinct from Store pathname admission.
//! The production41 constructor awaits reviewed registration/conversion proof.
use super::*;

struct OwnedImage {
    db: Connection,
    poisoned: std::cell::Cell<bool>,
}
impl OwnedImage {
    fn admitted(&self) -> Result<AdmittedRestoreOwner<'_>> {
        if self.poisoned.get()
            || self.db.is_autocommit()
            || self.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
        {
            return Err(fail(
                "isolated restore image requires owned unpoisoned writer",
            ));
        }
        Ok(AdmittedRestoreOwner {
            store: None,
            db: &self.db,
            poisoned: Some(&self.poisoned),
        })
    }
}

// No production constructor exists until the exact registered41/conversion
// validator can mint this owner from retained verified pair custody.
#[cfg(test)]
impl OwnedImage {
    fn candidate(
        bytes: &[u8],
        expected_catalog: &str,
        expected_incarnation: &str,
        expected_inventory: &str,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        use std::time::{Duration, Instant};
        check(cancel)?;
        let length = u64::try_from(bytes.len()).map_err(|_| fail("image length overflow"))?;
        if length == 0 || length > 56 * 1024 * 1024 {
            return Err(fail("isolated restore image cohort exceeded"));
        }
        let mut db = Connection::open_in_memory()?;
        db.pragma_update(None, "cache_size", -2048)?;
        image_allocation(length, &[&db])?;
        db.deserialize_read_exact("main", std::io::Cursor::new(bytes), bytes.len(), false)?;
        db.pragma_update(None, "foreign_keys", true)?;
        db.pragma_update(None, "recursive_triggers", true)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        db.progress_handler(10000, Some(move || Instant::now() >= deadline))?;
        image_allocation(length, &[&db])?;
        let (catalog, incarnation) = crate::candidate41_test_support::validate_closed_image(&db)?;
        if catalog != expected_catalog || incarnation != expected_incarnation {
            return Err(fail("isolated restore image identity mismatch"));
        }
        let integrity: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if integrity != "ok" {
            return Err(fail("isolated restore image integrity failed"));
        }
        let audit = crate::catalog_backup::inventory::capture(&db, cancel, deadline)?;
        if audit.complete != expected_inventory {
            return Err(fail("isolated restore image inventory mismatch"));
        }
        image_allocation(length, &[&db])?;
        check(cancel)?;
        db.progress_handler(0, None::<fn() -> bool>)?;
        db.execute_batch("BEGIN IMMEDIATE")?;
        Ok(Self {
            db,
            poisoned: std::cell::Cell::new(false),
        })
    }

    fn rotate_and_serialize(
        self,
        pair: &str,
        binding: &str,
        cancel: &AtomicBool,
    ) -> Result<(Vec<u8>, chain::Origin)> {
        use std::time::{Duration, Instant};
        let admitted = self.admitted()?;
        let origin = rotate_admitted_with(&admitted, pair, binding, cancel, || Ok(()))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let before = crate::catalog_backup::inventory::capture(&self.db, cancel, deadline)?;
        check(cancel)?;
        self.db.execute_batch("COMMIT")?;
        let pages: i64 = self
            .db
            .pragma_query_value(None, "page_count", |r| r.get(0))?;
        let page_size: i64 = self
            .db
            .pragma_query_value(None, "page_size", |r| r.get(0))?;
        let image_bytes = u64::try_from(pages)
            .ok()
            .and_then(|p| u64::try_from(page_size).ok().and_then(|s| p.checked_mul(s)))
            .ok_or_else(|| fail("rotated image size overflow"))?;
        // Charge growth before SQLite allocates the serialization buffer.
        image_allocation(image_bytes, &[&self.db])?;
        let bytes = self.db.serialize("main")?;
        image_allocation(bytes.len() as u64, &[&self.db])?;
        let mut copy = Connection::open_in_memory()?;
        copy.pragma_update(None, "cache_size", -2048)?;
        image_allocation(bytes.len() as u64, &[&self.db, &copy])?;
        copy.deserialize_read_exact("main", std::io::Cursor::new(&*bytes), bytes.len(), true)?;
        copy.pragma_update(None, "foreign_keys", true)?;
        copy.pragma_update(None, "recursive_triggers", true)?;
        copy.progress_handler(10000, Some(move || Instant::now() >= deadline))?;
        image_allocation(bytes.len() as u64, &[&self.db, &copy])?;
        let identity = crate::candidate41_test_support::validate_closed_image(&copy)?;
        if identity != (origin.catalog_id.clone(), origin.to_incarnation.clone())
            || crate::catalog_backup::inventory::capture(&copy, cancel, deadline)? != before
        {
            return Err(fail("rotated image independent copy mismatch"));
        }
        let integrity: String = copy.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if integrity != "ok" {
            return Err(fail("rotated image integrity failed"));
        }
        image_allocation(bytes.len() as u64, &[&self.db, &copy])?;
        check(cancel)?;
        Ok((bytes.to_vec(), origin))
    }
}

#[cfg(test)]
fn image_allocation(bytes: u64, dbs: &[&Connection]) -> Result<()> {
    // Five image charges cover retained input, owned SQLite image, serialized
    // output, independent verifier and returned Vec;64MiB reserves inventory/lineage copies.
    // This is checked allocation accounting, not a process RSS guarantee.
    if bytes == 0 || bytes > 56 * 1024 * 1024 {
        return Err(fail("isolated restore image cohort exceeded"));
    }
    let mut total = bytes
        .checked_mul(5)
        .and_then(|n| n.checked_add(64 * 1024 * 1024))
        .ok_or_else(|| fail("isolated restore allocation overflow"))?;
    for db in dbs {
        total = total
            .checked_add(crate::catalog_backup::allocations(db)?)
            .ok_or_else(|| fail("isolated restore allocation overflow"))?;
    }
    if total > 256 * 1024 * 1024 {
        return Err(fail("isolated restore allocation budget exhausted"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    fn fixture() -> (Vec<u8>, String, String, String) {
        let (_root, _path, store) = super::super::tests::candidate();
        let mut copy = Connection::open_in_memory().unwrap();
        {
            let backup = rusqlite::backup::Backup::new(&store.db, &mut copy).unwrap();
            backup
                .run_to_completion(128, Duration::from_millis(1), None)
                .unwrap();
        }
        let cancel = AtomicBool::new(false);
        let audit = crate::catalog_backup::inventory::capture(
            &copy,
            &cancel,
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
        (
            copy.serialize("main").unwrap().to_vec(),
            store.database_id.clone(),
            store.catalog_incarnation.clone().unwrap(),
            audit.complete,
        )
    }
    #[test]
    fn owned_image_rotation_serializes_independently_verified_new_identity() {
        let (bytes, catalog, incarnation, inventory) = fixture();
        let cancel = AtomicBool::new(false);
        let image =
            OwnedImage::candidate(&bytes, &catalog, &incarnation, &inventory, &cancel).unwrap();
        let (rotated, origin) = image
            .rotate_and_serialize(&"a".repeat(64), &"b".repeat(64), &cancel)
            .unwrap();
        assert_ne!(origin.to_incarnation, incarnation);
        assert_ne!(rotated, bytes);
        // The original input remains an independently admissible old image.
        OwnedImage::candidate(&bytes, &catalog, &incarnation, &inventory, &cancel).unwrap();
    }
    #[test]
    fn owned_image_identity_inventory_corruption_cancel_and_budget_refuse() {
        let (bytes, catalog, incarnation, inventory) = fixture();
        let cancel = AtomicBool::new(false);
        assert!(
            OwnedImage::candidate(&bytes, "foreign", &incarnation, &inventory, &cancel).is_err()
        );
        assert!(
            OwnedImage::candidate(&bytes, &catalog, &incarnation, &"0".repeat(64), &cancel)
                .is_err()
        );
        assert!(
            OwnedImage::candidate(
                &bytes[..bytes.len() / 2],
                &catalog,
                &incarnation,
                &inventory,
                &cancel
            )
            .is_err()
        );
        cancel.store(true, Ordering::Release);
        assert!(
            OwnedImage::candidate(&bytes, &catalog, &incarnation, &inventory, &cancel).is_err()
        );
        assert!(image_allocation(u64::MAX, &[]).is_err());
    }
    #[test]
    fn owned_image_post_rotation_error_rolls_back_and_drop_preserves_input() {
        let (bytes, catalog, incarnation, inventory) = fixture();
        let cancel = AtomicBool::new(false);
        let image =
            OwnedImage::candidate(&bytes, &catalog, &incarnation, &inventory, &cancel).unwrap();
        assert!(
            rotate_admitted_with(
                &image.admitted().unwrap(),
                &"a".repeat(64),
                &"b".repeat(64),
                &cancel,
                || Err(fail("injected after rotation"))
            )
            .is_err()
        );
        let actual: String = image
            .db
            .query_row(
                "SELECT incarnation FROM catalog_state WHERE id=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(actual, incarnation);
        assert_eq!(
            image
                .db
                .query_row("SELECT count(*) FROM catalog_restore_lineages", [], |r| r
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            0
        );
        drop(image);
        OwnedImage::candidate(&bytes, &catalog, &incarnation, &inventory, &cancel).unwrap();
    }
    #[test]
    fn owned_image_mid_rotation_cancel_and_unwind_preserve_old_identity() {
        for panic in [false, true] {
            let (bytes, catalog, incarnation, inventory) = fixture();
            let cancel = AtomicBool::new(false);
            let image =
                OwnedImage::candidate(&bytes, &catalog, &incarnation, &inventory, &cancel).unwrap();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                rotate_admitted_with(
                    &image.admitted().unwrap(),
                    &"a".repeat(64),
                    &"b".repeat(64),
                    &cancel,
                    || {
                        if panic {
                            panic!("injected isolated image post-effect unwind");
                        }
                        cancel.store(true, Ordering::Release);
                        Ok(())
                    },
                )
            }));
            if panic {
                assert!(outcome.is_err());
            } else {
                assert!(outcome.unwrap().is_err());
            }
            let actual: String = image
                .db
                .query_row(
                    "SELECT incarnation FROM catalog_state WHERE id=1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(actual, incarnation);
            assert_eq!(
                image
                    .db
                    .query_row("SELECT count(*) FROM catalog_restore_lineages", [], |r| r
                        .get::<_, i64>(
                        0
                    ))
                    .unwrap(),
                0
            );
            assert!(!image.poisoned.get());
            image.db.execute_batch("COMMIT").unwrap();
            drop(image);
            cancel.store(false, Ordering::Release);
            OwnedImage::candidate(&bytes, &catalog, &incarnation, &inventory, &cancel).unwrap();
        }
    }
}
