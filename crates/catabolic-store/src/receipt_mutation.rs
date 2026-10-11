// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Verified ingestion of producer-declared external rendition receipts.
//!
//! Receipt identity is immutable. Every new receipt keeps its source and output
//! descriptors open through the provenance transaction; all exit checks happen
//! before commit. Only bytes and their revisions are verified, never the external
//! producer's account of the processing it performed.
use crate::{Error, Result, Store, components, mutation, output_mutation as outputs, source};
use catabolic_core::{encode, encode_with, sha256, validate_finite};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::Read,
    os::unix::fs::MetadataExt,
    time::{Duration, Instant},
};

pub const MAX_RECEIPT_BYTES: usize = 1_048_576;
const REVISION_FIELDS: &[&str] = &["size", "mtime_ns", "device", "inode", "ctime_ns"];
const FIELDS: &[&str] = &[
    "version",
    "database_id",
    "profile",
    "producer",
    "instance",
    "job_id",
    "attempt",
    "configuration_digest",
    "tools",
    "outcome",
    "source",
    "outputs",
];

pub(crate) fn text<'a>(value: &'a Value, label: &str, maximum: usize) -> Result<&'a str> {
    let value = value
        .as_str()
        .filter(|s| !s.contains('\0') && !s.trim().is_empty())
        .ok_or_else(|| {
            Error(format!(
                "{label} must be nonempty text without NUL characters"
            ))
        })?;
    if value.len() > maximum {
        return Err(Error(format!("{label} exceeds {maximum} UTF-8 bytes")));
    }
    Ok(value)
}
fn exact(value: &Value, fields: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.len() == fields.len() && fields.iter().all(|k| o.contains_key(*k)))
}
fn digest_value(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn integer(value: &Value) -> bool {
    value
        .as_number()
        .is_some_and(|n| !n.to_string().contains(['.', 'e', 'E']))
}
/// Produce a descriptor-validated five-part source revision without changing it.
pub fn source_receipt(store: &Store, profile: &str, file: &str, item: &str) -> Result<Value> {
    outputs::validate_source_item(store, profile, file, item, &json!({"mode":"same_item"}))?;
    let snapshot = components::occurrence(&store.db, profile, file)?;
    let revision = source::validate(&snapshot)?;
    Ok(
        json!({"database_id":store.database_id,"profile":profile,"source":{"file_id":file,"item_id":item,"revision":revision}}),
    )
}
/// Validate envelope and return Python-compatible ASCII compact payload bytes.
pub fn canonical_payload(store: &Store, profile: &str, value: &Value) -> Result<String> {
    if !exact(value, FIELDS) {
        return Err(Error(
            "receipt must contain exactly the documented fields".into(),
        ));
    }
    if value["version"].as_u64() != Some(1) || value["outcome"] != "complete" {
        return Err(Error(
            "receipt must be version 1 with a complete outcome".into(),
        ));
    }
    if value["database_id"] != store.database_id || value["profile"] != profile {
        return Err(Error(
            "receipt belongs to another database or profile".into(),
        ));
    }
    for key in ["producer", "instance", "job_id", "attempt"] {
        text(&value[key], &format!("receipt {key}"), 255)?;
    }
    if !digest_value(&value["configuration_digest"]) {
        return Err(Error(
            "configuration_digest must be a lowercase SHA-256".into(),
        ));
    }
    let tools = value["tools"]
        .as_object()
        .filter(|o| (1..=32).contains(&o.len()))
        .ok_or_else(|| Error("receipt tools must declare 1..32 tool versions".into()))?;
    for (key, version) in tools {
        text(&json!(key), "tool name", 255)?;
        text(version, "tool version", 4096)?;
    }
    validate_finite(value).map_err(|_| Error("receipt must contain finite JSON values".into()))?;
    let payload = encode_with(value, true, true);
    if payload.len() > MAX_RECEIPT_BYTES {
        return Err(Error("receipt exceeds 1 MiB".into()));
    }
    Ok(payload)
}
struct Delivered {
    file: String,
    definition: String,
    snapshot: Value,
    expected: Option<String>,
    checksum: String,
}
fn delivered(store: &Store, profile: &str, value: &Value) -> Result<(Value, Vec<Delivered>)> {
    let src = &value["source"];
    if !exact(src, &["file_id", "item_id", "revision"]) {
        return Err(Error("invalid receipt source".into()));
    }
    for key in ["file_id", "item_id"] {
        text(&src[key], &format!("source {key}"), 255)?;
    }
    let revision = &src["revision"];
    if !exact(revision, REVISION_FIELDS) || !revision.as_object().unwrap().values().all(integer) {
        return Err(Error(
            "receipt source requires an exact numeric revision".into(),
        ));
    }
    let list = value["outputs"]
        .as_array()
        .filter(|a| (1..=32).contains(&a.len()))
        .ok_or_else(|| Error("receipt must deliver 1..32 outputs".into()))?;
    let source_file = src["file_id"].as_str().unwrap();
    let source_item = src["item_id"].as_str().unwrap();
    let mut snapshot = components::occurrence(&store.db, profile, source_file)?;
    if REVISION_FIELDS
        .iter()
        .filter(|k| **k != "ctime_ns")
        .any(|k| snapshot[*k] != revision[*k])
    {
        return Err(Error("receipt source revision is stale".into()));
    }
    snapshot["ctime_ns"] = revision["ctime_ns"].clone();
    let mut seen = BTreeSet::new();
    let mut records = Vec::new();
    for output in list {
        if !output.as_object().is_some_and(|o| {
            ["location", "path", "definition_id", "size"]
                .iter()
                .all(|k| o.contains_key(*k))
                && o.keys().all(|k| {
                    ["location", "path", "definition_id", "size", "sha256"].contains(&k.as_str())
                })
        }) {
            return Err(Error("invalid receipt output fields".into()));
        }
        let path = output["path"]
            .as_str()
            .ok_or_else(|| Error("invalid relative path".into()))?;
        crate::layout::relative_path(path)?;
        let location = text(&output["location"], "output location", 255)?;
        let definition = text(&output["definition_id"], "output definition_id", 255)?;
        if output["size"].as_u64().is_none_or(|n| n == 0) {
            return Err(Error("receipt output size must be positive".into()));
        }
        if output.get("sha256").is_some_and(|v| !digest_value(v)) {
            return Err(Error("invalid receipt output SHA-256".into()));
        }
        if store.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM generated_locations WHERE location=?)",
            [location],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(Error(
                "external workers cannot use Catabolic-owned generated locations".into(),
            ));
        }
        let file: Option<String> = store
            .db
            .query_row(
                "SELECT id FROM files WHERE location=? AND path=?",
                params![location, path],
                |r| r.get(0),
            )
            .optional()?;
        let file = file
            .filter(|f| f != source_file && seen.insert(f.clone()))
            .ok_or_else(|| {
                Error("receipt output is unscanned, duplicated, or the source itself".into())
            })?;
        if store.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM media_outputs WHERE file_id=? AND profile=?)",
            params![file, profile],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(Error("receipt output is already registered".into()));
        }
        let definition_value = outputs::get_definition(store, definition)?;
        outputs::validate_source_item(
            store,
            profile,
            source_file,
            source_item,
            &definition_value["definition"],
        )?;
        let output_snapshot = components::occurrence(&store.db, profile, &file)?;
        if output_snapshot["size"] != output["size"] {
            return Err(Error("delivered output size differs from receipt".into()));
        }
        records.push(Delivered {
            file,
            definition: definition.into(),
            snapshot: output_snapshot,
            expected: output["sha256"].as_str().map(str::to_owned),
            checksum: String::new(),
        });
    }
    Ok((snapshot, records))
}
/// Nest the bounded set of descriptor lifetimes so a late mutation unwinds as
/// an error while the SQL transaction is still live. Read memory is O(1 MiB).
fn verify_outputs<T>(
    outputs: &mut [Delivered],
    deadline: Instant,
    done: impl FnOnce(&[Delivered]) -> Result<T>,
) -> Result<T> {
    fn next<T>(
        all: &mut [Delivered],
        index: usize,
        deadline: Instant,
        done: impl FnOnce(&[Delivered]) -> Result<T>,
    ) -> Result<T> {
        if index == all.len() {
            return done(all);
        }
        let snapshot = all[index].snapshot.clone();
        source::with_validated_source(&snapshot, |file| {
            let metadata = file.metadata()?;
            all[index].snapshot["ctime_ns"] = json!(
                i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec())
            );
            let mut digest = Sha256::new();
            let mut bytes = 0u64;
            let mut buffer = vec![0; 1024 * 1024];
            loop {
                if Instant::now() > deadline {
                    return Err(Error("receipt validation exceeded one hour".into()));
                }
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                bytes = bytes
                    .checked_add(read as u64)
                    .ok_or_else(|| Error("delivered output size overflow".into()))?;
                if bytes > snapshot["size"].as_u64().unwrap_or(0) {
                    return Err(Error("delivered output changed while hashing".into()));
                }
                digest.update(&buffer[..read]);
            }
            if Some(bytes) != snapshot["size"].as_u64() {
                return Err(Error("delivered output changed while hashing".into()));
            }
            let checksum = format!("{:x}", digest.finalize());
            if all[index]
                .expected
                .as_ref()
                .is_some_and(|expected| *expected != checksum)
            {
                return Err(Error(
                    "delivered output checksum differs from receipt".into(),
                ));
            }
            all[index].checksum = checksum;
            // Keep a single chunk allocation live across the descriptor stack.
            drop(buffer);
            next(all, index + 1, deadline, done)
        })
    }
    next(outputs, 0, deadline, done)
}
pub(crate) fn enqueue_refresh(db: &Connection, profile: &str) -> Result<()> {
    db.execute("UPDATE watchers SET pending_generation=pending_generation+1,event_first=coalesce(event_first,unixepoch('now')),event_last=unixepoch('now') WHERE profile=? AND enabled=1 AND definition_id IN (SELECT id FROM watcher_definitions WHERE json_extract(definition,'$.events')=1)", [profile])?;
    db.execute("INSERT INTO catalog_refresh_queue(profile,catalog) SELECT profile,catalog FROM catalog_refresh_settings WHERE profile=? AND enabled=1 AND NOT EXISTS (SELECT 1 FROM watcher_owners w WHERE w.profile=catalog_refresh_settings.profile AND w.catalog=catalog_refresh_settings.catalog) ON CONFLICT(profile,catalog) DO UPDATE SET generation=generation+1,next_attempt=0,error=NULL,updated_at=CURRENT_TIMESTAMP",[profile])?;
    Ok(())
}
pub fn import_receipt(store: &Store, profile: &str, value: &Value) -> Result<Value> {
    import_with_acceptance(store, profile, value, |_, _| Ok(()))
}
/// Processor completion uses this to recheck its lease and acknowledge the
/// attempt in precisely the receipt transaction, including idempotent reuse.
pub(crate) fn import_with_acceptance(
    store: &Store,
    profile: &str,
    value: &Value,
    accepted: impl FnOnce(&Connection, &str) -> Result<()>,
) -> Result<Value> {
    mutation::require_recovered(&store.db)?;
    mutation::profile(store, profile)?;
    let payload = canonical_payload(store, profile, value)?;
    let digest = sha256(&payload);
    store.transaction(|db| {
        let previous: Option<(String,String)> = db.query_row("SELECT id,digest FROM external_receipts WHERE profile=? AND producer=? AND instance=? AND job_id=? AND attempt=?",params![profile,value["producer"].as_str(),value["instance"].as_str(),value["job_id"].as_str(),value["attempt"].as_str()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((id, old)) = previous {
            if old != digest { return Err(Error("receipt identity already has a conflicting payload".into())); }
            accepted(db, &id)?;
            let output_ids = db.prepare("SELECT output_id FROM receipt_outputs WHERE receipt_id=? ORDER BY output_id")?.query_map([&id], |r| r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
            return Ok(json!({"receipt_id":id,"reused":true,"output_ids":output_ids,"evidence":"previously accepted receipt; reuse is not a fresh verification"}));
        }
        let (source_snapshot, mut delivered) = delivered(store, profile, value)?;
        source::with_validated_source(&source_snapshot, |_| {
            verify_outputs(&mut delivered, Instant::now() + Duration::from_secs(3600), |verified| {
                let id = uuid::Uuid::new_v4().to_string();
                let mut records = Vec::new();
                for output in verified {
                    let result = outputs::record(db, profile, &output.file, value["source"]["file_id"].as_str().unwrap(),value["source"]["item_id"].as_str().unwrap(),&output.definition,None,&json!({"producer":value["producer"],"external_job_id":value["job_id"]}))?;
                    records.push((result["output_id"].as_str().unwrap().to_owned(), encode(&output.snapshot), output.checksum.clone()));
                }
                db.execute("INSERT INTO external_receipts(id,profile,producer,instance,job_id,attempt,digest,payload) VALUES (?,?,?,?,?,?,?,?)",params![id,profile,value["producer"].as_str(),value["instance"].as_str(),value["job_id"].as_str(),value["attempt"].as_str(),digest,payload])?;
                for (output, snapshot, checksum) in &records { db.execute("INSERT INTO receipt_outputs VALUES (?,?,?,?)", params![id,output,snapshot,checksum])?; }
                accepted(db, &id)?;
                enqueue_refresh(db, profile)?;
                Ok(json!({"receipt_id":id,"reused":false,"output_ids":records.iter().map(|r|&r.0).collect::<Vec<_>>(),"evidence":"delivered size and SHA-256 verified; processing claims are producer-declared"}))
            })
        })
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) struct Fixture {
        pub dir: tempfile::TempDir,
        pub store: Store,
        pub definition: String,
    }
    impl Fixture {
        pub(crate) fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("catalog.db");
            crate::migration::initialize(&path).unwrap();
            let store = Store::open(&path, true, false).unwrap();
            let root = dir.path().join("media");
            std::fs::create_dir(&root).unwrap();
            for name in ["source.mkv", "external.mkv", "third.mkv"] {
                std::fs::write(root.join(name), name.as_bytes()).unwrap();
            }
            let st = root.metadata().unwrap();
            store.transaction(|db| {
                db.execute("INSERT INTO locations VALUES ('media')",[])?;
                db.execute("INSERT INTO bindings VALUES ('default','source','media',?,?,?)",params![root.to_str(),st.dev() as i64,st.ino() as i64])?;
                db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','media',1,3,'[]')",[])?;
                for name in ["source.mkv", "external.mkv", "third.mkv"] {
                    let st = root.join(name).metadata()?;
                    db.execute("INSERT INTO files VALUES (?,'media',?)",params![name,name])?;
                    db.execute("INSERT INTO observations VALUES ('default',?,?,?,?,?,'present','scan')",params![name,st.len() as i64,st.mtime()*1_000_000_000+st.mtime_nsec(),st.dev() as i64,st.ino() as i64])?;
                }
                db.execute("INSERT INTO items VALUES ('item','movie','{}')",[])?;
                db.execute("INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES ('association','source.mkv','item','primary','explicit')",[])?;
                Ok(())
            }).unwrap();
            let definition = outputs::define(&store, "external", &json!({"purpose":"custom"}))
                .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned();
            Self {
                dir,
                store,
                definition,
            }
        }
        pub(crate) fn receipt(&self) -> Value {
            let captured = source_receipt(&self.store, "default", "source.mkv", "item").unwrap();
            json!({"version":1,"database_id":captured["database_id"],"profile":"default","producer":"fixture-processor","instance":"local","job_id":"job-1","attempt":"1","configuration_digest":"a".repeat(64),"tools":{"fixture":"1.0"},"outcome":"complete","source":captured["source"],"outputs":[{"location":"media","path":"external.mkv","definition_id":self.definition,"size":12}]})
        }
        fn count(&self, table: &str) -> i64 {
            self.store
                .db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        }
        fn empty(&self) {
            for table in ["media_outputs", "external_receipts", "receipt_outputs"] {
                assert_eq!(self.count(table), 0, "{table}");
            }
        }
    }
    #[test]
    fn receipt_acceptance_keeps_lineage_and_complete_immutable_provenance() {
        let f = Fixture::new();
        let mut receipt = f.receipt();
        receipt["outputs"][0]["sha256"] = json!(sha256(b"external.mkv"));
        let result = import_receipt(&f.store, "default", &receipt).unwrap();
        assert_eq!(result["reused"], false);
        uuid::Uuid::parse_str(result["receipt_id"].as_str().unwrap()).unwrap();
        let stored:(String,String,String,String)=f.store.db.query_row("SELECT r.payload,r.digest,o.sha256,o.snapshot FROM external_receipts r JOIN receipt_outputs o ON o.receipt_id=r.id",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(stored.0, encode_with(&receipt, true, true));
        assert_eq!(stored.1, sha256(&stored.0));
        assert_eq!(stored.2, sha256(b"external.mkv"));
        assert!(catabolic_core::decode_json(&stored.3).unwrap()["ctime_ns"].is_number());
        assert_eq!(f.count("processing_jobs"), 0);
        assert_eq!(f.count("processing_artifacts"), 0);
        let output = outputs::get(
            &f.store,
            "default",
            result["output_ids"][0].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(output["purpose"], "custom");
        assert_eq!(output["producer"], "fixture-processor");
        assert_eq!(
            f.store
                .db
                .query_row(
                    "SELECT active FROM item_files WHERE file_id='external.mkv'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            std::fs::read(f.dir.path().join("media/source.mkv")).unwrap(),
            b"source.mkv"
        );
    }
    #[test]
    fn duplicate_is_prior_acceptance_not_fresh_verification_and_conflicts_fail() {
        let f = Fixture::new();
        let mut receipt = f.receipt();
        let first = import_receipt(&f.store, "default", &receipt).unwrap();
        std::fs::remove_dir_all(f.dir.path().join("media")).unwrap();
        let repeated = import_receipt(&f.store, "default", &receipt).unwrap();
        assert_eq!(repeated["reused"], true);
        assert_eq!(repeated["receipt_id"], first["receipt_id"]);
        assert_eq!(repeated["output_ids"], first["output_ids"]);
        assert!(
            repeated["evidence"]
                .as_str()
                .unwrap()
                .contains("not a fresh verification")
        );
        receipt["tools"]["fixture"] = json!("2.0");
        assert!(
            import_receipt(&f.store, "default", &receipt)
                .unwrap_err()
                .0
                .contains("conflicting")
        );
    }
    #[test]
    fn multi_output_bad_checksum_and_cycles_roll_back_all_records() {
        let f = Fixture::new();
        let mut receipt = f.receipt();
        let mut third = receipt["outputs"][0].clone();
        third["path"] = json!("third.mkv");
        third["size"] = json!(9);
        third["sha256"] = json!("0".repeat(64));
        receipt["outputs"].as_array_mut().unwrap().push(third);
        assert!(
            import_receipt(&f.store, "default", &receipt)
                .unwrap_err()
                .0
                .contains("checksum")
        );
        f.empty();
        f.store.db.execute("INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES ('reverse','external.mkv','item','primary','explicit')",[]).unwrap();
        outputs::register(
            &f.store,
            "default",
            "source.mkv",
            "external.mkv",
            "item",
            &f.definition,
            &json!({}),
        )
        .unwrap();
        assert!(
            import_receipt(&f.store, "default", &f.receipt())
                .unwrap_err()
                .0
                .contains("cycle")
        );
        assert_eq!(f.count("media_outputs"), 1);
        assert_eq!(f.count("external_receipts"), 0);
    }
    #[test]
    fn late_source_or_output_mutation_rolls_back_receipt_and_acceptance() {
        for filename in ["source.mkv", "external.mkv"] {
            let f = Fixture::new();
            let receipt = f.receipt();
            let path = f.dir.path().join("media").join(filename);
            let result = import_with_acceptance(&f.store, "default", &receipt, |db, _| {
                db.execute("INSERT INTO catalogs(id) VALUES ('rolled-back')", [])?;
                std::fs::write(&path, b"late mutation")?;
                Ok(())
            });
            assert!(result.unwrap_err().0.contains("changed"));
            f.empty();
            assert_eq!(
                f.store
                    .db
                    .query_row(
                        "SELECT count(*) FROM catalogs WHERE id='rolled-back'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
        }
    }
    #[test]
    fn late_replacement_root_change_and_acceptance_failure_are_atomic() {
        for mode in ["leaf", "root", "fence"] {
            let f = Fixture::new();
            let receipt = f.receipt();
            let root = f.dir.path().join("media");
            let result = import_with_acceptance(&f.store, "default", &receipt, |_, _| {
                match mode {
                    "leaf" => {
                        std::fs::rename(root.join("external.mkv"), root.join("old.mkv"))?;
                        std::fs::write(root.join("external.mkv"), b"external.mkv")?;
                    }
                    "root" => {
                        std::fs::rename(&root, f.dir.path().join("old"))?;
                        std::fs::create_dir(&root)?;
                    }
                    _ => return Err(Error("worker lease expired".into())),
                }
                Ok(())
            });
            assert!(result.is_err(), "{mode}");
            f.empty();
        }
    }
    #[test]
    fn exact_schema_digest_text_and_numeric_revisions_are_enforced() {
        let f = Fixture::new();
        let original = f.receipt();
        let cases = [
            ("version", json!(true)),
            ("version", json!(1.0)),
            ("version", json!(2)),
            ("database_id", json!("other")),
            ("profile", json!("other")),
            ("producer", json!(" ")),
            ("instance", json!("x\u{0}")),
            ("attempt", json!(1)),
            ("configuration_digest", json!("A".repeat(64))),
            ("tools", json!({})),
            ("tools", json!({"x":" "})),
            ("outcome", json!("failed")),
            ("outputs", json!([])),
        ];
        for (key, value) in cases {
            let mut receipt = original.clone();
            receipt[key] = value;
            assert!(
                import_receipt(&f.store, "default", &receipt).is_err(),
                "{key}"
            );
            f.empty();
        }
        for revision in [
            json!({"size":1}),
            json!({"size":true,"mtime_ns":1,"device":1,"inode":1,"ctime_ns":1}),
        ] {
            let mut receipt = original.clone();
            receipt["source"]["revision"] = revision;
            assert!(import_receipt(&f.store, "default", &receipt).is_err());
        }
        let mut receipt = original.clone();
        receipt["extra"] = json!(1);
        assert!(import_receipt(&f.store, "default", &receipt).is_err());
        let mut receipt = original;
        receipt["source"]["revision"]["inode"] =
            catabolic_core::decode_json("123456789012345678901234567890").unwrap();
        assert!(
            import_receipt(&f.store, "default", &receipt)
                .unwrap_err()
                .0
                .contains("stale")
        );
        f.empty();
    }
    #[test]
    fn output_validation_rejects_unsafe_duplicate_unscanned_and_owned_targets() {
        let f = Fixture::new();
        let original = f.receipt();
        for path in [
            "../external.mkv",
            "/external.mkv",
            ".catabolic-owned",
            "missing.mkv",
            "source.mkv",
        ] {
            let mut receipt = original.clone();
            receipt["outputs"][0]["path"] = json!(path);
            assert!(
                import_receipt(&f.store, "default", &receipt).is_err(),
                "{path}"
            );
            f.empty();
        }
        for (key, value) in [
            ("size", json!(true)),
            ("size", json!(0)),
            ("size", json!(13)),
            ("sha256", json!("A".repeat(64))),
            ("definition_id", json!("not-a-definition")),
        ] {
            let mut receipt = original.clone();
            receipt["outputs"][0][key] = value;
            assert!(
                import_receipt(&f.store, "default", &receipt).is_err(),
                "{key}"
            );
        }
        let mut receipt = original.clone();
        let output = receipt["outputs"][0].clone();
        receipt["outputs"].as_array_mut().unwrap().push(output);
        assert!(
            import_receipt(&f.store, "default", &receipt)
                .unwrap_err()
                .0
                .contains("duplicated")
        );
        f.store
            .db
            .execute(
                "INSERT INTO generated_locations VALUES ('default','media')",
                [],
            )
            .unwrap();
        assert!(
            import_receipt(&f.store, "default", &original)
                .unwrap_err()
                .0
                .contains("generated")
        );
        f.empty();
    }
    #[test]
    fn source_ctime_and_association_are_required_and_symlinks_are_not_followed() {
        let f = Fixture::new();
        let mut receipt = f.receipt();
        receipt["source"]["revision"]["ctime_ns"] = json!(0);
        assert!(
            import_receipt(&f.store, "default", &receipt)
                .unwrap_err()
                .0
                .contains("revision changed")
        );
        let receipt = f.receipt();
        f.store
            .db
            .execute("UPDATE item_files SET active=0", [])
            .unwrap();
        assert!(import_receipt(&f.store, "default", &receipt).is_err());
        f.store
            .db
            .execute("UPDATE item_files SET active=1", [])
            .unwrap();
        let target = f.dir.path().join("media/external.mkv");
        std::fs::remove_file(&target).unwrap();
        std::os::unix::fs::symlink("source.mkv", target).unwrap();
        assert!(import_receipt(&f.store, "default", &receipt).is_err());
        f.empty();
    }
    #[test]
    fn canonical_unicode_digest_matches_frozen_python_encoding() {
        let f = Fixture::new();
        let mut receipt = f.receipt();
        receipt["producer"] = json!("café😀");
        let payload = canonical_payload(&f.store, "default", &receipt).unwrap();
        assert!(payload.contains(r#""producer":"caf\u00e9\ud83d\ude00""#));
        assert!(!payload.contains(": "));
        // Oracle use is confined to this explicit parity test, never runtime.
        use std::io::Write;
        let mut child=std::process::Command::new("python3").args(["-c","import json,sys,hashlib; v=json.load(sys.stdin); p=json.dumps(v,sort_keys=True,separators=(',',':'),allow_nan=False); print(hashlib.sha256(p.encode()).hexdigest())"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(encode(&receipt).as_bytes())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("Python parity oracle exceeded five seconds");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            sha256(&payload)
        );
    }
    #[test]
    fn limits_and_refresh_intent_are_atomic_and_reuse_does_not_enqueue_again() {
        let f = Fixture::new();
        let mut receipt = f.receipt();
        receipt["outputs"] = json!(vec![receipt["outputs"][0].clone(); 33]);
        assert!(import_receipt(&f.store, "default", &receipt).is_err());
        let mut receipt = f.receipt();
        receipt["source"]["large"] = json!("x".repeat(MAX_RECEIPT_BYTES));
        assert!(
            import_receipt(&f.store, "default", &receipt)
                .unwrap_err()
                .0
                .contains("1 MiB")
        );
        f.store
            .db
            .execute("INSERT INTO catalogs(id) VALUES ('refresh')", [])
            .unwrap();
        f.store
            .db
            .execute(
                "INSERT INTO catalog_refresh_settings VALUES ('default','refresh',1,0)",
                [],
            )
            .unwrap();
        let receipt = f.receipt();
        import_receipt(&f.store, "default", &receipt).unwrap();
        import_receipt(&f.store, "default", &receipt).unwrap();
        assert_eq!(
            f.store
                .db
                .query_row("SELECT generation FROM catalog_refresh_queue", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}
