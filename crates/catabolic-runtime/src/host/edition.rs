//! Retained-owner descriptive timeline projection, including empty editions.
use super::*;
impl HostSession {
    pub fn projection_timeline(
        &self,
        expected_generation: i64,
        timeline_id: &str,
        edition_item_id: &str,
    ) -> Result<Value> {
        let _lease = self
            .acquire_authorization()?
            .ok_or_else(|| invalid("timeline projection requires retained host authorization"))?;
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
        catabolic_store::catalog_projection::named_timeline(
            store,
            &authority,
            timeline_id,
            edition_item_id,
        )
        .map_err(internal)
    }
}

impl HostSession {
    /// The embedding adapter must retain its current complete-membership write
    /// authorization and aggregate precondition reservation through this call.
    pub fn execute_empty_edition_observed(
        &mut self,
        command: &catabolic_core::catalog_command::CommandEnvelope,
        grant: &catabolic_store::catalog_edition_creation::EmptyEditionAuthority,
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
            .ok_or_else(|| invalid("empty edition dispatch requires retained authorization"))?;
        self.require_open()?;
        if !self.writable
            || command.key().host_namespace() != lease.namespace()
            || grant.host_namespace != lease.namespace()
            || grant.profile != self.profile
            || command.payload()["operation"] != "empty_edition_create"
        {
            return Err(invalid(
                "empty edition operation/profile/namespace mismatch",
            ));
        }
        let authority = CatalogAuthority::new(TrustedCommandAuthority {
            host_namespace: lease.namespace(),
            receipt_visibility: catabolic_core::catalog_command::ReceiptVisibility::FullResult,
            allow_new_commands: true,
            identity_authority: None,
        })
        .with_empty_edition(grant);
        let outcome = catabolic_store::catalog_commands::execute_with_authority(
            self.session.store().map_err(internal)?,
            command,
            authority,
            policy,
        )
        .map_err(|failure| match failure.code {
            CommandStoreErrorCode::OperationConflict => error(
                ErrorCode::Conflict,
                Retry::CorrectRequest,
                "empty edition operation conflicts with current native owners",
            ),
            CommandStoreErrorCode::InvalidOperation => invalid("empty edition input is invalid"),
            _ => internal(failure.error),
        })?;
        let witness = self.stale_generation_witness(command, &outcome)?;
        Ok((outcome, witness))
    }
}

impl HostSession {
    /// Absence observation only, not a create grant. The command must preserve
    /// this generation; the native owner repeats all non-reuse checks.
    pub fn empty_edition_allocation_available(
        &self,
        request: &catabolic_store::catalog_edition_creation::EmptyEditionRequest,
    ) -> Result<bool> {
        let _lease = self
            .acquire_authorization()?
            .ok_or_else(|| invalid("edition allocation requires retained authorization"))?;
        self.require_open()?;
        let edition = format!("motion.edition:{}", request.edition_id);
        self.session.store().map_err(internal)?.db.query_row(
            "SELECT NOT (EXISTS(SELECT 1 FROM items WHERE id=?1) OR EXISTS(SELECT 1 FROM catalog_entity_revisions WHERE (entity_kind='items' AND entity_key=json_array(?1)) OR (entity_kind='item_relationships' AND entity_key=json_array(?3))) OR EXISTS(SELECT 1 FROM catalog_timelines WHERE id=?2) OR EXISTS(SELECT 1 FROM item_relationships WHERE id=?3) OR EXISTS(SELECT 1 FROM catalog_segmentation_contexts WHERE id=?4) OR EXISTS(SELECT 1 FROM catalog_entity_revisions WHERE entity_kind='catalog_timelines' AND entity_key=json_array(?2)) OR EXISTS(SELECT 1 FROM catalog_entity_revisions WHERE entity_kind='catalog_segmentation_contexts' AND entity_key=json_array(?4)))",
            rusqlite::params![edition,request.timeline_id,request.relationship_id,request.context_id],|r|r.get(0)
        ).map_err(|e|internal(e.into()))
    }
}
