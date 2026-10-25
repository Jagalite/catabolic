//! Closed, bounded lineage decoding on an independently admitted staged image.
//! A decoded ancestor authenticates history only, never present worker cleanup.
use super::*;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Origin {
    pub contract: u32,
    pub restore_id: String,
    pub catalog_id: String,
    pub schema_version: u32,
    pub from_incarnation: String,
    pub to_incarnation: String,
    pub parent_restore_id: Option<String>,
    pub pair_sha256: String,
    pub native_inventory_sha256: String,
    pub motion_binding_sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Worker {
    pub contract: u32,
    pub restore_id: String,
    pub owner_kind: String,
    pub owner_id: String,
    pub origin_incarnation: String,
    pub tuple_sha256: String,
}
pub(super) struct Chain {
    pub origins: Vec<Origin>,
    pub workers: BTreeMap<(String, String, String), Worker>,
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.contains('\0')
}
fn canonical<T: serde::de::DeserializeOwned + Serialize>(raw: &str) -> Result<T> {
    let decoded: T = serde_json::from_str(raw)?;
    if serde_json::to_string(&decoded)? != raw {
        return Err(fail("restore lineage proof is not canonical"));
    }
    Ok(decoded)
}
/// Every retained hop and worker proof is accounted for. An orphan, fork,
/// foreign identity, oversized closure or missing predecessor is an error.
pub(super) fn read(
    db: &Connection,
    catalog: &str,
    current: &str,
    cancel: &AtomicBool,
) -> Result<Chain> {
    check(cancel)?;
    let (origins, workers, bytes): (i64, i64, i64) = db.query_row(
        "SELECT (SELECT count(*) FROM catalog_restore_lineages),(SELECT count(*) FROM catalog_restore_worker_history),coalesce((SELECT sum(length(CAST(proof_json AS BLOB))) FROM catalog_restore_lineages),0)+coalesce((SELECT sum(length(CAST(proof_json AS BLOB))) FROM catalog_restore_worker_history),0)",
        [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    if !(0..=32).contains(&origins)
        || !(0..=16384).contains(&workers)
        || !(0..=8 * 1024 * 1024).contains(&bytes)
    {
        return Err(fail("complete restore ancestry exceeds supported bound"));
    }
    let mut by_id = BTreeMap::new();
    let mut statement = db.prepare("SELECT restore_id,catalog_id,from_incarnation,to_incarnation,parent_restore_id,proof_json FROM catalog_restore_lineages ORDER BY restore_id")?;
    let mut query = statement.query([])?;
    while let Some(row) = query.next()? {
        check(cancel)?;
        let raw: String = row.get(5)?;
        let proof: Origin = canonical(&raw)?;
        let tuple: (String, String, String, String, Option<String>) = (
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        );
        if proof.contract != 1
            || proof.schema_version != 41
            || proof.catalog_id != catalog
            || ![
                &proof.restore_id,
                &proof.catalog_id,
                &proof.from_incarnation,
                &proof.to_incarnation,
            ]
            .into_iter()
            .all(|v| identity(v))
            || proof.from_incarnation == proof.to_incarnation
            || ![
                &proof.pair_sha256,
                &proof.native_inventory_sha256,
                &proof.motion_binding_sha256,
            ]
            .into_iter()
            .all(|v| digest(v))
            || tuple
                != (
                    proof.restore_id.clone(),
                    proof.catalog_id.clone(),
                    proof.from_incarnation.clone(),
                    proof.to_incarnation.clone(),
                    proof.parent_restore_id.clone(),
                )
        {
            return Err(fail(
                "restore origin proof disagrees with exact retained identity",
            ));
        }
        if by_id.insert(proof.restore_id.clone(), proof).is_some() {
            return Err(fail("duplicate restore origin"));
        }
    }
    let mut ordered = Vec::new();
    if !by_id.is_empty() {
        let tips: Vec<_> = by_id
            .values()
            .filter(|v| v.to_incarnation == current)
            .map(|v| v.restore_id.clone())
            .collect();
        if tips.len() != 1 {
            return Err(fail("restore ancestry does not reach current incarnation"));
        }
        let mut next = Some(tips[0].clone());
        while let Some(id) = next {
            check(cancel)?;
            let origin = by_id
                .remove(&id)
                .ok_or_else(|| fail("restore ancestry missing predecessor or cycle"))?;
            if let Some(child) = ordered.last() {
                let child: &Origin = child;
                if child.from_incarnation != origin.to_incarnation {
                    return Err(fail("restore ancestry incarnation discontinuity"));
                }
            }
            next = origin.parent_restore_id.clone();
            ordered.push(origin);
        }
        if !by_id.is_empty() {
            return Err(fail("restore ancestry has disconnected or competing hops"));
        }
        ordered.reverse();
    }
    let mut incarnations = BTreeSet::new();
    if let Some(first) = ordered.first() {
        incarnations.insert(first.from_incarnation.as_str());
    }
    for origin in &ordered {
        if !incarnations.insert(origin.to_incarnation.as_str()) {
            return Err(fail("restore lineage reuses a historical incarnation"));
        }
    }
    let mut retained = BTreeMap::new();
    let mut statement = db.prepare("SELECT restore_id,owner_kind,owner_id,origin_incarnation,tuple_sha256,proof_json FROM catalog_restore_worker_history ORDER BY restore_id,owner_kind,owner_id")?;
    let mut query = statement.query([])?;
    while let Some(row) = query.next()? {
        check(cancel)?;
        let proof: Worker = canonical(&row.get::<_, String>(5)?)?;
        let tuple: (String, String, String, String, String) = (
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        );
        let hop = ordered
            .iter()
            .position(|o| o.restore_id == proof.restore_id)
            .ok_or_else(|| fail("historical worker has no authenticated restore origin"))?;
        if proof.contract != 1
            || !matches!(proof.owner_kind.as_str(), "processing" | "observation")
            || !identity(&proof.owner_id)
            || !digest(&proof.tuple_sha256)
            || !ordered[..=hop]
                .iter()
                .any(|o| o.from_incarnation == proof.origin_incarnation)
            || tuple
                != (
                    proof.restore_id.clone(),
                    proof.owner_kind.clone(),
                    proof.owner_id.clone(),
                    proof.origin_incarnation.clone(),
                    proof.tuple_sha256.clone(),
                )
        {
            return Err(fail(
                "historical worker proof disagrees with authenticated ancestry",
            ));
        }
        let key = (
            proof.restore_id.clone(),
            proof.owner_kind.clone(),
            proof.owner_id.clone(),
        );
        if retained.insert(key, proof).is_some() {
            return Err(fail("duplicate historical worker proof"));
        }
    }
    // An older origin must be carried through every immediately preceding hop
    // with exactly the same owner tuple, not rediscovered as a new witness.
    for (index, origin) in ordered.iter().enumerate() {
        for worker in retained
            .values()
            .filter(|w| w.restore_id == origin.restore_id)
        {
            if worker.origin_incarnation != origin.from_incarnation {
                let previous = index.checked_sub(1).and_then(|i| {
                    retained.get(&(
                        ordered[i].restore_id.clone(),
                        worker.owner_kind.clone(),
                        worker.owner_id.clone(),
                    ))
                });
                if previous.is_none_or(|p| {
                    p.origin_incarnation != worker.origin_incarnation
                        || p.tuple_sha256 != worker.tuple_sha256
                }) {
                    return Err(fail(
                        "historical worker ancestry is missing or changed at an intermediate hop",
                    ));
                }
            }
        }
    }
    Ok(Chain {
        origins: ordered,
        workers: retained,
    })
}

/// Only the staged rotation owner may call this with its internally derived
/// exact rows. The callback is reset before return, including transaction error.
/// This does not commit or rotate; both must share the caller's isolated writer.
pub(super) struct PreparedInsertion {
    pub(super) origin_json: String,
    pub(super) worker_json: Vec<String>,
    pub(super) expected: BTreeMap<(String, String), String>,
}
/// Complete validation and serialization only. The caller retains the same
/// isolated WRITE and its continuity observer before using these exact rows.
pub(super) fn prepare_exact(
    db: &Connection,
    origin: &Origin,
    workers: &[Worker],
) -> Result<PreparedInsertion> {
    if db.is_autocommit()
        || db.transaction_state(Some("main"))? != rusqlite::TransactionState::Write
    {
        return Err(fail(
            "restore lineage insertion requires isolated staged writer",
        ));
    }
    if workers.len() > 4096 {
        return Err(fail("restore insertion owner count exceeds bound"));
    }
    let previous = read(
        db,
        &origin.catalog_id,
        &origin.from_incarnation,
        &AtomicBool::new(false),
    )?;
    if previous.origins.last().map(|o| &o.restore_id) != origin.parent_restore_id.as_ref() {
        return Err(fail(
            "restore insertion does not extend the complete current lineage",
        ));
    }
    if origin.contract != 1
        || origin.schema_version != 41
        || origin.from_incarnation == origin.to_incarnation
        || ![
            &origin.restore_id,
            &origin.catalog_id,
            &origin.from_incarnation,
            &origin.to_incarnation,
        ]
        .into_iter()
        .all(|v| identity(v))
        || ![
            &origin.pair_sha256,
            &origin.native_inventory_sha256,
            &origin.motion_binding_sha256,
        ]
        .into_iter()
        .all(|v| digest(v))
        || previous.origins.len() >= 32
        || previous.origins.iter().any(|o| {
            o.restore_id == origin.restore_id
                || o.from_incarnation == origin.to_incarnation
                || o.to_incarnation == origin.to_incarnation
        })
    {
        return Err(fail("invalid, reused or over-budget restore transition"));
    }
    for worker in workers {
        if worker.contract != 1
            || worker.restore_id != origin.restore_id
            || !matches!(worker.owner_kind.as_str(), "processing" | "observation")
            || !identity(&worker.owner_id)
            || !digest(&worker.tuple_sha256)
        {
            return Err(fail("invalid exact historical owner insertion"));
        }
        if worker.origin_incarnation != origin.from_incarnation {
            let prior = previous.origins.last().and_then(|tip| {
                previous.workers.get(&(
                    tip.restore_id.clone(),
                    worker.owner_kind.clone(),
                    worker.owner_id.clone(),
                ))
            });
            if prior.is_none_or(|p| {
                p.origin_incarnation != worker.origin_incarnation
                    || p.tuple_sha256 != worker.tuple_sha256
            }) {
                return Err(fail(
                    "restore insertion cannot invent or change an ancestral worker tuple",
                ));
            }
        }
    }
    let origin_json = serde_json::to_string(origin)?;
    let worker_json = workers
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let bytes = worker_json
        .iter()
        .try_fold(origin_json.len(), |total, row| total.checked_add(row.len()))
        .ok_or_else(|| fail("restore insertion bytes overflow"))?;
    if bytes > 8 * 1024 * 1024 {
        return Err(fail("restore insertion bytes exceed bound"));
    }
    let mut expected = BTreeMap::new();
    expected.insert(
        ("origin".to_owned(), origin.restore_id.clone()),
        origin_json.clone(),
    );
    for (worker, json) in workers.iter().zip(&worker_json) {
        if worker.restore_id != origin.restore_id
            || expected
                .insert(
                    (
                        "worker".to_owned(),
                        format!(
                            "{}:{}:{}",
                            worker.restore_id, worker.owner_kind, worker.owner_id
                        ),
                    ),
                    json.clone(),
                )
                .is_some()
        {
            return Err(fail("duplicate or foreign exact restore insertion"));
        }
    }
    Ok(PreparedInsertion {
        origin_json,
        worker_json,
        expected,
    })
}

pub(super) fn insert_exact(db: &Connection, origin: &Origin, workers: &[Worker]) -> Result<()> {
    insert_exact_inner(db, origin, workers, None)
}
pub(super) fn insert_exact_until(
    db: &Connection,
    origin: &Origin,
    workers: &[Worker],
    deadline: std::time::Instant,
    cancel: &AtomicBool,
) -> Result<()> {
    insert_exact_inner(db, origin, workers, Some((deadline, cancel)))
}
fn insert_exact_inner(
    db: &Connection,
    origin: &Origin,
    workers: &[Worker],
    bounded: Option<(std::time::Instant, &AtomicBool)>,
) -> Result<()> {
    let live = || -> Result<()> {
        if let Some((deadline, cancel)) = bounded {
            check(cancel)?;
            if std::time::Instant::now() >= deadline {
                return Err(fail("current restore insertion deadline exceeded"));
            }
        }
        Ok(())
    };
    live()?;
    let PreparedInsertion {
        origin_json,
        worker_json,
        expected,
    } = prepare_exact(db, origin, workers)?;
    db.create_scalar_function(
        "catabolic_restore_lineage_insert",
        3,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
        move |ctx| {
            let kind: String = ctx.get(0)?;
            let id: String = ctx.get(1)?;
            let proof: String = ctx.get(2)?;
            Ok(i64::from(
                expected.get(&(kind, id)).is_some_and(|v| v == &proof),
            ))
        },
    )?;
    struct Reset<'a>(Option<&'a Connection>);
    impl Drop for Reset<'_> {
        fn drop(&mut self) {
            if let Some(db) = self.0 {
                let _ = db.remove_function("catabolic_restore_lineage_insert", 3);
            }
        }
    }
    let mut reset = Reset(Some(db));
    let outcome: Result<()> = (|| {
        live()?;
        db.execute("INSERT INTO catalog_restore_lineages(restore_id,catalog_id,from_incarnation,to_incarnation,parent_restore_id,proof_json) VALUES(?,?,?,?,?,?)",params![origin.restore_id,origin.catalog_id,origin.from_incarnation,origin.to_incarnation,origin.parent_restore_id,origin_json])?;
        for (worker, json) in workers.iter().zip(worker_json) {
            live()?;
            db.execute("INSERT INTO catalog_restore_worker_history(restore_id,owner_kind,owner_id,origin_incarnation,tuple_sha256,proof_json) VALUES(?,?,?,?,?,?)",params![worker.restore_id,worker.owner_kind,worker.owner_id,worker.origin_incarnation,worker.tuple_sha256,json])?;
        }
        Ok(())
    })();
    let removal = db.remove_function("catabolic_restore_lineage_insert", 3);
    if removal.is_ok() {
        reset.0 = None;
    }
    outcome?;
    removal?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        db.execute_batch(include_str!("schema41.sql")).unwrap();
        db
    }
    fn origin(id: &str, from: &str, to: &str, parent: Option<&str>) -> Origin {
        Origin {
            contract: 1,
            restore_id: id.into(),
            catalog_id: "catalog".into(),
            schema_version: 41,
            from_incarnation: from.into(),
            to_incarnation: to.into(),
            parent_restore_id: parent.map(str::to_owned),
            pair_sha256: "a".repeat(64),
            native_inventory_sha256: "b".repeat(64),
            motion_binding_sha256: "c".repeat(64),
        }
    }
    fn worker(id: &str) -> Worker {
        Worker {
            contract: 1,
            restore_id: id.into(),
            owner_kind: "observation".into(),
            owner_id: "job".into(),
            origin_incarnation: "first".into(),
            tuple_sha256: "d".repeat(64),
        }
    }
    #[test]
    fn exact_insert_is_transaction_owned_and_append_only() {
        let db = database();
        let proof = origin("restore1", "first", "second", None);
        assert!(insert_exact(&db, &proof, &[]).is_err());
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        insert_exact(&db, &proof, &[worker("restore1")]).unwrap();
        assert!(
            db.query_row::<i64, _, _>(
                "SELECT catabolic_restore_lineage_insert('origin','x','{}')",
                [],
                |r| r.get(0)
            )
            .is_err()
        );
        assert!(
            db.execute("DELETE FROM catalog_restore_worker_history", [])
                .is_err()
        );
        assert!(
            db.execute(
                "UPDATE catalog_restore_lineages SET to_incarnation='changed'",
                []
            )
            .is_err()
        );
        let chain = read(&db, "catalog", "second", &AtomicBool::new(false)).unwrap();
        assert_eq!(chain.origins.len(), 1);
        assert_eq!(chain.workers.len(), 1);
        db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            db.query_row::<i64, _, _>("SELECT count(*) FROM catalog_restore_lineages", [], |r| r
                .get(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn repeated_history_requires_exact_every_hop_and_current_tip() {
        let db = database();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        insert_exact(&db, &origin("r1", "first", "second", None), &[worker("r1")]).unwrap();
        insert_exact(
            &db,
            &origin("r2", "second", "third", Some("r1")),
            &[worker("r2")],
        )
        .unwrap();
        assert_eq!(
            read(&db, "catalog", "third", &AtomicBool::new(false))
                .unwrap()
                .origins
                .len(),
            2
        );
        assert!(read(&db, "foreign", "third", &AtomicBool::new(false)).is_err());
        assert!(read(&db, "catalog", "second", &AtomicBool::new(false)).is_err());
        let mut changed = worker("r3");
        changed.tuple_sha256 = "e".repeat(64);
        assert!(
            insert_exact(
                &db,
                &origin("r3", "third", "fourth", Some("r2")),
                &[changed],
            )
            .is_err()
        );
        assert_eq!(
            read(&db, "catalog", "third", &AtomicBool::new(false))
                .unwrap()
                .origins
                .len(),
            2
        );
    }
    #[test]
    fn null_callback_or_absent_callback_never_admits_lineage() {
        let db = database();
        let proof = origin("r1", "first", "second", None);
        let json = serde_json::to_string(&proof).unwrap();
        let sql =
            "INSERT INTO catalog_restore_lineages VALUES('r1','catalog','first','second',NULL,?)";
        assert!(db.execute(sql, [&json]).is_err());
        db.create_scalar_function(
            "catabolic_restore_lineage_insert",
            3,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            |_| Ok(None::<i64>),
        )
        .unwrap();
        assert!(db.execute(sql, [&json]).is_err());
    }
    #[test]
    fn disconnected_cycle_cancelled_and_noncanonical_history_refuse() {
        let db = database();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        insert_exact(&db, &origin("r1", "first", "second", None), &[]).unwrap();
        assert!(read(&db, "catalog", "second", &AtomicBool::new(true)).is_err());
        assert!(insert_exact(&db, &origin("fork", "first", "other", None), &[]).is_err());
        assert!(insert_exact(&db, &origin("r2", "second", "first", Some("r1")), &[]).is_err());
        assert!(read(&db, "catalog", "first", &AtomicBool::new(false)).is_err());
        let raw = serde_json::to_string(&origin("r3", "x", "y", None)).unwrap();
        assert!(canonical::<Origin>(&format!(" {raw}")).is_err());
        let duplicate = raw.replacen("\"contract\":1", "\"contract\":1,\"contract\":1", 1);
        assert!(canonical::<Origin>(&duplicate).is_err());
    }
}
