//! Restricted host facade. This API never returns a Store, Connection, legacy
//! operator HostSession, or general-purpose embedding Session.
use catabolic_core::catalog_host_lease::AuthorizationLease;
pub use catabolic_store::catalog_scope::{
    LeasedRead, ReadBudget, SavedQueryRef, ScopeError, ScopeErrorCode, ScopeReadRequest,
    TrustedHostReadGrant, TrustedSelection,
};
use catabolic_store::catalog_scope::{ScopeHandle, ScopedReadSession};
use std::path::Path;
/// An opaque handle can only be consumed by the session which admitted it.
pub struct HostScopeHandle(ScopeHandle);
pub struct ScopedHostSession {
    inner: ScopedReadSession,
}
impl ScopedHostSession {
    pub fn open(path: &Path) -> Result<Self, ScopeError> {
        Ok(Self {
            inner: ScopedReadSession::open(path)?,
        })
    }
    pub fn admit_host_scope(
        &mut self,
        grant: TrustedHostReadGrant,
        lease: &AuthorizationLease,
        budget: ReadBudget,
    ) -> Result<HostScopeHandle, ScopeError> {
        self.inner
            .admit_host_scope(grant, lease, budget)
            .map(HostScopeHandle)
    }
    pub fn read_host_scope(
        &mut self,
        handle: &HostScopeHandle,
        lease: AuthorizationLease,
        request: ScopeReadRequest,
        budget: ReadBudget,
    ) -> Result<LeasedRead, ScopeError> {
        self.inner
            .read_host_scope(&handle.0, lease, request, budget)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use catabolic_core::catalog_host_lease::{HostAuthorization, LeaseError};
    use catabolic_store::{Store, curation, definitions, migration};
    use serde_json::json;
    use std::time::Duration;
    #[test]
    fn facade_delivers_only_scoped_values_while_host_lease_is_held() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.sqlite3");
        migration::initialize_native_at(&path, 34).unwrap();
        let writer = Store::open(&path, true, false).unwrap();
        curation::item_put(
            &writer,
            &json!({"id":"a","kind":"video","metadata":{"title":"Visible","secret":"hidden"}}),
        )
        .unwrap();
        let query=definitions::query_put(&writer,"default","visible",&json!({"version":1,"mode":"selection","selection":{"language":"sql","query":"SELECT item_id FROM catalog_items WHERE item_id='a'"}})).unwrap();
        let reference = SavedQueryRef::new(
            query["id"].as_str().unwrap().into(),
            query["revision"].as_i64().unwrap(),
            query["digest"].as_str().unwrap().into(),
        )
        .unwrap();
        let host = HostAuthorization::new("runtime-host".into(), "opaque-revision".into()).unwrap();
        let budget = ReadBudget::new(Duration::from_secs(5), 100).unwrap();
        let grant = TrustedHostReadGrant::new(
            host.clone(),
            "default".into(),
            vec![TrustedSelection::new(reference, vec!["title".into()]).unwrap()],
            vec![],
        )
        .unwrap();
        let mut session = ScopedHostSession::open(&path).unwrap();
        let handle = session
            .admit_host_scope(grant, &host.acquire(host.revision()).unwrap(), budget)
            .unwrap();
        let output = session
            .read_host_scope(
                &handle,
                host.acquire(host.revision()).unwrap(),
                ScopeReadRequest::GetItem("a".into()),
                budget,
            )
            .unwrap();
        let revoked = host.begin_revocation().unwrap();
        output.deliver(|value| {
            assert!(!value.to_string().contains("hidden"));
            assert_eq!(
                revoked.wait_drained(Duration::ZERO),
                Err(LeaseError::Timeout)
            );
        });
        assert_eq!(revoked.wait_drained(Duration::ZERO), Ok(()));
    }
}
#[cfg(test)]
#[test]
fn existing_and_scoped_host_owners_remain_send() {
    fn assert_send<T: Send>() {}
    assert_send::<crate::host::HostSession>();
    assert_send::<ScopedHostSession>();
    assert_send::<HostScopeHandle>();
}
