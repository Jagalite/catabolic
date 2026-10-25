//! Exact-subject metadata host reads. The embedding supplies current application
//! authorization; this privileged capability never infers it from a source name.
use super::*;
impl HostSession {
    pub fn metadata_contributions(
        &self,
        subject: &str,
    ) -> Result<catabolic_store::catalog_metadata_store::ContributionSnapshot> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        let store = self.session.store().map_err(internal)?;
        let generation = store
            .db
            .query_row("SELECT generation FROM catalog_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .map_err(|e| internal(e.into()))?;
        catabolic_store::catalog_metadata_store::read_contributions(
            store,
            &catabolic_store::catalog_metadata_store::MetadataReadAuthority {
                catalog_id: store.database_id.clone(),
                incarnation: store
                    .catalog_incarnation
                    .clone()
                    .ok_or_else(|| invalid("native metadata required"))?,
                subject: subject.into(),
                expected_generation: generation,
            },
        )
        .map_err(|failure| internal(failure.error))
    }
}

impl HostSession {
    pub fn verified_metadata_created_origin(
        &self,
        creation: &catabolic_core::catalog_command::CommandEnvelope,
        subject: &str,
    ) -> Result<catabolic_store::catalog_metadata_store::VerifiedCreatedOrigin> {
        let _authorization = self.acquire_authorization()?;
        self.require_open()?;
        let namespace = self.authorization()?.namespace().to_owned();
        let store = self.session.store().map_err(internal)?;
        catabolic_store::catalog_metadata_store::verify_created_origin(
            store,
            creation,
            subject,
            catabolic_store::catalog_commands::TrustedCommandAuthority {
                host_namespace: &namespace,
                receipt_visibility: catabolic_core::catalog_command::ReceiptVisibility::FullResult,
                allow_new_commands: false,
                identity_authority: None,
            },
        )
        .map_err(|e| internal(e.error))
    }
}
