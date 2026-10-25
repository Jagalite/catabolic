//! Retained-owner logical review. Preview reserves nothing and grants no effects.
use super::*;

impl HostSession {
    /// Review through the same admitted native owner that will execute the
    /// eventual command. The caller supplies independent endpoint/identity
    /// grants; neither this method nor the returned review expands them.
    pub fn preview_logical_edit(
        &self,
        expected_generation: i64,
        request: catabolic_store::catalog_logical_edits::LogicalEditRequest,
        authority: &catabolic_store::catalog_logical_edits::LogicalEditAuthority,
        identity_authority: Option<&catabolic_store::catalog_identity_mutation::IdentityAuthority>,
    ) -> Result<catabolic_store::catalog_logical_edits::LogicalEditPreview> {
        let lease = self
            .acquire_authorization()?
            .ok_or_else(|| invalid("logical review requires native host authorization"))?;
        self.require_open()?;
        if authority.host_namespace != lease.namespace() {
            return Err(invalid(
                "logical review namespace differs from retained host authority",
            ));
        }
        if request
            .contexts
            .iter()
            .any(|context| context.mutation.profile != self.profile)
        {
            return Err(invalid(
                "logical review profile differs from retained host profile",
            ));
        }
        if self.catalog_generation()? != expected_generation {
            return Err(invalid("logical review generation changed"));
        }
        let store = self.session.store().map_err(internal)?;
        let preview = catabolic_store::catalog_logical_edits::preview_logical_edit(
            store,
            request,
            authority,
            identity_authority,
        )
        .map_err(|failure| {
            use catabolic_store::catalog_logical_edits::LogicalEditErrorCode;
            let mut mapped = match failure.code {
                LogicalEditErrorCode::InvalidInput => invalid("logical review input is invalid"),
                LogicalEditErrorCode::AuthorityDenied => error(
                    ErrorCode::UnsupportedCapability,
                    Retry::DoNotRetry,
                    "logical review is outside independently granted scope",
                ),
                LogicalEditErrorCode::Conflict => error(
                    ErrorCode::Conflict,
                    Retry::CorrectRequest,
                    "logical review conflicts with the current native graph",
                ),
                LogicalEditErrorCode::Unsupported => error(
                    ErrorCode::UnsupportedCapability,
                    Retry::CorrectRequest,
                    "logical review requires an unsupported native capability",
                ),
                LogicalEditErrorCode::StoreFailure => return internal(failure.error),
            };
            // Preserve the owner cause for trusted operator diagnostics while
            // retaining the typed public classification and static detail.
            mapped.diagnostic = Some(failure.error);
            mapped
        })?;
        if preview.reviewed.catalog_id != store.database_id
            || Some(preview.reviewed.incarnation.as_str()) != store.catalog_incarnation.as_deref()
            || preview.reviewed.expected_generation != expected_generation
        {
            return Err(invalid("logical review identity or generation changed"));
        }
        Ok(preview)
    }
}
