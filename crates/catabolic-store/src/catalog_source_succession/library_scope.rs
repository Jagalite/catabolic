//! Fixed descriptive library/current-source snapshot. It does not mint a saved
//! query grant, establish Motion revision bridges, or admit ordinary schema41.
//! Current item JSON and its permanent head retain the existing library-read
//! projection contract. This is not an independent contributor-resolution or
//! imported-metadata lineage proof.
use crate::{
    Error, Result, Store, catalog_identity_mutation::StructuralReadBudget,
    catalog_nfo_attribution::ProofBudget, catalog_scope::SavedQueryRef,
    operation_budget::BudgetCharge,
};
use serde_json::{Value, json};

#[derive(Debug, PartialEq, Eq)]
pub struct LibraryScopeSource {
    motion_source_id: String,
    native_source: String,
}
impl LibraryScopeSource {
    pub fn new(motion_source_id: String, native_source: String) -> Result<Self> {
        if motion_source_id.is_empty() || motion_source_id.len() > 256 {
            return Err(refused());
        }
        crate::query::name(&native_source)?;
        Ok(Self {
            motion_source_id,
            native_source,
        })
    }
    pub fn motion_source_id(&self) -> &str {
        &self.motion_source_id
    }
    pub fn native_source(&self) -> &str {
        &self.native_source
    }
}
#[derive(Debug, PartialEq)]
pub struct LibraryScopeSourceFacts {
    selector: LibraryScopeSource,
    binding: Value,
    policy: Value,
}
impl LibraryScopeSourceFacts {
    pub fn motion_source_id(&self) -> &str {
        self.selector.motion_source_id()
    }
    pub fn native_source(&self) -> &str {
        self.selector.native_source()
    }
    pub fn binding(&self) -> &Value {
        &self.binding
    }
    pub fn policy(&self) -> &Value {
        &self.policy
    }
}
/// Opaque descriptive output from a fixed native owner, not authorization.
#[derive(Debug, PartialEq)]
pub struct LibraryScopeSnapshot {
    catalog_id: String,
    incarnation: String,
    generation: i64,
    profile: String,
    library_id: String,
    item_revision: i64,
    metadata: Value,
    query: SavedQueryRef,
    sources: Vec<LibraryScopeSourceFacts>,
}
impl LibraryScopeSnapshot {
    pub fn catalog_id(&self) -> &str {
        &self.catalog_id
    }
    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }
    pub fn generation(&self) -> i64 {
        self.generation
    }
    pub fn profile(&self) -> &str {
        &self.profile
    }
    pub fn library_id(&self) -> &str {
        &self.library_id
    }
    pub fn item_revision(&self) -> i64 {
        self.item_revision
    }
    pub fn definition(&self) -> &Value {
        &self.metadata["motion_library"]
    }
    pub fn metadata(&self) -> &Value {
        &self.metadata
    }
    pub fn query(&self) -> &SavedQueryRef {
        &self.query
    }
    pub fn sources(&self) -> &[LibraryScopeSourceFacts] {
        &self.sources
    }
}
fn refused() -> Error {
    Error("fixed native library scope differs or is unsupported".into())
}
fn charge(
    b: &mut impl StructuralReadBudget,
    bytes: usize,
    values: usize,
    work: usize,
) -> Result<()> {
    b.charge(BudgetCharge {
        bytes,
        values,
        work,
        ..Default::default()
    })
}
// The two exact selected source relations are preflighted before their existing
// owner materializes rows/JSON. Plain SQL text allows six-byte JSON escaping;
// JSON policies reserve both parse and retained producer-clone representations.
fn preflight_source(
    store: &Store,
    profile: &str,
    source: &str,
    b: &mut impl StructuralReadBudget,
) -> Result<()> {
    for sql in [
        "SELECT * FROM main.bindings WHERE profile=? AND kind='source' AND owner=?",
        "SELECT * FROM main.source_observation_policies WHERE profile=? AND source=?",
    ] {
        charge(b, 0, 1, 1 + sql.len().div_ceil(64))?;
        let mut stmt = store.db.prepare(sql)?;
        let cols = stmt.column_count();
        let mut rows = stmt.query([profile, source])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            count += 1;
            if count > 1 {
                return Err(refused());
            }
            charge(b, cols * 512, cols * 2, cols * 2)?;
            for col in 0..cols {
                match row.get_ref(col)? {
                    rusqlite::types::ValueRef::Text(raw) => {
                        let raw = std::str::from_utf8(raw).map_err(|_| refused())?;
                        charge(
                            b,
                            raw.len().checked_mul(8).ok_or_else(refused)?,
                            2,
                            2 + 2 * raw.len().div_ceil(64),
                        )?;
                        if row.as_ref().column_name(col)? == "policy" {
                            b.raw_json(raw)?;
                            b.raw_json(raw)?;
                        }
                    }
                    rusqlite::types::ValueRef::Blob(_) => return Err(refused()),
                    _ => {}
                }
            }
        }
    }
    charge(b, 4096 + 4 * (profile.len() + source.len()), 64, 64)?;
    Ok(())
}
/// Closed caller must already hold its actual write transaction and budget.
/// This core is shared by ordinary40 host and genuine Completed41 wrappers.
pub(crate) fn capture_in(
    store: &Store,
    profile: &str,
    library: &str,
    reference: &SavedQueryRef,
    selectors: &[LibraryScopeSource],
    b: &mut impl StructuralReadBudget,
) -> Result<LibraryScopeSnapshot> {
    if !store.owns_writer_lock()
        || store.db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
        || !matches!(store.schema_version, 40 | 41)
        || library.is_empty()
        || library.len() > 256
        || selectors.len() > 100
    {
        return Err(refused());
    }
    crate::query::name(profile)?;
    let mut input_bytes = profile.len()
        + library.len()
        + store.database_id.len()
        + store
            .catalog_incarnation
            .as_ref()
            .ok_or_else(refused)?
            .len();
    for (i, s) in selectors.iter().enumerate() {
        crate::query::name(s.native_source())?;
        input_bytes = input_bytes
            .checked_add(s.motion_source_id.len() + s.native_source.len())
            .ok_or_else(refused)?;
        charge(b, 0, 0, 1 + i * (1 + 256usize.div_ceil(64)))?;
        if selectors[..i]
            .iter()
            .any(|v| v.motion_source_id == s.motion_source_id || v.native_source == s.native_source)
        {
            return Err(refused());
        }
    }
    charge(
        b,
        input_bytes * 3
            + selectors.len()
                * (std::mem::size_of::<LibraryScopeSourceFacts>() + std::mem::size_of::<&str>())
            + 2048,
        selectors.len() * 3 + 12,
        12 + input_bytes.div_ceil(64),
    )?;
    let item_id = format!("motion.library:{library}");
    let sql = "SELECT i.metadata,(SELECT revision FROM main.catalog_entity_revisions WHERE entity_kind='items' AND entity_key=json_array(i.id) AND deleted=0), EXISTS(SELECT 1 FROM main.identities WHERE item_id=i.id) OR EXISTS(SELECT 1 FROM main.item_files WHERE item_id=i.id) OR EXISTS(SELECT 1 FROM main.item_relationships WHERE source_id=i.id OR target_id=i.id) OR EXISTS(SELECT 1 FROM main.catalog_timelines WHERE edition_item_id=i.id) OR EXISTS(SELECT 1 FROM main.catalog_versions WHERE association_item_id=i.id) OR EXISTS(SELECT 1 FROM main.catalog_logical_retirements WHERE item_id=i.id) FROM main.items i WHERE i.id=? AND i.kind='custom:motion_library'";
    charge(b, 0, 3, 1 + sql.len().div_ceil(64))?;
    let mut stmt = store.db.prepare(sql)?;
    let mut rows = stmt.query([&item_id])?;
    let row = rows.next()?.ok_or_else(refused)?;
    let raw = row.get_ref(0)?.as_str().map_err(|_| refused())?;
    b.raw_json(raw)?;
    let metadata: Value = serde_json::from_str(raw)?;
    let item_revision: Option<i64> = row.get(1)?;
    let rich: bool = row.get(2)?;
    if rich || item_revision.is_none_or(|n| n < 1) || rows.next()?.is_some() {
        return Err(refused());
    }
    let object = metadata.as_object().ok_or_else(refused)?;
    let definition = &metadata["motion_library"];
    if object.keys().any(|k| k != "title" && k != "motion_library")
        || definition["contract_version"] != 1
        || definition["id"] != library
        || !definition["catalog_revision"]
            .as_i64()
            .is_some_and(|n| n > 0)
    {
        return Err(refused());
    }
    let members = definition["source_ids"].as_array().ok_or_else(refused)?;
    if members.len() != selectors.len() {
        return Err(refused());
    }
    for (i, member) in members.iter().enumerate() {
        let id = member.as_str().ok_or_else(refused)?;
        charge(
            b,
            0,
            1,
            1 + (i + selectors.len()) * (1 + id.len().max(256).div_ceil(64)),
        )?;
        if members[..i].iter().any(|v| v.as_str() == Some(id))
            || !selectors.iter().any(|s| s.motion_source_id == id)
        {
            return Err(refused());
        }
    }
    let mut native = Vec::with_capacity(selectors.len());
    for selector in selectors {
        native.push(selector.native_source());
    }
    crate::catalog_scope::validate_library_selection_in(store, profile, reference, &native, b)?;
    let mut sources = Vec::with_capacity(selectors.len());
    for s in selectors {
        preflight_source(store, profile, s.native_source(), b)?;
        let binding = crate::observation::binding(store, profile, "source", s.native_source())?;
        let policy = crate::observation::execute(
            store,
            profile,
            "scan_policy",
            &json!({"source":s.native_source()}),
        )?;
        sources.push(LibraryScopeSourceFacts {
            selector: LibraryScopeSource {
                motion_source_id: s.motion_source_id.clone(),
                native_source: s.native_source.clone(),
            },
            binding,
            policy,
        });
    }
    charge(b, 8, 1, 2)?;
    let generation: i64 = store.db.query_row(
        "SELECT generation FROM main.catalog_state WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    store
        .catalog_operation_budget()?
        .check()
        .map_err(|_| refused())?;
    Ok(LibraryScopeSnapshot {
        catalog_id: store.database_id.clone(),
        incarnation: store.catalog_incarnation.clone().ok_or_else(refused)?,
        generation,
        profile: profile.into(),
        library_id: library.into(),
        item_revision: item_revision.ok_or_else(refused)?,
        metadata,
        query: reference.clone(),
        sources,
    })
}
/// The ordinary host adapter must retain its existing authorization. This
/// descriptive entrypoint cannot open/admit schema41 or return a native grant.
pub fn capture_host_library_scope(
    store: &Store,
    profile: &str,
    library: &str,
    reference: &SavedQueryRef,
    selectors: &[LibraryScopeSource],
) -> Result<LibraryScopeSnapshot> {
    if store.schema_version != 40 {
        return Err(refused());
    }
    store.transaction_detailed(|_| {
        let mut budget = ProofBudget::new(store.catalog_operation_budget()?);
        capture_in(store, profile, library, reference, selectors, &mut budget)
    })
}

#[path = "library_scope/equality.rs"]
mod equality;
pub(super) use equality::{equal_text, equal_value};

pub(crate) fn revalidate_in(
    store: &Store,
    old: &LibraryScopeSnapshot,
    b: &mut impl StructuralReadBudget,
) -> Result<()> {
    capture_revalidated_in(store, old, b).map(drop)
}
/// Return the already-admitted fresh facts instead of dropping them. This is
/// still descriptive; only the fixed endpoint fence permits operation-local reuse.
pub(crate) fn capture_revalidated_in(
    store: &Store,
    old: &LibraryScopeSnapshot,
    b: &mut impl StructuralReadBudget,
) -> Result<LibraryScopeSnapshot> {
    let bytes = old.sources.iter().try_fold(0usize, |n, s| {
        n.checked_add(
            s.selector.motion_source_id.len()
                + s.selector.native_source.len()
                + std::mem::size_of::<LibraryScopeSource>(),
        )
        .ok_or_else(refused)
    })?;
    charge(b, bytes, old.sources.len() * 2, 1 + bytes.div_ceil(64))?;
    let mut selectors = Vec::with_capacity(old.sources.len());
    for source in &old.sources {
        selectors.push(LibraryScopeSource {
            motion_source_id: source.selector.motion_source_id.clone(),
            native_source: source.selector.native_source.clone(),
        });
    }
    let now = capture_in(
        store,
        old.profile(),
        old.library_id(),
        old.query(),
        &selectors,
        b,
    )?;
    // Every tree is traversed once by the actual charged comparison. There is
    // no subsequent derived whole-snapshot equality re-traversing those trees.
    if !equal_value(&old.metadata, &now.metadata, b)? {
        return Err(refused());
    }
    charge(b, 0, 0, 4)?;
    if old.generation != now.generation
        || old.item_revision != now.item_revision
        || old.sources.len() != now.sources.len()
    {
        return Err(refused());
    }
    for (a, z) in [
        (old.catalog_id.as_str(), now.catalog_id.as_str()),
        (old.incarnation.as_str(), now.incarnation.as_str()),
        (old.profile.as_str(), now.profile.as_str()),
        (old.library_id.as_str(), now.library_id.as_str()),
    ] {
        if !equal_text(a, z, b)? {
            return Err(refused());
        }
    }
    if !crate::catalog_scope::equal_saved_library_reference(&old.query, &now.query, b)? {
        return Err(refused());
    }
    for (a, z) in old.sources.iter().zip(&now.sources) {
        if !equal_text(a.motion_source_id(), z.motion_source_id(), b)?
            || !equal_text(a.native_source(), z.native_source(), b)?
            || !equal_value(&a.binding, &z.binding, b)?
            || !equal_value(&a.policy, &z.policy, b)?
        {
            return Err(refused());
        }
    }
    Ok(now)
}
