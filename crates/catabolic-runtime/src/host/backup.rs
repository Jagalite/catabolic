//! Database-only export; service drain and Motion custody belong to the host.
use super::*;
impl HostSession {
    pub fn export_snapshot(
        &self,
        expected_generation: i64,
        parent: &Path,
        cancelled: &AtomicBool,
    ) -> Result<catabolic_store::catalog_backup::VerifiedExport> {
        self.export_snapshot_reserved(expected_generation, parent, cancelled, 0)
    }
    /// Reservation tightens memory accounting, never grants export authority.
    pub fn export_snapshot_reserved(
        &self,
        expected_generation: i64,
        parent: &Path,
        cancelled: &AtomicBool,
        reserved: u64,
    ) -> Result<catabolic_store::catalog_backup::VerifiedExport> {
        let _lease = self
            .acquire_authorization()?
            .ok_or_else(|| invalid("snapshot export requires retained host authorization"))?;
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
        catabolic_store::catalog_backup::export_reserved(
            store, &authority, parent, cancelled, reserved,
        )
        .map_err(internal)
    }
}
