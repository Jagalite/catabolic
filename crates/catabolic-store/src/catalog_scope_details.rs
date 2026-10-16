//! Small, database-only detail adapters. Selection labels are only those the
//! exact capability admits. Provenance is field-scoped current owner data;
//! opaque source evidence and unscoped schema/input fingerprints never escape.
use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct TrustedProvenanceRead {
    pub(super) fields: BTreeSet<String>,
}
impl TrustedProvenanceRead {
    /// Explicit permission for current values, conflicts and source revisions
    /// contributing to these fields. Metadata/search grants do not imply this.
    pub fn new(fields: Vec<String>) -> Result<Self> {
        if fields.is_empty() || fields.len() > 64 {
            return Err(fail(ScopeErrorCode::InvalidGrant));
        }
        crate::access_admin::validate_host_metadata_fields(&fields)
            .map_err(|_| fail(ScopeErrorCode::InvalidGrant))?;
        Ok(Self {
            fields: fields.into_iter().collect(),
        })
    }
}

pub(super) enum Request {
    Memberships(String),
    Provenance(String),
    FileFacts(String),
    Components(String),
}
impl Request {
    fn item(&self) -> &str {
        match self {
            Self::Memberships(item)
            | Self::Provenance(item)
            | Self::FileFacts(item)
            | Self::Components(item) => item,
        }
    }
}
pub(super) fn read(
    store: &Store,
    cap: &Capability,
    fields: &BTreeMap<String, MemberVisibility>,
    request: Request,
    started: Instant,
    budget: ReadBudget,
    poisoned: &Cell<bool>,
) -> Result<Value> {
    let item = request.item();
    if item.is_empty() || item.len() > 1024 {
        return Err(fail(ScopeErrorCode::InvalidRequest));
    }
    let item = item.to_owned();
    let mask = fields
        .get(&item)
        .ok_or_else(|| fail(ScopeErrorCode::NotFoundOrNotVisible))?;
    let authorized = match &request {
        Request::Memberships(_) => !mask.memberships.is_empty(),
        Request::Provenance(_) => !mask.provenance.fields.is_empty(),
        Request::FileFacts(_) => !mask.file_facts.by_role.is_empty(),
        Request::Components(_) => !mask.components.by_role.is_empty(),
    };
    if !authorized {
        return Err(fail(ScopeErrorCode::NotFoundOrNotVisible));
    }
    let work = OperationBudget::current().ok_or_else(|| fail(ScopeErrorCode::Catalog))?;
    let vm = search::OwnedVm::install(&store.db, &work, poisoned)?;
    let result = (|| {
        remaining(started, budget)?;
        if !store
            .db
            .prepare("SELECT 1 FROM main.catalog_live_items WHERE id=?")?
            .exists([&item])?
        {
            return Err(fail(ScopeErrorCode::NotFoundOrNotVisible));
        }
        let mut value = match request {
            Request::Components(item) => {
                component_reads::read_components(store, cap, fields, &item, started, budget)?
            }
            Request::FileFacts(item) => {
                physical::file_facts(store, cap, &item, &mask.file_facts, started, budget)?
            }
            Request::Memberships(item) => {
                memberships(store, cap, &item, &mask.memberships, started, budget)?
            }
            Request::Provenance(item) => {
                let authority = crate::catalog_metadata_store::MetadataReadAuthority {
                    catalog_id: cap.state.catalog.clone(),
                    incarnation: cap.state.incarnation.clone(),
                    subject: item,
                    expected_generation: cap.state.generation,
                };
                crate::catalog_metadata_store::read_metadata_provenance_in_snapshot(
                    &store.db,
                    &authority,
                    &mask.provenance.fields,
                    &work,
                )
                .map_err(|error| {
                    use crate::catalog_metadata_store::MetadataStoreErrorCode as E;
                    use catabolic_core::catalog_metadata::MetadataError as M;
                    fail(match error.code {
                        E::NotInitialized => ScopeErrorCode::MetadataNotInitialized,
                        E::StaleGeneration | E::CatalogMismatch | E::IncarnationMismatch => {
                            ScopeErrorCode::StaleScope
                        }
                        E::Core(M::SourceBudget | M::FieldBudget) => ScopeErrorCode::IdBudget,
                        E::Core(
                            M::DocumentBudget
                            | M::EvidenceBudget
                            | M::SnapshotBudget
                            | M::JsonDepthBudget
                            | M::JsonNodeBudget,
                        ) => ScopeErrorCode::ByteBudget,
                        _ => ScopeErrorCode::Catalog,
                    })
                })?
            }
        };
        value["scope"] = json!(cap.fingerprint);
        let bytes = encode(&value).len();
        if bytes > MAX_BYTES {
            return Err(fail(ScopeErrorCode::ByteBudget));
        }
        shared_charge(BudgetCharge {
            bytes,
            values: 1,
            work: bytes / 64 + 1,
            ..Default::default()
        })?;
        remaining(started, budget)?;
        Ok(value)
    })();
    vm.finish()?;
    if let Some(cause) = work.failure() {
        return Err(fail(evaluation_code(Some(cause))));
    }
    result
}
fn memberships(
    store: &Store,
    cap: &Capability,
    item: &str,
    ids: &BTreeSet<String>,
    started: Instant,
    budget: ReadBudget,
) -> Result<Value> {
    let mut memberships = Vec::new();
    for id in ids {
        remaining(started, budget)?;
        // No dependency or other saved selection can become an output label.
        if !cap
            .selections
            .iter()
            .any(|s| s.membership_read && s.query.id == *id)
        {
            return Err(fail(ScopeErrorCode::WrongAuthority));
        }
        let length: i64 = store.db.query_row(
            "SELECT length(CAST(name AS BLOB)) FROM main.saved_queries WHERE id=? AND profile=?",
            params![id, cap.profile],
            |r| r.get(0),
        )?;
        if !(0..=1024).contains(&length) {
            return Err(fail(ScopeErrorCode::ByteBudget));
        }
        shared_charge(BudgetCharge {
            items: 1,
            bytes: length as usize + id.len(),
            values: 2,
            work: 1,
        })?;
        let name: String = store.db.query_row("SELECT name FROM main.saved_queries WHERE id=? AND profile=? AND length(CAST(name AS BLOB))<=1024", params![id,cap.profile], |r|r.get(0))?;
        memberships.push(json!({"selection_id":id,"name":name}));
    }
    Ok(
        json!({"contract_version":1,"item_id":item,"scoped_total":memberships.len(),"memberships":memberships}),
    )
}
