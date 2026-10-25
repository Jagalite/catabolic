//! Exact descriptor read under the actual retained host authorization lifetime.
use super::*;
use catabolic_core::catalog_identity::ExactFileRevision;

/// No serialization, path constructor or authority reconstruction is available.
/// Holding this value keeps native drain pending through all actual consumers.
pub struct ExactSource {
    lease: AuthorizationLease,
    authorization: HostAuthorization,
    source: catabolic_store::catalog_source_read::ExactSource,
}
impl ExactSource {
    pub fn descriptor(&self) -> Result<std::fs::File> {
        let _fresh = self
            .authorization
            .acquire(self.authorization.revision())
            .map_err(|_| invalid("exact source authorization is closed"))?;
        self.source.descriptor().map_err(internal)
    }
}
impl HostSession {
    pub fn open_exact_source(
        &self,
        expected_generation: i64,
        source: &str,
        exact: &ExactFileRevision,
    ) -> Result<ExactSource> {
        let lease = self
            .acquire_authorization()?
            .ok_or_else(|| invalid("exact source requires retained authorization"))?;
        self.require_open()?;
        let store = self.session.store().map_err(internal)?;
        let authority = catabolic_store::catalog_projection::TrustedProjectionAuthority {
            catalog_id: store.database_id.clone(),
            incarnation: store
                .catalog_incarnation
                .clone()
                .ok_or_else(|| invalid("exact source incarnation missing"))?,
            expected_generation,
        };
        let source = catabolic_store::catalog_source_read::ExactSource::open(
            store,
            &authority,
            &self.profile,
            source,
            exact,
        )
        .map_err(internal)?;
        Ok(ExactSource {
            lease,
            authorization: self.authorization()?,
            source,
        })
    }

    pub fn validate_exact_source(
        &self,
        expected_generation: i64,
        source: &ExactSource,
    ) -> Result<()> {
        let _current = self
            .acquire_authorization()?
            .ok_or_else(|| invalid("exact source requires retained authorization"))?;
        self.require_open()?;
        if !source.lease.belongs_to(&self.authorization()?) {
            return Err(invalid(
                "exact source belongs to another host authorization",
            ));
        }
        let store = self.session.store().map_err(internal)?;
        source
            .source
            .validate(
                store,
                &catabolic_store::catalog_projection::TrustedProjectionAuthority {
                    catalog_id: store.database_id.clone(),
                    incarnation: store
                        .catalog_incarnation
                        .clone()
                        .ok_or_else(|| invalid("exact source incarnation missing"))?,
                    expected_generation,
                },
            )
            .map_err(internal)
    }
}
