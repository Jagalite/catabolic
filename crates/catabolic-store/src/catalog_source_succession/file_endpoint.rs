//! Authenticated relocation-only endpoint transformation. This is neither a
//! physical-open capability nor a library-membership/read grant. Its caller
//! must independently bind the requested historical reference to the immutable
//! host occurrence, library ACK, query and revision bridge.
use super::{history, library_scope, withdrawal};
use crate::migration::source_history_rows::RowView;
use crate::{
    Error, Result, Store, catalog_identity_mutation::StructuralReadBudget,
    operation_budget::BudgetCharge,
};
use catabolic_core::catalog_identity::ExactFileRevision;
use serde_json::Value;

#[derive(Debug, PartialEq, Eq)]
pub struct FileEndpointRequest {
    exact: ExactFileRevision,
    ctime_ns: i64,
}
impl FileEndpointRequest {
    pub fn new(exact: ExactFileRevision, ctime_ns: i64) -> Result<Self> {
        exact.validate().map_err(|_| refused())?;
        Ok(Self { exact, ctime_ns })
    }
    pub fn exact(&self) -> &ExactFileRevision {
        &self.exact
    }
    pub fn ctime_ns(&self) -> i64 {
        self.ctime_ns
    }
}
/// Immutable facts returned only after complete history and current native
/// occurrence validation. Stable IDs alone never construct this value.
#[derive(Debug, PartialEq)]
pub struct CurrentFileEndpoint {
    request: FileEndpointRequest,
    source: String,
    baseline: Value,
    current: Value,
    current_revision: String,
    current_ctime_ns: i64,
    first_revision: i64,
    last_revision: i64,
    content_sha256: String,
    bytes: i64,
}
impl CurrentFileEndpoint {
    pub fn request(&self) -> &FileEndpointRequest {
        &self.request
    }
    pub fn native_source(&self) -> &str {
        &self.source
    }
    pub fn baseline_snapshot(&self) -> &Value {
        &self.baseline
    }
    pub fn current_snapshot(&self) -> &Value {
        &self.current
    }
    pub fn current_revision(&self) -> &str {
        &self.current_revision
    }
    pub fn current_ctime_ns(&self) -> i64 {
        self.current_ctime_ns
    }
    pub fn first_succession_revision(&self) -> i64 {
        self.first_revision
    }
    pub fn last_succession_revision(&self) -> i64 {
        self.last_revision
    }
    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }
    pub fn bytes(&self) -> i64 {
        self.bytes
    }
}
fn refused() -> Error {
    Error("source file endpoint has no uninterrupted authenticated relocation proof".into())
}
fn equal(a: &Value, b: &Value, budget: &mut impl StructuralReadBudget) -> Result<bool> {
    library_scope::equal_value(a, b, budget)
}
fn string<'a>(v: &'a (impl RowView + ?Sized), key: &str) -> Result<&'a str> {
    v.get_field(key).and_then(Value::as_str).ok_or_else(refused)
}
fn integer(v: &(impl RowView + ?Sized), key: &str) -> Result<i64> {
    v.get_field(key).and_then(Value::as_i64).ok_or_else(refused)
}
fn decoded(
    v: &(impl RowView + ?Sized),
    key: &str,
    b: &mut impl StructuralReadBudget,
) -> Result<Value> {
    let raw = string(v, key)?;
    b.raw_json(raw)?;
    Ok(serde_json::from_str(raw)?)
}
/// Two decoded values moved from the complete verifier, never an admitted
/// endpoint by themselves. They remain private until the existing post-receipt
/// collector boundary. No clones or second JSON parse are performed.
pub(super) struct SelectedOld {
    observation: Value,
    occurrence: Value,
}
impl SelectedOld {
    pub(super) fn admit(
        observation: &Value,
        occurrence: &Value,
        b: &mut impl StructuralReadBudget,
    ) -> Result<()> {
        // The original decode was admitted by complete history capture. Admit
        // the *additional live overlap* through later siblings/receipt checking
        // using observable capacities and the existing bounded map-node model.
        // Charge both this traversal and the capacity measurer before measuring;
        // neither traversal allocates or performs opaque map-key lookups.
        fn walk(v: &Value, depth: usize, b: &mut impl StructuralReadBudget) -> Result<()> {
            if depth > 128 {
                return Err(refused());
            }
            b.charge(BudgetCharge {
                work: 2,
                ..Default::default()
            })?;
            match v {
                Value::Array(a) => {
                    for child in a {
                        walk(child, depth + 1, b)?;
                    }
                }
                Value::Object(o) => {
                    for (_, child) in o {
                        walk(child, depth + 1, b)?;
                    }
                }
                _ => (),
            }
            Ok(())
        }
        walk(&observation, 0, b)?;
        walk(&occurrence, 0, b)?;
        let bytes = crate::migration::retained_value_allocation_bytes(&observation)?
            .checked_add(crate::migration::retained_value_allocation_bytes(
                &occurrence,
            )?)
            .and_then(|n| n.checked_add(std::mem::size_of::<Option<Self>>() as u64))
            .ok_or_else(refused)?;
        b.charge(BudgetCharge {
            bytes: usize::try_from(bytes).map_err(|_| refused())?,
            values: 2,
            // Constructor -> optional slot -> collector destructure. The owned
            // trees do not move; only the two Value headers change locations.
            work: 1 + (3 * std::mem::size_of::<Self>()).div_ceil(64),
            ..Default::default()
        })?;
        Ok(())
    }
    pub(super) fn new(observation: Value, occurrence: Value) -> Self {
        Self {
            observation,
            occurrence,
        }
    }
}
struct Hop {
    snapshot: Value,
    ctime_ns: i64,
    first: i64,
    last: i64,
    digest: String,
    bytes: i64,
}
/// A concrete collector borrowed by the existing complete verifier. No partial
/// result is public; all subsequent records must authenticate before finish.
pub(super) struct Collector {
    request: FileEndpointRequest,
    baseline: Value,
    hop: Option<Hop>,
}
impl Collector {
    pub(super) fn file_id(&self) -> &str {
        &self.request.exact.file_id
    }
    pub(super) fn observe(
        &mut self,
        revision: i64,
        row: Option<&impl RowView>,
        selected: Option<SelectedOld>,
        b: &mut impl StructuralReadBudget,
    ) -> Result<()> {
        // Fixed field lookups are bounded by this compiled row schema; charge
        // their traversal separately from decoded JSON and text comparisons.
        b.charge(BudgetCharge {
            work: 32,
            ..Default::default()
        })?;
        let Some(row) = row else {
            return if self.hop.is_some() || selected.is_some() {
                Err(refused())
            } else {
                Ok(())
            };
        };
        let SelectedOld {
            observation,
            occurrence: old,
        } = selected.ok_or_else(refused)?;
        let old_ctime = integer(row, "old_ctime_ns")?;
        if let Some(previous) = &self.hop {
            if revision != previous.last.checked_add(1).ok_or_else(refused)?
                || old_ctime != previous.ctime_ns
                || !equal(&old, &previous.snapshot, b)?
                || string(&observation, "observation_origin")? != "succession"
                || integer(&observation, "succession_revision")? != previous.last
            {
                return Err(refused());
            }
        } else {
            if !equal(&old, &self.baseline, b)? {
                return Ok(());
            }
            if old_ctime != self.request.ctime_ns {
                return Err(refused());
            }
        }
        let next = decoded(row, "new_occurrence_json", b)?;
        let digest = string(row, "content_sha256")?;
        let bytes = integer(row, "bytes")?;
        let mut compared_bytes = self.file_id().len();
        for (value, key) in [
            (&old, "path"),
            (&next, "path"),
            (&old, "location"),
            (&next, "location"),
        ] {
            compared_bytes = compared_bytes
                .checked_add(string(value, key)?.len())
                .ok_or_else(refused)?;
        }
        compared_bytes = compared_bytes
            .checked_add(string(row, "relative_path")?.len())
            .ok_or_else(refused)?;
        b.charge(BudgetCharge {
            work: 5 + compared_bytes.div_ceil(64),
            ..Default::default()
        })?;

        b.charge(BudgetCharge {
            bytes: 64,
            work: 4 + digest.len().div_ceil(64),
            ..Default::default()
        })?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || bytes < 0
            || integer(&old, "size")? != bytes
            || integer(&next, "size")? != bytes
            || string(&next, "id")? != self.file_id()
            || string(&old, "path")? != string(&next, "path")?
            || string(&old, "location")? != string(&next, "location")?
            || string(row, "relative_path")? != string(&next, "path")?
            || string(row, "disposition")? != "verified_whole_original"
            || self
                .hop
                .as_ref()
                .is_some_and(|p| p.digest != digest || p.bytes != bytes)
        {
            return Err(refused());
        }
        self.hop = Some(Hop {
            snapshot: next,
            ctime_ns: integer(row, "new_ctime_ns")?,
            first: self.hop.as_ref().map_or(revision, |p| p.first),
            last: revision,
            digest: digest.to_owned(),
            bytes,
        });
        Ok(())
    }
}
fn baseline_in(
    store: &Store,
    profile: &str,
    source: &str,
    request: &FileEndpointRequest,
    b: &mut impl StructuralReadBudget,
) -> Result<Value> {
    b.charge(BudgetCharge {
        work: 1,
        ..Default::default()
    })?;
    let mut statement = store.db.prepare("SELECT revision_contract_version,snapshot_json FROM main.catalog_exact_file_revisions WHERE profile=? AND file_id=? AND revision=?")?;
    let mut rows = statement.query(rusqlite::params![
        profile,
        request.exact.file_id,
        request.exact.revision
    ])?;
    let row = rows.next()?.ok_or_else(refused)?;
    let raw = match row.get_ref(1)? {
        rusqlite::types::ValueRef::Text(raw) => raw,
        _ => return Err(refused()),
    };
    b.charge(BudgetCharge {
        bytes: raw.len().checked_add(8).ok_or_else(refused)?,
        work: 2 + raw.len().div_ceil(64),
        values: 2,
        ..Default::default()
    })?;
    if row.get::<_, i64>(0)? != 1 {
        return Err(refused());
    }
    let raw = std::str::from_utf8(raw).map_err(|_| refused())?;
    b.raw_json(raw)?;
    let mut snapshot: Value = serde_json::from_str(raw)?;
    if rows.next()?.is_some() {
        return Err(refused());
    }
    let object = snapshot.as_object_mut().ok_or_else(refused)?;
    b.charge(BudgetCharge {
        work: 2 + object.len().max(1).ilog2() as usize,
        ..Default::default()
    })?;
    if let Some(ctime) = object.remove("ctime_ns") {
        if ctime.as_i64() != Some(request.ctime_ns) {
            return Err(refused());
        }
    }
    if string(&snapshot, "id")? != request.exact.file_id
        || string(&snapshot, "location")? != source
        || history::snapshot_revision(&snapshot, b)? != request.exact.revision
    {
        return Err(refused());
    }
    Ok(snapshot)
}
pub(crate) fn capture_in(
    store: &Store,
    scope: &library_scope::LibraryScopeSnapshot,
    source: &str,
    request: &FileEndpointRequest,
    b: &mut crate::catalog_nfo_attribution::ProofBudget,
) -> Result<CurrentFileEndpoint> {
    capture_inner(store, scope, source, request, None, b)
}
pub(crate) fn capture_fresh_in(
    store: &Store,
    fresh: &crate::migration::EndpointFreshFacts<'_, '_>,
    source: &str,
    request: &FileEndpointRequest,
    b: &mut crate::catalog_nfo_attribution::ProofBudget,
) -> Result<CurrentFileEndpoint> {
    fresh.check(store, b)?;
    capture_inner(store, fresh.scope(), source, request, Some(fresh), b)
}
fn capture_inner(
    store: &Store,
    scope: &library_scope::LibraryScopeSnapshot,
    source: &str,
    request: &FileEndpointRequest,
    fresh: Option<&crate::migration::EndpointFreshFacts<'_, '_>>,
    b: &mut crate::catalog_nfo_attribution::ProofBudget,
) -> Result<CurrentFileEndpoint> {
    if store.schema_version != 41
        || !store.owns_writer_lock()
        || store.db.is_autocommit()
        || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
    {
        return Err(refused());
    }
    // The facade independently revalidates this opaque scope under this exact
    // writer before and after capture. The source must be in its complete set.
    let input_bytes = request
        .exact
        .file_id
        .len()
        .checked_add(request.exact.revision.len())
        .and_then(|n| n.checked_add(source.len()))
        .ok_or_else(refused)?;
    b.charge(BudgetCharge {
        bytes: input_bytes
            .checked_add(3 * std::mem::size_of::<String>())
            .ok_or_else(refused)?,
        values: 4,
        work: 4 + input_bytes.div_ceil(64),
        ..Default::default()
    })?;
    let mut selected = None;
    for candidate in scope.sources() {
        b.charge(BudgetCharge {
            work: 1 + candidate
                .native_source()
                .len()
                .min(source.len())
                .div_ceil(64),
            ..Default::default()
        })?;
        if candidate.native_source() == source {
            if selected.replace(candidate).is_some() {
                return Err(refused());
            }
        }
    }
    let selected = selected.ok_or_else(refused)?;
    request.exact.validate().map_err(|_| refused())?;
    let baseline = baseline_in(store, scope.profile(), source, request, b)?;
    let mut collector = Collector {
        request: FileEndpointRequest {
            exact: request.exact.clone(),
            ctime_ns: request.ctime_ns,
        },
        baseline,
        hop: None,
    };
    let binding =
        history::collect_file_endpoint_in(store, scope.profile(), source, b, &mut collector)?
            .ok_or_else(refused)?;
    if !equal(&binding, selected.binding(), b)? {
        return Err(refused());
    }
    let Hop {
        snapshot: terminal,
        ctime_ns,
        first,
        last,
        digest,
        bytes,
    } = collector.hop.ok_or_else(refused)?;
    // Zero-hop is deliberately unsupported. No current-binding-only fallback.
    let mut snapshot;
    let mut fresh_rows;
    let files = if let Some(fresh) = fresh {
        // Binding, scan policy and generation came from the freshly compared
        // scope under this exact fence. Do not reconstruct/drop another header
        // or compare the same fact to itself. Per-file metadata, identity
        // policy, volume and complete current inventory are still recaptured.
        fresh_rows = withdrawal::capture_endpoint_occurrences_in(store, fresh, source, b)?;
        &mut fresh_rows
    } else {
        snapshot =
            withdrawal::capture_occurrences_admitted_in(store, scope.profile(), source, 0, b)?;
        if snapshot["generation"].as_i64() != Some(scope.generation())
            || !equal(&snapshot["binding"], selected.binding(), b)?
            || !equal(&snapshot["policy"], selected.policy(), b)?
        {
            return Err(refused());
        }
        snapshot["occurrences"].as_array_mut().ok_or_else(refused)?
    };
    let mut index = None;
    for (i, file) in files.iter().enumerate() {
        let id = string(file, "id")?;
        b.charge(BudgetCharge {
            work: 1 + id.len().min(request.exact.file_id.len()).div_ceil(64),
            ..Default::default()
        })?;
        if id == request.exact.file_id {
            if index.replace(i).is_some() {
                return Err(refused());
            }
        }
    }
    let index = index.ok_or_else(refused)?;
    // swap_remove moves the selected/last Value through a fixed temporary. It
    // does not clone their owned trees; admit the bounded slot movement first.
    b.charge(BudgetCharge {
        work: 1 + (3 * std::mem::size_of::<Value>()).div_ceil(64),
        ..Default::default()
    })?;
    let mut current = files.swap_remove(index);
    let object = current.as_object_mut().ok_or_else(refused)?;
    b.charge(BudgetCharge {
        work: 4 + 2 * object.len().max(1).ilog2() as usize,
        ..Default::default()
    })?;
    if object.remove("ctime_ns").and_then(|v| v.as_i64()) != Some(ctime_ns) {
        return Err(Error("source file endpoint terminal ctime differs".into()));
    }
    let current_revision = match object.remove("exact_revision") {
        Some(Value::String(s)) => s,
        _ => return Err(refused()),
    };
    if !equal(&current, &terminal, b)?
        || history::snapshot_revision(&terminal, b)? != current_revision
    {
        return Err(refused());
    }
    require_terminal_origin(
        store,
        scope.profile(),
        source,
        &request.exact.file_id,
        last,
        b,
    )?;
    b.charge(BudgetCharge::default())?;
    Ok(CurrentFileEndpoint {
        request: collector.request,
        source: source.to_owned(),
        baseline: collector.baseline,
        current,
        current_revision,
        current_ctime_ns: ctime_ns,
        first_revision: first,
        last_revision: last,
        content_sha256: digest,
        bytes,
    })
}
fn terminal_origin_refused() -> Error {
    Error("source file endpoint terminal succession provenance differs".into())
}
fn require_terminal_origin(
    store: &Store,
    profile: &str,
    source: &str,
    file: &str,
    revision: i64,
    b: &mut impl StructuralReadBudget,
) -> Result<()> {
    b.charge(BudgetCharge {
        work: 1,
        ..Default::default()
    })?;
    let mut statement = store.db.prepare("SELECT observation_origin,succession_source_id,succession_revision FROM main.observations WHERE profile=? AND file_id=? AND status='present' AND scan_id IS NULL")?;
    let mut rows = statement.query([profile, file])?;
    let row = rows.next()?.ok_or_else(terminal_origin_refused)?;
    for (column, expected) in [(0, "succession"), (1, source)] {
        let raw = match row.get_ref(column)? {
            rusqlite::types::ValueRef::Text(raw) => raw,
            _ => return Err(terminal_origin_refused()),
        };
        b.charge(BudgetCharge {
            bytes: raw.len(),
            work: 1 + raw.len().div_ceil(64),
            values: 1,
            ..Default::default()
        })?;
        if raw != expected.as_bytes() {
            return Err(terminal_origin_refused());
        }
    }
    b.charge(BudgetCharge {
        bytes: 8,
        values: 1,
        work: 1,
        ..Default::default()
    })?;
    if row.get::<_, i64>(2)? != revision || rows.next()?.is_some() {
        return Err(terminal_origin_refused());
    }
    Ok(())
}
pub(crate) fn revalidate_fresh_in(
    store: &Store,
    fresh: &crate::migration::EndpointFreshFacts<'_, '_>,
    old: &CurrentFileEndpoint,
    b: &mut crate::catalog_nfo_attribution::ProofBudget,
) -> Result<()> {
    let current = capture_fresh_in(store, fresh, &old.source, &old.request, b)?;
    if !equal(&old.baseline, &current.baseline, b)? || !equal(&old.current, &current.current, b)? {
        return Err(refused());
    }
    for (a, z) in [
        (old.source.as_str(), current.source.as_str()),
        (
            old.request.exact.file_id.as_str(),
            current.request.exact.file_id.as_str(),
        ),
        (
            old.request.exact.revision.as_str(),
            current.request.exact.revision.as_str(),
        ),
        (
            old.current_revision.as_str(),
            current.current_revision.as_str(),
        ),
        (old.content_sha256.as_str(), current.content_sha256.as_str()),
    ] {
        if !library_scope::equal_text(a, z, b)? {
            return Err(refused());
        }
    }
    b.charge(BudgetCharge {
        work: 5,
        ..Default::default()
    })?;
    if old.request.ctime_ns != current.request.ctime_ns
        || old.current_ctime_ns != current.current_ctime_ns
        || old.first_revision != current.first_revision
        || old.last_revision != current.last_revision
        || old.bytes != current.bytes
    {
        return Err(refused());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[derive(Default)]
    struct Meter {
        work: usize,
        bytes: usize,
    }
    impl StructuralReadBudget for Meter {
        fn charge(&mut self, c: BudgetCharge) -> Result<()> {
            self.work += c.work;
            self.bytes += c.bytes;
            Ok(())
        }
        fn raw_json(&mut self, raw: &str) -> Result<()> {
            self.charge(BudgetCharge {
                bytes: raw.len(),
                work: 1 + raw.len().div_ceil(64),
                ..Default::default()
            })
        }
    }
    fn observe(c: &mut Collector, revision: i64, row: Option<&Value>, b: &mut Meter) -> Result<()> {
        let selected = row
            .map(|row| -> Result<SelectedOld> {
                let observation = decoded(row, "old_observation_json", b)?;
                let occurrence = decoded(row, "old_occurrence_json", b)?;
                SelectedOld::admit(&observation, &occurrence, b)?;
                Ok(SelectedOld::new(observation, occurrence))
            })
            .transpose()?;
        c.observe(revision, row, selected, b)
    }
    fn snapshot(inode: i64) -> Value {
        json!({"id":"file", "location":"media", "path":"part.mkv", "size":4,
            "mtime_ns":7,"device":3,"inode":inode,"status":"present"})
    }
    fn collector() -> Collector {
        Collector {
            request: FileEndpointRequest::new(
                ExactFileRevision {
                    file_id: "file".into(),
                    revision: crate::components::revision(&snapshot(1)),
                },
                11,
            )
            .unwrap(),
            baseline: snapshot(1),
            hop: None,
        }
    }
    fn row(old_inode: i64, new_inode: i64, old_ctime: i64, new_ctime: i64, prior: i64) -> Value {
        json!({"file_id":"file", "relative_path":"part.mkv", "disposition":"verified_whole_original",
            "old_occurrence_json":catabolic_core::encode(&snapshot(old_inode)),
            "new_occurrence_json":catabolic_core::encode(&snapshot(new_inode)),
            "old_ctime_ns":old_ctime,"new_ctime_ns":new_ctime,"bytes":4,"content_sha256":"a".repeat(64),
            "old_observation_json":catabolic_core::encode(&json!({"observation_origin":"succession","succession_revision":prior}))})
    }
    #[test]
    fn endpoint_snapshot_streaming_revision_matches_existing_exact_owner() {
        for value in [
            snapshot(1),
            json!({"unicode":"雪", "escape":"\n\u{1}\"\\",
            "nested":[{},[],null,true,-0.0,1.25], "z":"long".repeat(256)}),
        ] {
            let mut meter = Meter::default();
            assert_eq!(
                history::snapshot_revision(&value, &mut meter).unwrap(),
                crate::components::revision(&value)
            );
            assert!(meter.bytes >= 64 && meter.work > 0);
        }
        assert!(
            history::snapshot_revision(&json!({"ctime_ns":11}), &mut Meter::default()).is_err()
        );
    }
    #[test]
    fn selected_payload_absence_and_retention_are_explicit() {
        let mut c = collector();
        let mut b = Meter::default();
        c.observe(1, None::<&Value>, None, &mut b).unwrap();
        assert!(
            c.hop.is_none(),
            "an absent historical file is not an endpoint"
        );
        let row = row(1, 2, 11, 12, 0);
        assert!(c.observe(1, Some(&row), None, &mut b).is_err());
        let observation = decoded(&row, "old_observation_json", &mut b).unwrap();
        let occurrence = decoded(&row, "old_occurrence_json", &mut b).unwrap();
        let before = b.bytes;
        SelectedOld::admit(&observation, &occurrence, &mut b).unwrap();
        assert!(b.bytes - before >= std::mem::size_of::<Option<SelectedOld>>());
        assert!(
            c.observe(
                1,
                None::<&Value>,
                Some(SelectedOld::new(observation, occurrence)),
                &mut b
            )
            .is_err()
        );
        observe(&mut c, 1, Some(&row), &mut b).unwrap();
        assert!(
            c.observe(2, None::<&Value>, None, &mut b).is_err(),
            "absence cannot silently interrupt a retained chain"
        );
    }
    #[test]
    fn endpoint_collector_keeps_exact_two_hop_snapshot_and_separate_ctime() {
        let mut c = collector();
        let mut b = Meter::default();
        observe(&mut c, 1, Some(&row(1, 2, 11, 12, 0)), &mut b).unwrap();
        observe(&mut c, 2, Some(&row(2, 3, 12, 13, 1)), &mut b).unwrap();
        let hop = c.hop.unwrap();
        assert_eq!((hop.first, hop.last, hop.ctime_ns), (1, 2, 13));
        assert_eq!(hop.snapshot, snapshot(3));
        assert_eq!(hop.bytes, 4);
    }
    #[test]
    fn endpoint_collector_refuses_interrupted_scan_ctime_content_and_missing_hops() {
        // Controlled reducer negatives, not fabricated authenticated history or
        // an admitted Completed scan. Full-owner positives live in observation.
        for case in 0..8 {
            let mut c = collector();
            let mut b = Meter::default();
            observe(&mut c, 1, Some(&row(1, 2, 11, 12, 0)), &mut b).unwrap();
            let mut next = row(2, 3, 12, 13, 1);
            match case {
                0 => next["old_ctime_ns"] = json!(99),
                1 => next["old_occurrence_json"] = json!(catabolic_core::encode(&snapshot(99))),
                2 => {
                    next["old_observation_json"] =
                        json!("{\"observation_origin\":\"scan\",\"succession_revision\":1}")
                }
                3 => {
                    next["old_observation_json"] =
                        json!("{\"observation_origin\":\"succession\",\"succession_revision\":0}")
                }
                4 => next["content_sha256"] = json!("b".repeat(64)),
                5 => next["bytes"] = json!(5),
                _ => (),
            }
            assert!(
                observe(
                    &mut c,
                    if case == 6 { 3 } else { 2 },
                    if case == 7 { None } else { Some(&next) },
                    &mut b
                )
                .is_err(),
                "case {case}"
            );
        }
        let mut c = collector();
        assert!(
            observe(
                &mut c,
                1,
                Some(&row(1, 2, 99, 12, 0)),
                &mut Meter::default()
            )
            .is_err()
        );
    }
}

#[cfg(test)]
pub(crate) fn assert_fresh_occurrence_parity_in(
    store: &Store,
    fresh: &crate::migration::EndpointFreshFacts<'_, '_>,
    source: &str,
    b: &mut crate::catalog_nfo_attribution::ProofBudget,
) -> Result<()> {
    let original =
        withdrawal::capture_occurrences_admitted_in(store, fresh.scope().profile(), source, 0, b)?;
    let rows = withdrawal::capture_endpoint_occurrences_in(store, fresh, source, b)?;
    assert_eq!(original["generation"], fresh.scope().generation());
    let selected = fresh
        .scope()
        .sources()
        .iter()
        .find(|s| s.native_source() == source)
        .unwrap();
    assert_eq!(&original["binding"], selected.binding());
    assert_eq!(&original["policy"], selected.policy());
    assert_eq!(original["occurrences"].as_array().unwrap(), &rows);
    // This complete output equality includes separate ctime, exact revisions,
    // and each file's identity policy/volume UUID, not merely source headers.
    fresh.check(store, b)
}
