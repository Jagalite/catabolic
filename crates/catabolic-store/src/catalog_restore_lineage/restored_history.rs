//! Historical authority only for the independently verified isolated rotation.
//! Never a Store, reopen token, current cleanup witness or command admission.
use super::{chain, fail};
use crate::{Result, catalog_backup::inventory::Audit};
use rusqlite::Connection;

pub(super) struct RestoredHistoryState {
    catalog: String,
    current: String,
    generation: i64,
    transition: String,
    ancestors: Vec<String>,
}
/// Non-serializable, non-clone proof bound to one immutable read connection.
pub(crate) struct AdmittedRestoreHistory<'a> {
    db: &'a Connection,
    state: &'a RestoredHistoryState,
}
impl RestoredHistoryState {
    // Only the fixed isolated-image producer calls this after independent byte
    // reinspection and exact generation-preserving rotation delta validation.
    pub(super) fn from_rotation(
        before: &Audit,
        after: &Audit,
        origin: &chain::Origin,
        ancestry: chain::Chain,
        generation: i64,
        transition: &str,
    ) -> Result<Self> {
        if generation < 0
            || before.schema != after.schema
            || before.ledger != after.ledger
            || origin.native_inventory_sha256 != before.complete
            || ancestry.origins.last() != Some(origin)
            || before.tables.keys().ne(after.tables.keys())
        {
            return Err(fail("restored historical producer closure differs"));
        }
        for (name, digest) in &before.tables {
            if !matches!(
                name.as_str(),
                "catalog_state" | "catalog_restore_lineages" | "catalog_restore_worker_history"
            ) && after.tables.get(name) != Some(digest)
            {
                return Err(fail("restored historical rows changed"));
            }
        }
        let mut ancestors = Vec::with_capacity(ancestry.origins.len());
        for hop in ancestry.origins {
            ancestors.push(hop.from_incarnation);
        }
        Ok(Self {
            catalog: origin.catalog_id.clone(),
            current: origin.to_incarnation.clone(),
            generation,
            transition: transition.into(),
            ancestors,
        })
    }
    pub(super) fn admit<'a>(
        &'a self,
        image: &'a super::completed_image::ImmutableDeserializedImage,
    ) -> Result<AdmittedRestoreHistory<'a>> {
        let db = image.connection();
        let proof = AdmittedRestoreHistory { db, state: self };
        proof.check(db)?;
        Ok(proof)
    }
    pub(super) fn retained_allocations(&self) -> Result<u64> {
        let slots = self
            .ancestors
            .capacity()
            .checked_mul(std::mem::size_of::<String>())
            .ok_or_else(|| fail("restore history allocation overflow"))?;
        let mut total = [
            self.catalog.capacity(),
            self.current.capacity(),
            self.transition.capacity(),
            slots,
        ]
        .into_iter()
        .try_fold(0u64, |n, v| {
            n.checked_add(v as u64)
                .ok_or_else(|| fail("restore history allocation overflow"))
        })?;
        for value in &self.ancestors {
            total = total
                .checked_add(value.capacity() as u64)
                .ok_or_else(|| fail("restore history allocation overflow"))?;
        }
        Ok(total)
    }
}
impl AdmittedRestoreHistory<'_> {
    pub(crate) fn check(&self, db: &Connection) -> Result<()> {
        // Immutability comes only from the closed readonly-deserialize producer.
        // db_readonly checks different Btree flags and is not a memdb witness.
        if !std::ptr::eq(db, self.db) || db.is_autocommit() {
            return Err(fail(
                "restore history requires exact immutable read snapshot",
            ));
        }
        let equal: bool = db.query_row(
            "SELECT catalog_id,incarnation,generation FROM main.catalog_state WHERE id=1",
            [],
            |r| {
                Ok(
                    r.get_ref(0)?.as_str().ok() == Some(self.state.catalog.as_str())
                        && r.get_ref(1)?.as_str().ok() == Some(self.state.current.as_str())
                        && r.get::<_, i64>(2)? == self.state.generation,
                )
            },
        )?;
        if !equal {
            return Err(fail("restore history current identity changed"));
        }
        // Materialize the identity read before testing the deferred BEGIN's
        // actual main transaction state. This is a read snapshot, never WRITE.
        let state = unsafe { rusqlite::ffi::sqlite3_txn_state(db.handle(), c"main".as_ptr()) };
        if state != rusqlite::ffi::SQLITE_TXN_READ {
            return Err(fail(
                "restore history requires exact immutable read snapshot",
            ));
        }
        Ok(())
    }
    pub(crate) fn catalog_id(&self) -> &str {
        &self.state.catalog
    }
    pub(crate) fn incarnation(&self) -> &str {
        &self.state.current
    }
    pub(crate) fn generation(&self) -> i64 {
        self.state.generation
    }
    pub(crate) fn transition_digest(&self) -> &str {
        &self.state.transition
    }
    /// Oldest original incarnation first, current tip last. Borrowed bounded
    /// facts only; callers must check the exact connection before SQL assembly.
    pub(crate) fn ordered_origins(&self) -> impl Iterator<Item = &str> {
        self.state
            .ancestors
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(self.state.current.as_str()))
    }
    pub(crate) fn origin_rank(&self, incarnation: &str) -> Result<usize> {
        self.ordered_origins()
            .position(|v| v == incarnation)
            .ok_or_else(|| fail("unauthenticated historical incarnation"))
    }
    pub(crate) fn require_forward_origins(&self, earlier: &str, later: &str) -> Result<()> {
        if self.origin_rank(earlier)? > self.origin_rank(later)? {
            return Err(fail(
                "historical source moves backwards through restore ancestry",
            ));
        }
        Ok(())
    }
    /// This is a current-image bound, not an invented per-hop generation cap.
    pub(crate) fn authorize_historical(&self, incarnation: &str, committed: i64) -> Result<()> {
        self.check(self.db)?;
        if committed < 0
            || committed > self.state.generation
            || (incarnation != self.state.current
                && !self.state.ancestors.iter().any(|v| v == incarnation))
        {
            return Err(fail(
                "historical incarnation is not authenticated by this image",
            ));
        }
        Ok(())
    }
}
