//! Descriptive bootstrap discovery, not a source grant, recovered result or
//! authority to read media. Normal schema40 admission only. A later bootstrap
//! owner independently recaptures the complete inventory and effect proof.
pub use super::bootstrap_inventory::{
    Assertion, BootstrapInventory, FileInventory, HeadStamp, HistoryStep, MergeHistory, Obligation,
    ReceiptCommitment, Retirement, SchemaHead, SourceHead, SubjectInventory, TagHistory,
};
use crate::{Error, Result, Store, catalog_projection::TrustedProjectionAuthority};

pub fn capture(store: &Store, expected: &TrustedProjectionAuthority) -> Result<BootstrapInventory> {
    if store.schema_version != 40 || !store.writable || !store.db.is_autocommit() {
        return Err(Error(
            "NFO inventory requires normal native40 writer without ambient transaction".into(),
        ));
    }
    store.transaction_available()?;
    store.transaction(|_| {
        let inventory = super::bootstrap_inventory::capture_in(store)?;
        if inventory.catalog_id() != expected.catalog_id
            || inventory.incarnation() != expected.incarnation
            || inventory.generation() != expected.expected_generation
        {
            return Err(Error(
                "NFO inventory identity or expected generation differs".into(),
            ));
        }
        // Do not call require_complete: unresolved item/file/merge obligations
        // must cross this descriptive boundary explicitly, never disappear.
        Ok(inventory)
    })
}
