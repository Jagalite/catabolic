//! Bounded historical NFO discovery for the retained embedding owner. The
//! returned descriptions are not provenance, curation or filesystem grants.
use super::*;
impl HostSession {
    pub fn nfo_bootstrap_inventory(
        &self,
        expected_generation: i64,
    ) -> Result<catabolic_store::catalog_nfo_attribution::inventory::BootstrapInventory> {
        let _authorization = self
            .acquire_authorization()?
            .ok_or_else(|| invalid("NFO inventory requires retained authorization"))?;
        self.require_open()?;
        let store = self.session.store().map_err(internal)?;
        catabolic_store::catalog_nfo_attribution::inventory::capture(
            store,
            &catabolic_store::catalog_projection::TrustedProjectionAuthority {
                catalog_id: store.database_id.clone(),
                incarnation: store
                    .catalog_incarnation
                    .clone()
                    .ok_or_else(|| invalid("NFO inventory native incarnation missing"))?,
                expected_generation,
            },
        )
        .map_err(internal)
    }
}
