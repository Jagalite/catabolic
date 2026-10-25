//! Read-only current named structural ownership under the retained host.
use super::*;
impl HostSession {
    pub fn projection_version_lineage(
        &self,
        expected_generation: i64,
        requests: &[catabolic_store::catalog_projection::VersionLineageRequest],
    ) -> Result<Value> {
        let _lease = self
            .acquire_authorization()?
            .ok_or_else(|| invalid("lineage projection requires retained authorization"))?;
        self.require_open()?;
        let store = self.session.store().map_err(internal)?;
        catabolic_store::catalog_projection::version_lineage(
            store,
            &catabolic_store::catalog_projection::TrustedProjectionAuthority {
                catalog_id: store.database_id.clone(),
                incarnation: store
                    .catalog_incarnation
                    .clone()
                    .ok_or_else(|| invalid("lineage incarnation missing"))?,
                expected_generation,
            },
            requests,
        )
        .map_err(internal)
    }
}
