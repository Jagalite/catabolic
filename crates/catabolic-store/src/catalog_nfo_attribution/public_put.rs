//! Unregistered existing-source NFO PUT composition. Authentication, complete
//! work curation, source/schema/tag grants and receipt disclosure are established
//! by the embedding host; this private helper never derives them from a payload.
//! The durable dispatcher must own the sole transaction and propagate every
//! error, including errors after metadata effects, without committing.
use super::{permit, publication, state};
use crate::{
    Error, Result, Store,
    catalog_metadata_store::{self, ContributionReplace, MetadataAuthority},
    catalog_tag_mutation::TagAuthority,
};
use catabolic_core::catalog_command::CommandEnvelope;

/// Existing and withdrawn typed sources only. A never-existing source needs the
/// independently verified positive absence/initialization composition, not a
/// no-row observation. This function is deliberately not a dispatcher operation.
pub(super) fn apply_existing_put_in(
    store: &Store,
    command: &CommandEnvelope,
    request: &ContributionReplace,
    expected_public_revision: i64,
    externally_asserted_aggregate_revision: i64,
    source: &MetadataAuthority,
    initialization: &MetadataAuthority,
    extension: &MetadataAuthority,
    tags: &TagAuthority,
    admitted_at_us: i64,
) -> Result<publication::PublishedNfoPut> {
    let current = state::read_in(
        store,
        &request.subject,
        command.expected_generation(),
        externally_asserted_aggregate_revision,
    )?;
    let permit =
        permit::prepare_put_in(store, current, command, request, expected_public_revision)?;
    // The exact outer command remains the source/tag history and receipt key.
    // The metadata owner retains its deferred commit barrier for this corridor.
    catalog_metadata_store::replace_nfo_contribution(
        store,
        request,
        command,
        source,
        initialization,
        extension,
        tags,
        admitted_at_us,
        &permit,
    )
    .map_err(|error| Error(format!("catalog_nfo_put_metadata: {error}")))?;
    let verified = permit.verify_applied_in(store, command)?;
    publication::publish_put_in(store, verified)
}

/// Audited first-source composition. The token owns the exact descriptive
/// pre-state and independent absence proof; missing source rows cannot mint it.
/// Ordinary source, initialization, extension and tag grants remain mandatory.
pub(super) fn apply_first_put_in(
    store: &Store,
    command: &CommandEnvelope,
    request: &ContributionReplace,
    expected_public_revision: i64,
    first: super::first_source::VerifiedFirstNfoAbsence,
    source: &MetadataAuthority,
    initialization: &MetadataAuthority,
    extension: &MetadataAuthority,
    tags: &TagAuthority,
    admitted_at_us: i64,
) -> Result<publication::PublishedNfoPut> {
    let permit = permit::prepare_first_put_in(
        store,
        first,
        command,
        request,
        expected_public_revision,
        initialization,
    )?;
    catalog_metadata_store::replace_nfo_contribution(
        store,
        request,
        command,
        source,
        initialization,
        extension,
        tags,
        admitted_at_us,
        &permit,
    )
    .map_err(|error| Error(format!("catalog_nfo_first_put_metadata: {error}")))?;
    let verified = permit.verify_applied_in(store, command)?;
    publication::publish_put_in(store, verified)
}
