//! Independently fenced named current versions, never an application ACL.
use super::*;
impl HostSession {
    pub fn projection_selected_current_versions(
        &self,
        expected_generation: i64,
        requests: &[catabolic_store::catalog_projection::VersionLineageRequest],
    ) -> Result<Value> {
        let _lease = self
            .acquire_authorization()?
            .ok_or_else(|| invalid("selected projection requires retained authorization"))?;
        self.require_open()?;
        let store = self.session.store().map_err(internal)?;
        catabolic_store::catalog_projection::selected_current_versions(
            store,
            &catabolic_store::catalog_projection::TrustedProjectionAuthority {
                catalog_id: store.database_id.clone(),
                incarnation: store
                    .catalog_incarnation
                    .clone()
                    .ok_or_else(|| invalid("selected projection incarnation missing"))?,
                expected_generation,
            },
            &self.profile,
            requests,
        )
        .map_err(internal)
    }
}
