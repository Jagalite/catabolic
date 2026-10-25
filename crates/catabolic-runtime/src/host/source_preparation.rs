//! Normal native40 retained-host source preparation. Descriptive structural
//! closure only, never a relocation/hash/current-byte or binding mutation grant.
use super::*;
impl HostSession {
    pub fn prepare_source_succession(
        &self,
        expected_generation: i64,
        source: &str,
    ) -> Result<catabolic_store::catalog_source_succession::SourcePreparation> {
        let _authorization = self
            .acquire_authorization()?
            .ok_or_else(|| invalid("source preparation requires retained authorization"))?;
        self.require_open()?;
        let store = self.session.store().map_err(internal)?;
        let authority = catabolic_store::catalog_projection::TrustedProjectionAuthority {
            catalog_id: store.database_id.clone(),
            incarnation: store
                .catalog_incarnation
                .clone()
                .ok_or_else(|| invalid("source preparation incarnation missing"))?,
            expected_generation,
        };
        catabolic_store::catalog_source_succession::prepare(
            store,
            &authority,
            &self.profile,
            source,
        )
        .map_err(internal)
    }
    /// Recaptures the complete closure; old saved heads alone cannot exclude a
    /// newly referencing context. This does not issue a mutation capability.
    pub fn revalidate_source_preparation(
        &self,
        prepared: &catabolic_store::catalog_source_succession::SourcePreparation,
    ) -> Result<()> {
        let _authorization = self
            .acquire_authorization()?
            .ok_or_else(|| invalid("source preparation requires retained authorization"))?;
        self.require_open()?;
        if prepared.profile() != self.profile {
            return Err(invalid("source preparation profile differs"));
        }
        catabolic_store::catalog_source_succession::revalidate(
            self.session.store().map_err(internal)?,
            prepared,
        )
        .map_err(internal)
    }
}
