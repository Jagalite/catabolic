//! Independent bounded native database-image inspection. No writer, restore,
//! incarnation-rotation, worker-cleanup or effect authority is returned.
use super::*;

/// Descriptive binding-root identity copied from the independently verified image.
pub struct BindingRoot {
    path: String,
    device: Option<u64>,
    inode: Option<u64>,
}
impl BindingRoot {
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn identity(&self) -> Option<(u64, u64)> {
        self.device.zip(self.inode)
    }
}
pub struct InspectedImage {
    binding_roots: Vec<BindingRoot>,
    catalog_id: String,
    incarnation: String,
    generation: i64,
    schema: u32,
    bytes: u64,
    sha256: String,
    inventory: inventory::Audit,
    worker_history:
        std::result::Result<crate::catalog_restore_lineage::PreparedWorkerHistory, String>,
}
impl InspectedImage {
    pub fn binding_roots(&self) -> &[BindingRoot] {
        &self.binding_roots
    }
    /// Historical closure preparation only; unresolved retained evidence stays
    /// an explicit obligation and never becomes a current cleanup witness.
    pub fn worker_history(&self) -> Result<&crate::catalog_restore_lineage::PreparedWorkerHistory> {
        self.worker_history
            .as_ref()
            .map_err(|error| Error(error.clone()))
    }
    pub fn catalog_id(&self) -> &str {
        &self.catalog_id
    }
    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }
    pub fn generation(&self) -> i64 {
        self.generation
    }
    pub fn schema_version(&self) -> u32 {
        self.schema
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn inventory_sha256(&self) -> &str {
        &self.inventory.complete
    }
    pub fn migration_ledger_sha256(&self) -> &str {
        &self.inventory.ledger
    }
}
/// `reserved` only tightens the fixed accounting cap for other actually retained
/// owner allocations. Input is read into an isolated SQLite image, never opened
/// by pathname or interpreted as permission to restore a catalog.
pub fn inspect<R: Read>(
    input: R,
    bytes: u64,
    reserved: u64,
    cancelled: &AtomicBool,
) -> Result<InspectedImage> {
    if bytes == 0 || bytes > 56 * 1024 * 1024 {
        return Err(fail("native inspection image cohort exceeded"));
    }
    // Root strings/typed rows remain live alongside the full audit. Charge a
    // separate reserve rather than silently spending the existing audit reserve.
    let reserved = reserved
        .checked_add(512 * 1024)
        .ok_or_else(|| fail("native inspection root accounting overflow"))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    live(cancelled, deadline)?;
    charge_reserved(bytes, reserved)?;
    let mut db = Connection::open_in_memory()?;
    db.pragma_update(None, "cache_size", -2048)?;
    let account = |db: &Connection| -> Result<()> {
        let used = bytes
            .checked_mul(4)
            .and_then(|n| n.checked_add(METADATA_RESERVE))
            .and_then(|n| n.checked_add(reserved))
            .and_then(|n| n.checked_add(allocations(db).ok()?));
        if used.is_none_or(|n| n > ACCOUNTING_LIMIT) {
            return Err(fail("native inspection allocation budget exhausted"));
        }
        live(cancelled, deadline)
    };
    account(&db)?;
    let length = usize::try_from(bytes).map_err(|_| fail("native image length overflow"))?;
    db.deserialize_read_exact("main", input, length, true)?;
    db.pragma_update(None, "foreign_keys", true)?;
    db.pragma_update(None, "recursive_triggers", true)?;
    db.pragma_update(None, "read_uncommitted", false)?;
    account(&db)?;
    db.progress_handler(10000, Some(move || Instant::now() >= deadline))?;
    let schema = crate::migration::validate(&db, true)?["schema"]
        .as_u64()
        .ok_or_else(|| fail("native inspected schema missing"))?;
    let schema = u32::try_from(schema).map_err(|_| fail("native schema overflow"))?;
    if !crate::migration::is_supported_native_schema(schema) {
        return Err(fail("unsupported native inspected schema"));
    }
    account(&db)?;
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok"
        || db
            .prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_some()
    {
        return Err(fail("native inspected image integrity failed"));
    }
    let (catalog_id, incarnation, generation): (String, String, i64) = db.query_row(
        "SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if generation < 0 {
        return Err(fail("negative inspected native generation"));
    }
    account(&db)?;
    // Retained descriptive target-disjointness evidence has its own small bound;
    // absent device/inode is retained as missing proof, never inferred disjoint.
    let (root_count, root_bytes): (i64, i64) = db.query_row(
        "SELECT count(*),coalesce(sum(length(CAST(root AS BLOB))),0) FROM bindings",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if !(0..=1024).contains(&root_count) || !(0..=262144).contains(&root_bytes) {
        return Err(fail("restore binding-root inventory cohort exceeded"));
    }
    let mut roots =
        db.prepare("SELECT root,device,inode FROM bindings ORDER BY root,device,inode")?;
    let binding_roots = roots
        .query_map([], |r| {
            Ok(BindingRoot {
                path: r.get(0)?,
                device: optional_nonnegative(r, 1)?,
                inode: optional_nonnegative(r, 2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(roots);
    let inventory = inventory::capture(&db, cancelled, deadline)?;
    account(&db)?;
    let data = db.serialize("main")?;
    account(&db)?;
    if data.len() != length {
        return Err(fail("native inspected image length changed"));
    }
    let sha256 = catabolic_core::sha256(&*data);
    let worker_history = crate::catalog_restore_lineage::prepare_in(
        &db,
        schema,
        &catalog_id,
        &incarnation,
        cancelled,
    )
    .map_err(|e| e.0);
    account(&db)?;
    live(cancelled, deadline)?;
    Ok(InspectedImage {
        binding_roots,
        catalog_id,
        incarnation,
        generation,
        schema,
        bytes,
        sha256,
        inventory,
        worker_history,
    })
}
