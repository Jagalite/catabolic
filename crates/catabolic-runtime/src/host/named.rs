//! Exact named policy facts without saved-query creation or authority expansion.
use super::*;
impl HostSession {
    pub fn projection_named_items(
        &self,
        expected_generation: i64,
        ids: &[String],
    ) -> Result<Value> {
        let _lease = self
            .acquire_authorization()?
            .ok_or_else(|| invalid("named projection requires retained host authorization"))?;
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
        catabolic_store::catalog_projection::named_items(store, &authority, ids).map_err(internal)
    }
}
