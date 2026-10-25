//! Retained-owner preparation reads. No execution or cleanup authority.
use super::*;
impl HostSession {
    /// Capture current facts for an independently authorized host preparation.
    /// Capture reserves nothing across awaits; dispatch must keep these exact
    /// heads and generation, and separately prove all new output/evidence scope.
    pub fn capture_generated_input(
        &self,
        expected_generation: i64,
        input_version_id: &str,
    ) -> Result<(
        catabolic_store::catalog_generated_output::GeneratedInputFacts,
        catabolic_store::catalog_import_typed::TypedAuthority,
    )> {
        let lease = self.acquire_authorization()?;
        self.require_open()?;
        let lease = lease
            .ok_or_else(|| invalid("generated preparation requires native host authorization"))?;
        let store = self.session.store().map_err(internal)?;
        let authority = catabolic_store::catalog_projection::TrustedProjectionAuthority {
            catalog_id: store.database_id.clone(),
            incarnation: store
                .catalog_incarnation
                .clone()
                .ok_or_else(|| invalid("native incarnation required"))?,
            expected_generation,
        };
        catabolic_store::catalog_generated_output::capture_input(
            store,
            &authority,
            lease.namespace(),
            &self.profile,
            input_version_id,
        )
        .map_err(internal)
    }
}

impl HostSession {
    /// Named retained history, never a playable manifest or currentness proof.
    pub fn historical_version(
        &self,
        expected_generation: i64,
        version_id: &str,
        association_item_id: &str,
        version_revision: i64,
    ) -> Result<Value> {
        let _lease = self.acquire_authorization()?.ok_or_else(|| {
            invalid("historical preparation read requires native host authorization")
        })?;
        self.require_open()?;
        let store = self.session.store().map_err(internal)?;
        let authority = catabolic_store::catalog_projection::TrustedProjectionAuthority {
            catalog_id: store.database_id.clone(),
            incarnation: store
                .catalog_incarnation
                .clone()
                .ok_or_else(|| invalid("native incarnation required"))?,
            expected_generation,
        };
        catabolic_store::catalog_generated_output::historical_version(
            store,
            &authority,
            &self.profile,
            version_id,
            association_item_id,
            version_revision,
        )
        .map_err(internal)
    }
}
