//! Exact work-edge owner dispatch preserving typed operation refusals.
use super::*;
impl HostSession {
    pub fn execute_relationship_command_observed(
        &mut self,
        command: &catabolic_core::catalog_command::CommandEnvelope,
        grant: &catabolic_store::catalog_relationships::RelationshipAuthority,
        policy: catabolic_store::catalog_commands::ReceiptPolicy,
    ) -> Result<(
        catabolic_store::catalog_commands::CommandOutcome,
        Option<StaleGenerationWitness>,
    )> {
        use catabolic_store::catalog_commands::{
            CatalogAuthority, CommandStoreErrorCode, TrustedCommandAuthority,
        };
        let lease = self
            .acquire_authorization()?
            .ok_or_else(|| invalid("relationship dispatch requires retained authorization"))?;
        self.require_open()?;
        if !self.writable
            || command.key().host_namespace() != lease.namespace()
            || grant.host_namespace != lease.namespace()
            || command.payload()["operation"] != "relationships"
        {
            return Err(invalid(
                "relationship operation or retained namespace mismatch",
            ));
        }
        let grants = CatalogAuthority::new(TrustedCommandAuthority {
            host_namespace: lease.namespace(),
            receipt_visibility: catabolic_core::catalog_command::ReceiptVisibility::FullResult,
            allow_new_commands: true,
            identity_authority: None,
        })
        .with_relationships(grant);
        let outcome = catabolic_store::catalog_commands::execute_with_authority(
            self.session.store().map_err(internal)?,
            command,
            grants,
            policy,
        )
        .map_err(|failure| match failure.code {
            CommandStoreErrorCode::OperationConflict => error(
                ErrorCode::Conflict,
                Retry::CorrectRequest,
                "relationship operation conflicts with current native graph",
            ),
            CommandStoreErrorCode::InvalidOperation => invalid("relationship input is invalid"),
            _ => internal(failure.error),
        })?;
        let witness = self.stale_generation_witness(command, &outcome)?;
        Ok((outcome, witness))
    }
}
