// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Native source identity, bounded inventory and durable scope publication.
//! Inventory absence is authorized by an unchanged completed directory only;
//! deferred, excluded and failed child scopes retain their prior observations.
mod report;
use report::{Reuse, Statistics};

use crate::filesystem_mutation::{entries, identity, now, open_at, open_root, stat_at, version};
use crate::{Error, Result, Store, migration::rows};
use catabolic_core::{coordination as lifecycle, encode, sha256};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::File,
    ops::Deref,
    os::{
        fd::AsRawFd,
        unix::fs::{FileExt, MetadataExt},
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub fn execute(store: &Store, profile: &str, action: &str, args: &Value) -> Result<Value> {
    execute_inner(store, profile, action, args, None)
}

/// Find the durable caller request after an isolated worker outlives its waiter.
pub fn request_id(
    store: &Store,
    profile: &str,
    source: &str,
    requester: &str,
) -> Result<Option<String>> {
    Ok(store
        .db
        .query_row(
            "SELECT id FROM observation_requests WHERE profile=? AND source=? AND requester=?",
            (profile, source, requester),
            |row| row.get(0),
        )
        .optional()?)
}

/// Runtime entry point: admission and each accepted batch own a writer only
/// briefly. The traversal retains descriptors and bounded private queue state,
/// but no catalog connection or writer lease across filesystem waits.
pub fn execute_detached(
    store: &mut Store,
    profile: &str,
    action: &str,
    args: &Value,
) -> Result<Value> {
    execute_detached_with_publisher(store, profile, action, args, None)
}

pub fn execute_detached_with_publisher(
    store: &mut Store,
    profile: &str,
    action: &str,
    args: &Value,
    factory: Option<&mut dyn PublisherFactory>,
) -> Result<Value> {
    execute_detached_inner(store, profile, action, args, factory, None)
}

/// Closed protocol only: no publisher callback can enter this call chain.
pub(crate) fn execute_closed_detached(
    store: &mut Store,
    profile: &str,
    args: &Value,
) -> Result<(
    Value,
    Option<crate::native_observation_companion::OwnedClaim>,
)> {
    let mut claim = None;
    let result = execute_detached_inner(store, profile, "scan", args, None, Some(&mut claim))?;
    Ok((result, claim))
}

/// The explicit shared-handle path only attaches caller demand. It never runs
/// recovery, makes a replacement job, acquires a claim, or invokes callbacks.
pub(crate) fn attach_closed_shared(
    store: &Store,
    context: &crate::native_observation_companion::Reservation,
    shared: &crate::native_observation_companion::SharedClaim,
) -> Result<Value> {
    store.transaction(|db| {
        let (_, guarantees) = crate::native_observation_companion::shared_demand(store, context, shared)?;
        if db.prepare("SELECT 1 FROM observation_requests WHERE profile=? AND source=? AND requester=?")?.exists(params![context.profile,context.source,context.requester])? {
            return Err(Error("shared_observation_requester_already_exists".into()));
        }
        let (state, report): (String, Option<String>) = db.query_row(
            "SELECT state,report FROM observation_jobs WHERE id=?",
            [&shared.owned.job_id], |r| Ok((r.get(0)?,r.get(1)?)),
        )?;
        let request = uuid::Uuid::new_v4().to_string();
        db.execute("INSERT INTO observation_requests(id,profile,source,requester,guarantees,job_id,created_at) VALUES(?,?,?,?,?,?,?)",params![request,context.profile,context.source,context.requester,guarantees,shared.owned.job_id,now()])?;
        scan_result(store,&shared.owned.job_id,&request,&catabolic_core::decode_json(&guarantees)?,catabolic_core::decode_json(report.as_deref().unwrap_or("{}"))?,Some(if state=="complete" { Reuse::Completed } else { Reuse::Inflight }))
    })
}

fn execute_detached_inner(
    store: &mut Store,
    profile: &str,
    action: &str,
    args: &Value,
    factory: Option<&mut dyn PublisherFactory>,
    closed_claim: Option<&mut Option<crate::native_observation_companion::OwnedClaim>>,
) -> Result<Value> {
    let mut admitted = None;
    let result = execute_inner(store, profile, action, args, Some(&mut admitted))?;
    let Some(admitted) = admitted else {
        return Ok(result);
    };
    if let Some(claim) = closed_claim {
        *claim = Some(crate::native_observation_companion::capture_claim(
            store,
            &admitted.job,
            &admitted.request,
            &admitted.claim,
        )?);
    }
    let source: String = store.db.query_row(
        "SELECT source FROM observation_jobs WHERE id=?",
        [&admitted.job],
        |r| r.get(0),
    )?;
    let prepared = prepare_scan(store, profile, &source, args, admitted)?;
    match prepared {
        Prepared::Result(value) => Ok(value),
        Prepared::Scan(mut scan, context) => {
            if let Some(factory) = factory {
                scan.publisher.with(|p| {
                    p.initial_scopes = Some(Publisher::snapshot(store, &p.job)?);
                    p.eager_scopes = true;
                    Ok(())
                })?;
                let job = rows(
                    &store.db,
                    "SELECT * FROM observation_jobs WHERE id=?",
                    &[scan.job.clone().into()],
                )?
                .remove(0);
                let bound = crate::embedding::application(
                    store,
                    profile,
                    "binding",
                    &json!({"kind":"source","owner":source}),
                )?;
                match factory.create(scan.publisher.clone(), job, bound) {
                    Ok(callbacks) => scan.callbacks = Some(callbacks),
                    Err(error) => {
                        fail_claim(store, &scan.job, &scan.claim, &error)?;
                        return Err(error);
                    }
                }
            }
            let outcome = store.detached(|| scan.walk(None));
            // A failed reopen deliberately leaves Store unusable. Do not mask
            // it by reporting a terminal job on a replacement catalog.
            if !store.owns_writer_lock() {
                return outcome.and(Err(Error(
                    "catalog session unavailable after detached scan".into(),
                )));
            }
            if scan.callbacks.as_ref().is_some_and(|c| c.exceptional()) {
                let error = outcome
                    .err()
                    .unwrap_or_else(|| Error("publisher callback failed".into()));
                fail_claim(store, &scan.job, &scan.claim, &error)?;
                return Err(error);
            }
            scan.record_outcome(outcome);
            let job = scan.job.clone();
            let claim = scan.claim.clone();
            let result = finish_scan(store, *scan, context);
            if let Err(error) = &result {
                fail_claim(store, &job, &claim, error)?;
            }
            result
        }
    }
}

fn execute_inner(
    store: &Store,
    profile: &str,
    action: &str,
    args: &Value,
    admitted: Option<&mut Option<Admitted>>,
) -> Result<Value> {
    if !store
        .db
        .prepare("SELECT 1 FROM profiles WHERE id=?")?
        .exists([profile])?
    {
        return Err(Error(format!("unknown profile: {profile}")));
    }
    let source = args["source"]
        .as_str()
        .or(args["owner"].as_str())
        .unwrap_or("media");
    let resumed_source = if action == "scan" {
        args.get("continue_id")
            .or(args.get("continue_request"))
            .or(args.get("request_id"))
            .and_then(Value::as_str)
            .map(|id| {
                store
                    .db
                    .query_row(
                        "SELECT source FROM observation_requests WHERE id=? AND profile=?",
                        [id, profile],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()
            })
            .transpose()?
            .flatten()
    } else {
        None
    };
    let source = resumed_source.as_deref().unwrap_or(source);
    match action {
        "bind" => bind(
            store,
            profile,
            args["kind"].as_str().unwrap_or("source"),
            source,
            args["root"]
                .as_str()
                .ok_or_else(|| Error("binding requires an explicit root".into()))?,
        ),
        "trust" => configure_trust(store, profile, source, args),
        "remount" => crate::remount::execute(store, profile, args),
        "recover_device_numbers" => crate::remount::recover_device_numbers(store, profile, args),
        "scan_policy" => configure_policy(store, profile, source, args),
        "scan" => scan(store, profile, source, args, admitted),
        "dirty" => {
            if store
                .db
                .prepare("SELECT 1 FROM generated_locations WHERE profile=? AND location=?")?
                .exists([profile, source])?
            {
                return Ok(json!({"ignored":"generated_source_uses_completion_events"}));
            }
            store.transaction(|db|{db.execute("INSERT INTO observation_sources VALUES(?,?,1) ON CONFLICT(profile,source) DO UPDATE SET dirty_generation=dirty_generation+1",[profile,source])?;dirty_watchers(db,profile,None)?;Ok(())})?;
            Ok(json!({"source":source,"reason":"event","coverage":"full_source"}))
        }
        "recover" => recover(store),
        "cancel" => {
            let request = args["request_id"]
                .as_str()
                .ok_or_else(|| Error("unknown_observation_request".into()))?;
            store.transaction(|db| {
                let current: Option<String> = db.query_row("SELECT state FROM observation_requests WHERE id=? AND profile=?", [request,profile], |r|r.get(0)).optional()?;
                let current = current.ok_or_else(|| Error("unknown_observation_request".into()))?;
                let effect = lifecycle::reduce(&lifecycle::State::default(), &lifecycle::Input::CancelCaller { active: current == "active" }).map_err(|e|Error(format!("invalid observation cancellation: {e:?}")))?;
                if matches!(effect,lifecycle::Effect::CancelCaller { changed:true }) {
                    db.execute("UPDATE observation_requests SET state='cancelled' WHERE id=? AND profile=?", [request,profile])?;
                }
                Ok(())
            })?;
            Ok(json!({"request_id":request,"state":"cancelled"}))
        }
        _ => Err(Error(format!("unknown observation action: {action}"))),
    }
}
pub fn binding(store: &Store, profile: &str, kind: &str, owner: &str) -> Result<Value> {
    rows(
        &store.db,
        "SELECT * FROM bindings WHERE profile=? AND kind=? AND owner=?",
        &[
            profile.to_string().into(),
            kind.to_string().into(),
            owner.to_string().into(),
        ],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| Error(format!("unbound {kind} {owner} in profile {profile}")))
}
pub fn trust_policy(store: &Store, profile: &str, source: &str) -> Result<Value> {
    let rows = rows(
        &store.db,
        "SELECT * FROM source_identity_policies WHERE profile=? AND location=?",
        &[profile.to_string().into(), source.to_string().into()],
    )?;
    let row = rows.first();
    let preset = row
        .and_then(|v| v["identity_policy"].as_str())
        .unwrap_or("strict");
    let default = if preset == "path" { "skip" } else { "check" };
    let mut settings = json!({"uuid":default,"device":default,"inode":default});
    if let Some(raw) = row.and_then(|r| r["settings"].as_str()) {
        let overrides: Value = catabolic_core::decode_json(raw)?;
        if let Some(overrides) = overrides.as_object() {
            for (k, v) in overrides {
                settings[k] = v.clone();
            }
        }
    }
    Ok(
        json!({"preset":preset,"settings":settings,"revision":row.map(|r|r["revision"].clone()).unwrap_or(json!(0))}),
    )
}
pub(crate) fn checked_root(store: &Store, profile: &str, binding: &Value) -> Result<(File, Value)> {
    let path = binding["root"]
        .as_str()
        .ok_or_else(|| Error("invalid binding root".into()))?;
    let kind = binding["kind"].as_str().unwrap_or("source");
    let root =
        open_root(path).map_err(|e| Error(format!("unavailable {kind} root: {path}: {e}")))?;
    let st = root.metadata()?;
    let policy = if kind == "source" {
        trust_policy(store, profile, binding["owner"].as_str().unwrap())?
    } else {
        json!({"revision":0,"settings":{"uuid":"check","device":"check","inode":"check"}})
    };
    let expected_uuid: Option<String> = store
        .db
        .query_row(
            "SELECT volume_uuid FROM binding_volumes WHERE profile=? AND kind=? AND owner=?",
            params![profile, kind, binding["owner"].as_str()],
            |r| r.get(0),
        )
        .optional()?;
    let mut checks = serde_json::Map::new();
    for key in ["uuid", "device", "inode"] {
        if kind == "source" && policy["settings"][key] == "skip" {
            checks.insert(key.into(), json!("skipped_owner_policy"));
            continue;
        }
        if key == "uuid" {
            if let Some(expected) = &expected_uuid {
                if crate::source::volume_uuid(&root).as_ref() != Some(expected) {
                    return Err(Error("bound volume identity changed or unavailable".into()));
                }
                checks.insert(key.into(), json!("matched"));
            } else {
                checks.insert(key.into(), json!("not_enrolled"));
            }
        } else {
            let actual = if key == "device" { st.dev() } else { st.ino() };
            if binding[key].as_u64() != Some(actual) {
                return Err(Error(format!(
                    "root identity changed: {path}; inspect remount repair"
                )));
            }
            checks.insert(key.into(), json!("matched"));
        }
    }
    let verified = checks.values().all(|v| v == "matched");
    Ok((
        root,
        json!({"policy":policy,"checks":checks,"allowed":true,"identity_verified":verified,"availability":"available"}),
    ))
}
fn recovered(store: &Store) -> Result<()> {
    if store
        .db
        .prepare("SELECT 1 FROM journal LIMIT 1")?
        .exists([])?
    {
        return Err(Error(
            "recover pending operations before changing desired catalog state".into(),
        ));
    }
    Ok(())
}
fn bind(store: &Store, profile: &str, kind: &str, owner: &str, root: &str) -> Result<Value> {
    recovered(store)?;
    if !matches!(kind, "source" | "output") {
        return Err(Error("binding kind must be source or output".into()));
    }
    if owner.is_empty()
        || owner.len() > 128
        || !owner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
    {
        return Err(Error("invalid name".into()));
    }
    let canonical = Path::new(root).canonicalize()?;
    let path = canonical
        .to_str()
        .ok_or_else(|| Error("paths must be valid UTF-8".into()))?;
    let fd = open_root(path)?;
    let stat = fd.metadata()?;
    if store.path.starts_with(&canonical) {
        return Err(Error(
            "a source or output cannot contain the database".into(),
        ));
    }
    for row in rows(&store.db, "SELECT * FROM bindings", &[])? {
        if row["profile"] == profile && row["kind"] == kind && row["owner"] == owner {
            continue;
        }
        let other = Path::new(row["root"].as_str().unwrap());
        let overlaps = canonical.starts_with(other) || other.starts_with(&canonical);
        let generated = store
            .db
            .prepare("SELECT 1 FROM generated_locations WHERE location=?")?
            .exists([row["owner"].as_str().unwrap()])?;
        if overlaps
            && (kind == "output"
                || row["kind"] == "output"
                || row["profile"] == profile
                || generated)
        {
            return Err(Error(format!(
                "root overlaps registered {}: {}",
                row["kind"].as_str().unwrap(),
                other.display()
            )));
        }
    }
    let old = binding(store, profile, kind, owner).ok();
    if old.as_ref().is_some_and(|old| {
        old["root"] == path
            && old["device"].as_u64() == Some(stat.dev())
            && old["inode"].as_u64() == Some(stat.ino())
    }) {
        return Ok(old.unwrap());
    }
    if kind == "source"
        && store
            .db
            .prepare("SELECT 1 FROM generated_locations WHERE location=?")?
            .exists([owner])?
    {
        return Err(Error("generated locations require artifact binding; ordinary binding cannot adopt or replace them".into()));
    }
    if old.is_some() && kind == "output" {
        for table in ["owned_links", "owned_hardlinks", "retained_hardlinks"] {
            if store
                .db
                .prepare(&format!(
                    "SELECT 1 FROM {table} WHERE profile=? AND catalog=? LIMIT 1"
                ))?
                .exists([profile, owner])?
            {
                return Err(Error(
                    "cannot rebind an output while it has owned links or retained hardlinks".into(),
                ));
            }
        }
    }
    let stable = crate::source::volume_uuid(&fd);
    let current = open_root(path)?;
    if identity(&stat) != identity(&current.metadata()?) {
        return Err(Error("root changed during binding".into()));
    }
    store.transaction(|db|{let table=if kind=="source"{"locations"}else{"catalogs"};db.execute(&format!("INSERT INTO {table}(id) VALUES(?) ON CONFLICT DO NOTHING"),[owner])?;
        db.execute("INSERT INTO bindings VALUES(?,?,?,?,?,?) ON CONFLICT(profile,kind,owner) DO UPDATE SET root=excluded.root,device=excluded.device,inode=excluded.inode",params![profile,kind,owner,path,stat.dev() as i64,stat.ino() as i64])?;
        db.execute("DELETE FROM binding_volumes WHERE profile=? AND kind=? AND owner=?",[profile,kind,owner])?;if let Some(stable)=&stable{db.execute("INSERT INTO binding_volumes VALUES(?,?,?,?)",params![profile,kind,owner,stable])?;}
        if kind=="source"{db.execute("DELETE FROM observations WHERE profile=? AND file_id IN(SELECT id FROM files WHERE location=?)",[profile,owner])?;}Ok(())})?;
    binding(store, profile, kind, owner)
}
fn configure_trust(store: &Store, profile: &str, source: &str, args: &Value) -> Result<Value> {
    let binding = binding(store, profile, "source", source)?;
    let old = trust_policy(store, profile, source)?;
    let preset = args["policy"]
        .as_str()
        .unwrap_or(old["preset"].as_str().unwrap());
    if !matches!(preset, "strict" | "path") {
        return Err(Error("identity policy must be strict or path".into()));
    }
    let mut settings = if args["policy"].is_string() {
        let mode = if preset == "path" { "skip" } else { "check" };
        json!({"uuid":mode,"device":mode,"inode":mode})
    } else {
        old["settings"].clone()
    };
    if let Some(overrides) = args.get("overrides") {
        let overrides = overrides
            .as_object()
            .ok_or_else(|| Error("identity settings must be an object".into()))?;
        for (k, v) in overrides {
            if !matches!(k.as_str(), "uuid" | "device" | "inode")
                || !matches!(v.as_str(), Some("check" | "skip"))
            {
                return Err(Error("unknown identity check or policy value".into()));
            }
            settings[k] = v.clone();
        }
    }
    if settings.as_object().unwrap().values().any(|v| v == "skip")
        && store
            .db
            .prepare("SELECT 1 FROM generated_locations WHERE profile=? AND location=?")?
            .exists([profile, source])?
    {
        return Err(Error(
            "generated artifact locations require strict identity".into(),
        ));
    }
    let apply = args["apply"].as_bool().unwrap_or(false);
    let changed = old["preset"] != preset || old["settings"] != settings;
    let mut revision = old["revision"].as_i64().unwrap();
    if apply && changed {
        recovered(store)?;
        if store
            .db
            .prepare("SELECT 1 FROM consumer_deliveries WHERE profile=? AND lease_until>?")?
            .exists(params![profile, now()])?
        {
            return Err(Error(
                "consumer delivery is in flight; retry after its lease ends".into(),
            ));
        }
        revision += 1;
        store.transaction(|db|{db.execute("INSERT INTO source_identity_policies(profile,location,identity_policy,settings,revision) VALUES(?,?,?,?,?) ON CONFLICT(profile,location) DO UPDATE SET identity_policy=excluded.identity_policy,settings=excluded.settings,revision=excluded.revision",params![profile,source,preset,encode(&settings),revision])?;db.execute("INSERT INTO source_policy_history(profile,location,revision,settings) VALUES(?,?,?,?)",params![profile,source,revision,encode(&json!({"preset":preset,"settings":settings}))])?;Ok(())})?;
    }
    let history = rows(
        &store.db,
        "SELECT revision,settings,created_at FROM source_policy_history WHERE profile=? AND location=? ORDER BY revision",
        &[profile.to_string().into(), source.to_string().into()],
    )?;
    Ok(
        json!({"source":source,"root":binding["root"],"previous_identity_policy":old["preset"],"identity_policy":preset,"settings":settings,"revision":revision,"applied":apply,"complete":true,"precedence":"strict default < stored source policy < explicit preset reset < explicit check settings","scope":"source root identity only; file versions, availability, output ownership and state correctness remain required","history":history}),
    )
}
fn default_extensions() -> Value {
    let mut extensions=".mkv .mp4 .m4v .avi .mov .webm .mpg .mpeg .mpe .m2v .m2ts .mts .ts .vob .ogv .wmv .asf .flv .f4v .3gp .3g2 .divx .rm .rmvb .mxf .mp3 .flac .wav .wave .aac .m4a .m4b .m4p .mka .ogg .oga .opus .aif .aiff .alac .ape .wv .wma .dsf .dff .ac3 .eac3 .dts .mid .midi .amr .au .srt .ass .ssa .vtt .sub .idx .sup .smi .sami .ttml .dfxp .scc .lrc .jpg .jpeg .png .gif .webp .bmp .tif .tiff .heic .heif .avif .jxl .svg .raw .dng .cr2 .cr3 .nef .arw .orf .rw2 .raf .pef .srw .psd .epub .pdf .mobi .azw .azw3 .azw4 .fb2 .djvu .cbz .cbr .cb7 .cbt .txt .md .rtf .doc .docx .odt .html .htm .nfo .xml .xmp .cue .m3u .m3u8 .pls .asx .log .ttf .otf .woff .woff2 .iso .img .bin .dat .ifo .bup .bdmv .mpls .clpi .zip .rar .7z".split_whitespace().collect::<Vec<_>>();
    extensions.sort();
    json!(extensions)
}
fn defaults() -> Value {
    json!({"directory_entries":50000,"depth":64,"directory_seconds":15,"metadata_seconds":2,"errors":16,"total_seconds":120,"total_entries":250000,"pending_directories":100000,"batch_records":500,"batch_bytes":1048576,"batch_seconds":1,"temporary_bytes":67108864,"minimum_free_bytes":67108864})
}
fn budgets(value: Option<&Value>) -> Result<Value> {
    let mut result = defaults();
    if let Some(value) = value {
        let object = value
            .as_object()
            .ok_or_else(|| Error("unknown scan budget".into()))?;
        for (k, v) in object {
            if result.get(k).is_none() {
                return Err(Error("unknown scan budget".into()));
            }
            let n = v
                .as_f64()
                .filter(|n| n.is_finite() && *n > 0.0 && *n < 1e12)
                .ok_or_else(|| Error("scan budgets must be finite positive numbers".into()))?;
            if !matches!(
                k.as_str(),
                "directory_seconds" | "metadata_seconds" | "total_seconds" | "batch_seconds"
            ) && v.as_u64().is_none()
            {
                return Err(Error("scan count and byte budgets must be integers".into()));
            }
            result[k] = v.clone();
            let _ = n;
        }
    }
    if result["batch_records"].as_u64().unwrap() > 10000
        || result["batch_bytes"].as_u64().unwrap() > 16777216
    {
        return Err(Error("scan batch exceeds hard memory allowance".into()));
    }
    Ok(result)
}
fn paths(value: &Value, empty: bool) -> Result<Vec<String>> {
    let rows = value
        .as_array()
        .ok_or_else(|| Error("paths must be a list".into()))?;
    let mut result = BTreeSet::new();
    for path in rows {
        let path = path
            .as_str()
            .ok_or_else(|| Error("path must be text".into()))?;
        if !(empty && path.is_empty()) {
            crate::layout::relative_path(path)?;
        }
        result.insert(path.to_string());
    }
    Ok(result.into_iter().collect())
}
fn normalize_extensions(value: &Value) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let list = value
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 1000)
        .ok_or_else(|| Error("extensions must be a nonempty list or null for all files".into()))?;
    let mut result = BTreeSet::new();
    for v in list {
        let s = v.as_str().ok_or_else(|| {
            Error("extensions must be suffixes such as .mkv; globs are not supported".into())
        })?;
        if !s.starts_with('.')
            || s.len() < 2
            || s.len() > 21
            || !s[1..].chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(Error(
                "extensions must be suffixes such as .mkv; globs are not supported".into(),
            ));
        }
        result.insert(s.to_lowercase());
    }
    Ok(json!(result))
}
fn effective_policy(store: &Store, profile: &str, source: &str) -> Result<Value> {
    let rows = rows(
        &store.db,
        "SELECT * FROM source_observation_policies WHERE profile=? AND source=?",
        &[profile.to_string().into(), source.to_string().into()],
    )?;
    let row = rows.first();
    let raw: Value =
        catabolic_core::decode_json(row.and_then(|r| r["policy"].as_str()).unwrap_or("{}"))?;
    Ok(
        json!({"revision":row.map(|r|r["revision"].clone()).unwrap_or(json!(0)),"exclusions":raw.get("exclusions").cloned().unwrap_or(json!([])),"extensions":normalize_extensions(raw.get("extensions").unwrap_or(&default_extensions()))?,"include_regex":raw.get("include_regex").cloned().unwrap_or(json!([])),"exclude_regex":raw.get("exclude_regex").cloned().unwrap_or(json!([])),"budgets":budgets(raw.get("budgets"))?}),
    )
}
fn configure_policy(store: &Store, profile: &str, source: &str, args: &Value) -> Result<Value> {
    binding(store, profile, "source", source)?;
    let old = effective_policy(store, profile, source)?;
    let Some(policy) = args.get("policy") else {
        return Ok(old);
    };
    let object = policy
        .as_object()
        .ok_or_else(|| Error("invalid observation policy".into()))?;
    if object.keys().any(|k| {
        !matches!(
            k.as_str(),
            "exclusions" | "extensions" | "include_regex" | "exclude_regex" | "budgets"
        )
    }) {
        return Err(Error("invalid observation policy".into()));
    }
    let mut new = old.clone();
    new.as_object_mut().unwrap().remove("revision");
    for (k, v) in object {
        new[k] = v.clone();
    }
    new["exclusions"] = json!(paths(&new["exclusions"], false)?);
    new["extensions"] = normalize_extensions(&new["extensions"])?;
    new["budgets"] = budgets(Some(&new["budgets"]))?;
    new["include_regex"] = patterns(&new["include_regex"])?;
    new["exclude_regex"] = patterns(&new["exclude_regex"])?;
    let changed = new.as_object().unwrap().iter().any(|(k, v)| old[k] != *v);
    let revision = old["revision"].as_i64().unwrap() + i64::from(changed);
    let apply = args["apply"].as_bool().unwrap_or(false);
    if apply && changed {
        store.transaction(|db|{db.execute("INSERT INTO source_observation_policies VALUES(?,?,?,?) ON CONFLICT(profile,source) DO UPDATE SET revision=excluded.revision,policy=excluded.policy",params![profile,source,revision,encode(&new)])?;db.execute("INSERT INTO source_observation_policy_history(profile,source,revision,policy) VALUES(?,?,?,?)",params![profile,source,revision,encode(&new)])?;db.execute("INSERT INTO observation_sources VALUES(?,?,1) ON CONFLICT(profile,source) DO UPDATE SET dirty_generation=dirty_generation+1",[profile,source])?;dirty_watchers(db,profile,None)?;Ok(())})?;
    }
    Ok(json!({"previous":old,"policy":new,"revision":revision,"applied":apply}))
}
fn compile_pattern(pattern: &str) -> Result<fancy_regex::Regex> {
    // The qualified VERSION0 domain includes lookaround and backreferences.
    // Other regex-module dialects are refused, never silently approximated.
    if ["(?V", "(?r", "(?f", "(?e", "(?b", r"\K", r"\X", "(?|"]
        .iter()
        .any(|syntax| pattern.contains(syntax))
    {
        return Err(Error("native scan regex construct is not qualified".into()));
    }
    let mut translated = String::new();
    let mut chars = pattern.chars();
    while let Some(character) = chars.next() {
        translated.push(character);
        if character == '\\'
            && let Some(next) = chars.next()
        {
            // Python regex VERSION0 uses a strict end anchor. Fancy's \Z
            // instead accepts trailing newlines, so use its strict \z.
            translated.push(if next == 'Z' { 'z' } else { next });
        }
    }
    fancy_regex::RegexBuilder::new(&translated)
        .backtrack_limit(100_000)
        .seek(false)
        .delegate_size_limit(1_048_576)
        .delegate_dfa_size_limit(1_048_576)
        .build()
        .map_err(|e| Error(format!("invalid scan regex: {e}")))
}
fn patterns(value: &Value) -> Result<Value> {
    let values = value
        .as_array()
        .filter(|v| v.len() <= 32)
        .ok_or_else(|| Error("regex filters must be lists of at most 32 patterns".into()))?;
    let mut normalized = BTreeSet::new();
    for value in values {
        let pattern = value
            .as_str()
            .filter(|v| (1..=1024).contains(&v.chars().count()))
            .ok_or_else(|| Error("regex patterns must contain 1 to 1024 characters".into()))?;
        compile_pattern(pattern)?;
        normalized.insert(pattern);
    }
    Ok(json!(normalized))
}
#[derive(Clone)]
struct ScanFilter {
    extensions: Value,
    include: Vec<fancy_regex::Regex>,
    exclude: Vec<fancy_regex::Regex>,
}
impl ScanFilter {
    fn new(execution: &Value) -> Result<Self> {
        let compile = |key: &str| -> Result<Vec<fancy_regex::Regex>> {
            let normalized = patterns(&execution[key])?;
            normalized
                .as_array()
                .unwrap()
                .iter()
                .map(|value| compile_pattern(value.as_str().unwrap()))
                .collect()
        };
        Ok(Self {
            extensions: execution["extensions"].clone(),
            include: compile("include_regex")?,
            exclude: compile("exclude_regex")?,
        })
    }
    fn matches(&self, path: &str) -> Result<bool> {
        if !included(path, &self.extensions) {
            return Ok(false);
        }
        let started = Instant::now();
        let any = |patterns: &[fancy_regex::Regex]| -> Result<bool> {
            for pattern in patterns {
                if started.elapsed() >= Duration::from_millis(10) {
                    return Err(Error("regex_filter_timeout".into()));
                }
                let matched = pattern
                    .is_match(path)
                    .map_err(|_| Error("regex_filter_timeout".into()))?;
                if started.elapsed() >= Duration::from_millis(10) {
                    return Err(Error("regex_filter_timeout".into()));
                }
                if matched {
                    return Ok(true);
                }
            }
            Ok(false)
        };
        Ok((self.include.is_empty() || any(&self.include)?) && !any(&self.exclude)?)
    }
}
fn included(path: &str, extensions: &Value) -> bool {
    extensions.is_null()
        || Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                extensions
                    .as_array()
                    .is_some_and(|list| list.contains(&json!(format!(".{}", ext.to_lowercase()))))
            })
}
pub(crate) fn header_check(file: &File, path: &str, expected: &Value) -> Result<Value> {
    let size = expected["size"].as_u64().unwrap_or(0);
    let lower = path.to_lowercase();
    let reason = if size == 0 {
        Some("empty")
    } else if [
        ".!qb",
        ".aria2",
        ".crdownload",
        ".download",
        ".incomplete",
        ".part",
        ".partial",
        ".tmp",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
    {
        Some("incomplete_download")
    } else {
        None
    };
    if let Some(reason) = reason {
        return Ok(json!({"status":"invalid","reason":reason,"bytes_read":0}));
    }
    if ![
        ".mkv", ".mka", ".webm", ".avi", ".wav", ".wave", ".flac", ".ogg", ".oga", ".ogv", ".opus",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
    {
        return Ok(json!({"status":"unknown","reason":"unsupported_header","bytes_read":0}));
    }
    if version(&file.metadata()?) != *expected {
        return Err(Error("source changed before checking media header".into()));
    }
    let mut raw = [0u8; 64];
    let count = file.read_at(&mut raw, 0)?;
    if count != size.min(64) as usize || version(&file.metadata()?) != *expected {
        return Err(Error("source changed while checking media header".into()));
    }
    let invalid = raw[..count].iter().all(|b| *b == 0);
    Ok(
        json!({"status":if invalid{"invalid"}else{"not_detected"},"reason":if invalid{Some("zero_filled_header")}else{None},"bytes_read":count}),
    )
}
pub(crate) fn dirty_watchers(db: &Connection, profile: &str, job: Option<&str>) -> Result<()> {
    db.execute("UPDATE watchers SET pending_generation=pending_generation+1,event_first=coalesce(event_first,unixepoch('now')),event_last=unixepoch('now') WHERE profile=? AND enabled=1 AND definition_id IN(SELECT id FROM watcher_definitions WHERE json_extract(definition,'$.events')=1) AND NOT EXISTS(SELECT 1 FROM watcher_runs r JOIN observation_requests q ON q.requester='watcher:'||r.definition_id||':'||r.generation WHERE r.id=watchers.active_run AND r.state='observing' AND q.job_id=?)",params![profile,job])?;
    Ok(())
}
fn enqueue(db: &Connection, profile: &str, job: &str) -> Result<()> {
    crate::catalog_refresh::enqueue(db, profile, Some(job))
}

fn compatibility_for(
    store: &Store,
    profile: &str,
    source: &str,
    excluded: &Value,
    execution: &Value,
    require_complete: bool,
) -> Result<String> {
    Ok(sha256(encode(
        &json!({"binding":rows(&store.db,"SELECT * FROM bindings WHERE profile=? AND kind='source' AND owner=?",&[profile.to_string().into(),source.to_string().into()])?,"volume":rows(&store.db,"SELECT * FROM binding_volumes WHERE profile=? AND kind='source' AND owner=?",&[profile.to_string().into(),source.to_string().into()])?,"policy":trust_policy(store,profile,source)?,"exclusions":excluded,"method":"guarded-inventory-v2","observation_policy":effective_policy(store,profile,source)?,"execution":execution,"require_complete":require_complete}),
    )))
}
fn recover(store: &Store) -> Result<Value> {
    let jobs = rows(
        &store.db,
        "SELECT * FROM observation_jobs WHERE state='running'",
        &[],
    )?;
    let mut stale = Vec::new();
    for job in jobs {
        let id = job["id"].as_str().unwrap();
        // The closed protocol may share demand with an existing owner, but it
        // may not turn flock availability, lease expiry or ESRCH into cleanup.
        if crate::source::children_forbidden()
            && !crate::native_observation_companion::has_cleanup_witness(store, id)?
        {
            continue;
        }
        let Ok(parsed) = uuid::Uuid::parse_str(id) else {
            return Err(Error("invalid observation identity".into()));
        };
        let path = store.path.with_file_name(format!(
            "{}.observation-{parsed}",
            store.path.file_name().unwrap().to_string_lossy()
        ));
        let _guard = if job["guarded"] == 1 {
            let Ok(guard) = crate::writer_lock(&path) else {
                continue;
            };
            Some(guard)
        } else {
            let Some(pid) = job["worker_pid"]
                .as_i64()
                .filter(|pid| (1..=i64::from(i32::MAX)).contains(pid))
            else {
                continue;
            };
            // SAFETY: signal zero only tests process existence and sends no signal.
            // Permission denial is evidence of a possible live owner, not death.
            if unsafe { libc::kill(pid as i32, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
            {
                continue;
            }
            None
        };
        store.transaction(|db|{db.execute("UPDATE observation_jobs SET state='stale',lease_until=0,completed_at=? WHERE id=? AND state='running'",params![now(),id])?;Ok(())})?;
        stale.push(id.to_string());
    }
    Ok(json!({"stale":stale}))
}

/// Immutable identity/execution context shared by traversal and publication.
struct PublicationContext {
    path: PathBuf,
    database_id: String,
    profile: String,
    source: String,
    job: String,
    scan: String,
    claim: String,
    generation: i64,
    root: Option<File>,
    database_identity: (u64, u64),
    execution: Value,
    filter: ScanFilter,
    generated: bool,
    exclusions: Value,
    compatibility: String,
}

/// Persistent native publication state. No Python callback runs while this
/// state's mutex is held; both the scanner and public adapter use this owner.
struct Publication {
    context: Arc<PublicationContext>,
    root_identity: Option<(u64, u64)>,
    sequence: i64,
    entries: Vec<Value>,
    bytes: usize,
    last: Instant,
    scopes: Vec<(String, String, Value)>,
    initial_scopes: Option<Connection>,
    blocker: Option<String>,
    // Traversal has already applied the filter; standalone Publishers have not.
    prefiltered_entries: bool,
    // Preserve the unqualified callback/generated schedule until its own slice.
    eager_scopes: bool,
}
impl Deref for Publication {
    type Target = PublicationContext;
    fn deref(&self) -> &Self::Target {
        &self.context
    }
}
#[derive(Clone)]
pub struct Publisher(Arc<Mutex<Publication>>);

impl Publisher {
    fn with<T>(&self, action: impl FnOnce(&mut Publication) -> Result<T>) -> Result<T> {
        let mut owner = self
            .0
            .try_lock()
            .map_err(|_| Error("native publisher is already in use".into()))?;
        action(&mut owner)
    }
    fn snapshot(store: &Store, job: &str) -> Result<Connection> {
        let snapshot = Connection::open("")?;
        snapshot.execute_batch("CREATE TABLE scopes(path,state,detail)")?;
        let mut statement = store.db.prepare("SELECT path,CASE WHEN state='running' THEN 'pending' ELSE state END,detail FROM observation_scopes WHERE job_id=?")?;
        let mut cursor = statement.query([job])?;
        while let Some(row) = cursor.next()? {
            snapshot.execute(
                "INSERT INTO scopes VALUES(?,?,?)",
                params![
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?
                ],
            )?;
        }
        Ok(snapshot)
    }
    pub fn new(store: &Store, profile: &str, job: &Value) -> Result<Self> {
        let field = |name: &str| {
            job[name]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error(format!("publisher job requires {name}")))
        };
        let execution = catabolic_core::decode_json(&field("execution")?)?;
        // Reject malformed caller jobs at the boundary instead of allowing a
        // panic inside a checkpoint. Admission remains the observation owner's.
        for key in ["batch_records", "batch_bytes"] {
            if execution["budgets"][key].as_u64().is_none() {
                return Err(Error(format!("invalid publisher {key}")));
            }
        }
        if execution["budgets"]["batch_seconds"].as_f64().is_none() {
            return Err(Error("invalid publisher batch_seconds".into()));
        }
        let source = field("source")?;
        let id = field("id")?;
        let sequence = store.db.query_row(
            "SELECT coalesce(max(sequence),0) FROM observation_batches WHERE job_id=?",
            [&id],
            |row| row.get(0),
        )?;
        let context = Arc::new(PublicationContext {
            path: store.path.clone(),
            database_id: store.database_id.clone(),
            profile: profile.to_owned(),
            source: source.clone(),
            job: id.clone(),
            scan: field("scan_id")?,
            claim: field("claim_token")?,
            generation: job["generation"]
                .as_i64()
                .ok_or_else(|| Error("invalid observation generation".into()))?,
            root: None,
            database_identity: identity(&std::fs::metadata(&store.path)?),
            filter: ScanFilter::new(&execution)?,
            execution,
            generated: store
                .db
                .prepare("SELECT 1 FROM generated_locations WHERE profile=? AND location=?")?
                .exists([profile, &source])?,
            exclusions: catabolic_core::decode_json(&field("exclusions")?)?,
            compatibility: field("compatibility")?,
        });
        Ok(Self(Arc::new(Mutex::new(Publication {
            context,
            root_identity: None,
            sequence,
            entries: vec![],
            scopes: vec![],
            bytes: 0,
            last: Instant::now(),
            initial_scopes: Some(Self::snapshot(store, &id)?),
            blocker: None,
            prefiltered_entries: false,
            eager_scopes: false,
        }))))
    }
    pub fn initial_scopes(&self, after: i64) -> Result<Value> {
        self.with(|p| {
            let mut result = rows(p.initial_scopes.as_ref().ok_or_else(|| Error("publisher snapshot unavailable".into()))?, "SELECT rowid AS cursor,path,state,detail FROM scopes WHERE rowid>? ORDER BY rowid LIMIT 256", &[after.into()])?;
            let cursor = result.last().and_then(|row| row["cursor"].as_i64()).unwrap_or(after);
            for row in &mut result {
                row.as_object_mut().unwrap().remove("cursor");
                row["detail"] = catabolic_core::decode_json(row["detail"].as_str().unwrap())?;
            }
            Ok(json!({"rows":result,"after":cursor}))
        })
    }
    pub fn state(&self) -> Result<Value> {
        self.with(|p| Ok(json!({"entries": p.entries, "scopes": p.scopes.iter().map(|(path,state,detail)| (path.clone(),json!([state,detail]))).collect::<serde_json::Map<_,_>>(), "bytes":p.bytes,"sequence":p.sequence,"blocker":p.blocker,"root_identity":p.root_identity,"execution":p.execution})))
    }
    pub fn set_root_identity(&self, value: Option<(u64, u64)>) -> Result<()> {
        self.with(|p| {
            p.root_identity = value;
            Ok(())
        })
    }
    pub fn sequence(&self) -> Result<i64> {
        self.with(|p| Ok(p.sequence))
    }
    fn append_to(p: &mut Publication, entry: Value) -> Result<bool> {
        let path = entry["path"]
            .as_str()
            .ok_or_else(|| Error("publisher entry requires path".into()))?;
        if entry["status"] == "missing" && !p.generated {
            return Err(Error("native public Publisher explicit missing entries for non-generated sources are unqualified".into()));
        }
        if !p.prefiltered_entries && !p.filter.matches(path)? {
            return Ok(false);
        }
        p.bytes += encode(&entry).len();
        p.entries.push(entry);
        Ok(true)
    }
    pub fn append(&self, entry: Value) -> Result<bool> {
        self.with(|p| Self::append_to(p, entry))
    }
    pub fn entry(&self, entry: Value) -> Result<bool> {
        self.with(|p| Ok(Self::append_to(p, entry)? && p.checkpoint()))
    }
    pub fn directory(&self, path: &str, state: &str, detail: &Value) -> Result<bool> {
        self.with(|p| {
            if !detail.is_object() {
                return Err(Error("publisher scope detail must be an object".into()));
            }
            if !p.eager_scopes {
                p.bytes += path.len() + encode(detail).len();
            }
            if let Some(row) = p.scopes.iter_mut().find(|row| row.0 == path) {
                *row = (path.to_owned(), state.to_owned(), detail.clone());
            } else {
                p.scopes
                    .push((path.to_owned(), state.to_owned(), detail.clone()));
            }
            Ok(p.eager_scopes || state == "running")
        })
    }
    pub fn checkpoint(&self) -> Result<bool> {
        self.with(|p| Ok(p.checkpoint()))
    }
    pub fn stop(&self, reason: &str) -> Result<()> {
        self.with(|p| {
            p.blocker = Some(reason.into());
            p.entries.clear();
            p.scopes.clear();
            p.bytes = 0;
            Ok(())
        })
    }
    pub fn flush(&self, store: Option<&Store>) -> Result<()> {
        self.with(|p| p.flush(store))
    }
}

/// Transport-only callbacks for an embedding's ordinary Publisher object.
/// Implementations must preserve the supplied owner and return callback errors.
pub trait PublisherCallbacks: Send {
    fn directory(&mut self, path: &str, state: &str, detail: &Value) -> Result<()>;
    fn entry(&mut self, entry: Value) -> Result<()>;
    fn failed(&self) -> bool;
    fn exceptional(&self) -> bool {
        self.failed()
    }
}
pub trait PublisherFactory: Send {
    fn create(
        &mut self,
        publisher: Publisher,
        job: Value,
        binding: Value,
    ) -> Result<Box<dyn PublisherCallbacks>>;
}
// statvfs block counts and fragment sizes have platform-dependent widths.
// Convert both to byte-counter width before multiplication can saturate.
fn available_bytes(blocks: impl Into<u64>, fragment_size: impl Into<u64>) -> u64 {
    blocks.into().saturating_mul(fragment_size.into())
}

struct Scan {
    context: Arc<PublicationContext>,
    publisher: Publisher,
    callbacks: Option<Box<dyn PublisherCallbacks>>,
    queue: Connection,
    queue_file: tempfile::NamedTempFile,
    started: Instant,
    visited: u64,
    directories: u64,
    blocker: Option<String>,
    errors: Vec<String>,
    crash_after_scope: Option<String>,
    statistics: Statistics,
}
impl Deref for Scan {
    type Target = PublicationContext;
    fn deref(&self) -> &Self::Target {
        &self.context
    }
}
impl Scan {
    fn record_outcome(&mut self, outcome: Result<()>) {
        let outcome = outcome.and_then(|()| {
            self.publisher.with(|p| {
                p.blocker
                    .clone()
                    .map_or(Ok(()), |reason| Err(Error(reason)))
            })
        });
        if let Err(error) = outcome {
            let _ = self.publisher.stop(&error.0);
            self.blocker = Some(error.0.clone());
            self.errors.push(error.0);
        }
    }

    fn scope(
        &mut self,
        store: Option<&Store>,
        path: &str,
        state: &str,
        detail: &Value,
    ) -> Result<()> {
        if let Some(callbacks) = &mut self.callbacks {
            callbacks.directory(path, state, detail)?;
        } else if self.publisher.directory(path, state, detail)? || self.publisher.checkpoint()? {
            self.publisher.flush(store)?;
        }
        self.queue.execute("INSERT INTO queue VALUES(?,?,?) ON CONFLICT(path) DO UPDATE SET state=excluded.state,detail=excluded.detail",params![path,state,encode(detail)])?;
        if state == "complete" && self.crash_after_scope.as_deref() == Some(path) {
            if self.callbacks.is_none() {
                self.publisher.flush(store)?;
            }
            std::process::exit(86);
        }
        Ok(())
    }
    fn publish(&mut self, store: Option<&Store>, entry: Value) -> Result<()> {
        if let Some(callbacks) = &mut self.callbacks {
            callbacks.entry(entry)
        } else {
            if self.publisher.entry(entry)? {
                self.publisher.flush(store)?;
            }
            Ok(())
        }
    }
    fn writer(&self) -> Result<Store> {
        self.publisher.with(|p| p.writer())
    }
    fn fence(&self, store: &Store) -> Result<()> {
        self.publisher.with(|p| p.fence(store))
    }
    fn generated_entry(&self, path: &str) -> Result<Value> {
        self.publisher.with(|p| p.generated_entry(path))
    }
    fn callback_failed(&self) -> bool {
        self.callbacks.as_ref().is_some_and(|c| c.failed())
    }
    fn resources(&self) -> Result<()> {
        let limits = &self.execution["budgets"];
        let elapsed = self.started.elapsed().as_secs_f64();
        self.statistics.elapsed_seconds.set(Some(elapsed));
        if elapsed >= limits["total_seconds"].as_f64().unwrap() {
            return Err(Error("total_time_budget".into()));
        }
        if self.visited > limits["total_entries"].as_u64().unwrap() {
            return Err(Error("total_entry_budget".into()));
        }
        // The persistent scope table doubles as the queue. Its cardinality is
        // bounded independently of file count; no entire directory is buffered.
        if self.directories > limits["pending_directories"].as_u64().unwrap() {
            return Err(Error("directory_queue_budget".into()));
        }
        let parent = File::open(self.queue_file.path().parent().unwrap())?;
        let pages: i64 = self
            .queue
            .pragma_query_value(None, "page_count", |r| r.get(0))?;
        let page_size: i64 = self
            .queue
            .pragma_query_value(None, "page_size", |r| r.get(0))?;
        self.statistics
            .temporary_bytes
            .set(Some(pages.saturating_mul(page_size) as u64));
        if pages.saturating_mul(page_size) as u64 > limits["temporary_bytes"].as_u64().unwrap() {
            return Err(Error("temporary_storage_budget".into()));
        }
        // SAFETY: the statvfs buffer is valid and parent owns a live descriptor.
        let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: fstatvfs writes only to the valid result buffer.
        if unsafe { libc::fstatvfs(parent.as_raw_fd(), &mut stats) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let free_bytes = available_bytes(stats.f_bavail, stats.f_frsize);
        self.statistics.temporary_free_bytes.set(Some(free_bytes));
        if free_bytes < limits["minimum_free_bytes"].as_u64().unwrap() {
            return Err(Error("temporary_storage_low".into()));
        }
        Ok(())
    }

    fn walk_generated(&mut self, store: Option<&Store>) -> Result<()> {
        let mut after = String::new();
        loop {
            self.resources()?;
            let path: Option<String> = self
                .queue
                .query_row(
                    "SELECT path FROM artifacts WHERE path>? ORDER BY path LIMIT 1",
                    [&after],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(path) = path else { break };
            after = path.clone();
            self.visited += 1;
            self.resources()?;
            if excluded(&path, &self.exclusions) || !self.filter.matches(&path)? {
                continue;
            }
            self.publish(store, self.generated_entry(&path)?)?;
        }
        // Registry coverage cannot authorize directory-wide absence.
        self.scope(store, "", "registry_complete", &json!({}))?;
        Ok(())
    }

    fn walk(&mut self, store: Option<&Store>) -> Result<()> {
        if self.generated {
            return self.walk_generated(store);
        }
        loop {
            self.resources()?;
            self.ordinary_checkpoint(store)?;
            let row = rows(
                &self.queue,
                "SELECT path,detail FROM queue WHERE state='pending' ORDER BY rowid LIMIT 1",
                &[],
            )?;
            let Some(row) = row.first() else { break };
            let path = row["path"].as_str().unwrap().to_string();
            let prior: Value = catabolic_core::decode_json(row["detail"].as_str().unwrap())?;
            if excluded(&path, &self.exclusions) {
                self.scope(
                    store,
                    &path,
                    "excluded",
                    &json!({"reason":"configured_exclusion"}),
                )?;
                continue;
            }
            let depth = if path.is_empty() {
                0
            } else {
                path.split('/').count() as u64
            };
            let depth_budget = self.execution["budgets"]["depth"].as_u64().unwrap();
            if depth > depth_budget {
                self.scope(
                    store,
                    &path,
                    "deferred",
                    &json!({"reason":"depth_budget","depth":depth,"limit":depth_budget}),
                )?;
                continue;
            }
            let fd = match reopen(self.root.as_ref().unwrap(), &path, prior.get("identity")) {
                Ok(fd) => fd,
                Err(e) => {
                    self.errors.push(format!(
                        "{}: {}",
                        if path.is_empty() { "." } else { &path },
                        e.0
                    ));
                    self.scope(store, &path, "failed", &json!({"reason":e.0}))?;
                    continue;
                }
            };
            let before = version(&fd.metadata()?);
            let mut detail = json!({"identity":[before["device"],before["inode"]],"mtime_ns":before["mtime_ns"],"ctime_ns":before["ctime_ns"]});
            self.scope(store, &path, "running", &detail)?;
            let began = Instant::now();
            let mut count = 0u64;
            let mut failures = 0u64;
            let mut status = "complete";
            let mut reason = None;
            let enumeration = entries(&fd, |name| {
                count += 1;
                self.visited += 1;
                #[cfg(debug_assertions)]
                if let Ok(delay) = std::env::var("CATABOLIC_TEST_SCAN_ENTRY_DELAY_MS")
                    && let Ok(delay) = delay.parse::<u64>()
                {
                    std::thread::sleep(Duration::from_millis(delay.min(50)));
                }
                self.resources()?;
                if count
                    > self.execution["budgets"]["directory_entries"]
                        .as_u64()
                        .unwrap()
                {
                    status = "deferred";
                    reason = Some("directory_entry_budget".to_string());
                    return Ok(false);
                }
                if began.elapsed().as_secs_f64()
                    >= self.execution["budgets"]["directory_seconds"]
                        .as_f64()
                        .unwrap()
                {
                    status = "deferred";
                    reason = Some("directory_time_budget".to_string());
                    return Ok(false);
                }
                if count % 100 == 1 {
                    self.ordinary_checkpoint(store)?;
                }
                let child = if path.is_empty() {
                    name.to_string()
                } else {
                    format!("{path}/{name}")
                };
                if excluded(&child, &self.exclusions) {
                    self.scope(
                        store,
                        &child,
                        "excluded",
                        &json!({"reason":"configured_exclusion"}),
                    )?;
                    return Ok(true);
                }
                let tick = Instant::now();
                let result = (|| -> Result<Option<Value>> {
                    crate::layout::relative_path(&child)?;
                    let (mode, stamp) = stat_at(&fd, name)?;
                    if u64::from(mode) == u64::from(libc::S_IFDIR) {
                        let mut child_detail = json!({"identity":[stamp["device"],stamp["inode"]]});
                        if stamp["device"] != before["device"] {
                            child_detail["reason"] = json!("filesystem_boundary");
                            self.scope(store, &child, "failed", &child_detail)?;
                        } else {
                            if self.directories
                                >= self.execution["budgets"]["pending_directories"]
                                    .as_u64()
                                    .unwrap()
                            {
                                return Err(Error("directory_queue_budget".into()));
                            }
                            if !self
                                .queue
                                .prepare("SELECT 1 FROM queue WHERE path=?")?
                                .exists([&child])?
                            {
                                self.directories += 1;
                                self.scope(store, &child, "pending", &child_detail)?;
                            }
                        }
                        return Ok(None);
                    }
                    if u64::from(mode) != u64::from(libc::S_IFREG) {
                        return Ok(None);
                    }
                    if !self.filter.matches(&child)? {
                        detail["files_filtered"] =
                            json!(detail["files_filtered"].as_u64().unwrap_or(0) + 1);
                        return Ok(None);
                    }
                    let file = open_at(&fd, name, false)?;
                    if version(&file.metadata()?) != stamp {
                        return Err(Error("source changed before checking media header".into()));
                    }
                    let health = header_check(&file, &child, &stamp)?;
                    if stat_at(&fd, name)?.1 != stamp {
                        return Err(Error("source changed while checking media header".into()));
                    }
                    let mut entry = stamp;
                    entry.as_object_mut().unwrap().remove("ctime_ns");
                    entry["path"] = json!(child);
                    entry["media_header"] = health;
                    Ok(Some(entry))
                })();
                match result {
                    // A directory callback can fail inside the entry closure.
                    // Preserve its source-wide error before per-file aggregation.
                    Err(error) if self.callback_failed() => return Err(error),
                    Ok(Some(entry)) => self.publish(store, entry)?,
                    Ok(None) => {}
                    Err(e)
                        if matches!(
                            e.0.as_str(),
                            "directory_queue_budget" | "regex_filter_timeout"
                        ) =>
                    {
                        return Err(e);
                    }
                    Err(e) => {
                        failures += 1;
                        status = "failed";
                        reason = Some(e.0);
                    }
                }
                if self.callback_failed() {
                    return Err(Error("publisher callback failed".into()));
                }
                if tick.elapsed().as_secs_f64()
                    >= self.execution["budgets"]["metadata_seconds"]
                        .as_f64()
                        .unwrap()
                {
                    status = "deferred";
                    reason = Some("metadata_latency_budget".into());
                    return Ok(false);
                }
                if failures >= self.execution["budgets"]["errors"].as_u64().unwrap() {
                    status = "deferred";
                    detail["last_error"] = json!(reason);
                    reason = Some("directory_error_budget".into());
                    return Ok(false);
                }
                Ok(true)
            });
            if let Err(e) = enumeration {
                if self.callback_failed() {
                    return Err(e);
                }
                if matches!(
                    e.0.as_str(),
                    "total_time_budget"
                        | "total_entry_budget"
                        | "directory_queue_budget"
                        | "temporary_storage_low"
                        | "temporary_storage_budget"
                        | "regex_filter_timeout"
                ) {
                    return Err(e);
                }
                // Publication/fencing and storage failures must abort the source,
                // never masquerade as a local file error and authorize absence.
                let owned;
                let current = match store {
                    Some(store) => store,
                    None => {
                        owned = self.writer()?;
                        &owned
                    }
                };
                self.fence(current)?;
                status = "failed";
                reason = Some(e.0);
            }
            let after = version(&fd.metadata()?);
            if before["mtime_ns"] != after["mtime_ns"] || before["ctime_ns"] != after["ctime_ns"] {
                status = "deferred";
                reason = Some("directory_changed_during_scan".into());
            }
            detail["entries"] = json!(count);
            detail["seconds"] = json!(began.elapsed().as_secs_f64());
            detail["errors"] = json!(failures);
            if let Some(reason) = reason {
                detail["reason"] = json!(reason);
                if self.errors.len() < 1000 {
                    self.errors.push(format!(
                        "{}: {}",
                        if path.is_empty() { "." } else { &path },
                        reason.replace('_', " ")
                    ));
                }
            }
            self.scope(store, &path, status, &detail)?;
            if self.callbacks.is_none() {
                // The frozen walker flushes after each enumerated directory,
                // even when the scope checkpoint just committed the buffer.
                self.publisher.flush(store)?;
            }
        }
        if self.callbacks.is_none() {
            // The executor's final successful flush is a real fenced batch,
            // including when the preceding directory flush emptied the buffer.
            self.publisher.flush(store)?;
        }
        Ok(())
    }
    fn ordinary_checkpoint(&self, store: Option<&Store>) -> Result<()> {
        if self.callbacks.is_none() && self.publisher.checkpoint()? {
            self.publisher.flush(store)?;
        }
        Ok(())
    }
}
impl Publication {
    fn checkpoint(&self) -> bool {
        let limits = &self.execution["budgets"];
        self.entries.len() as u64
            + if self.eager_scopes {
                0
            } else {
                self.scopes.len() as u64
            }
            >= limits["batch_records"].as_u64().unwrap()
            || self.bytes as u64 >= limits["batch_bytes"].as_u64().unwrap()
            || self.last.elapsed().as_secs_f64() >= limits["batch_seconds"].as_f64().unwrap()
    }
    fn writer(&self) -> Result<Store> {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            match Store::open(&self.path, true, false) {
                Ok(store) if store.database_id == self.database_id => return Ok(store),
                Ok(_) => return Err(Error("catalog replaced during scan".into())),
                Err(e) if e.0.contains("another Catabolic writer") && Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => return Err(e),
            }
        }
    }
    fn fence(&self, store: &Store) -> Result<()> {
        if identity(&std::fs::metadata(&self.path)?) != self.database_identity {
            return Err(Error("catalog replaced during scan".into()));
        }
        let current = rows(&store.db,"SELECT j.*,s.dirty_generation AS current_dirty FROM observation_jobs j JOIN observation_sources s USING(profile,source) WHERE j.id=?", &[self.job.clone().into()])?.pop().ok_or_else(||Error("stale_observation_claim".into()))?;
        let generation = current["generation"]
            .as_i64()
            .ok_or_else(|| Error("invalid observation generation".into()))?;
        let state = lifecycle::State {
            generation,
            lease: if current["state"] == "running" {
                Some(lifecycle::Lease {
                    generation,
                    token: sha256(current["claim_token"].as_str().unwrap_or("")),
                    identity: sha256(encode(&json!([
                        current["compatibility"],
                        current["dirty_generation"]
                    ]))),
                    until: current["lease_until"].as_f64().unwrap_or(0.0),
                })
            } else {
                None
            },
        };
        lifecycle::reduce(
            &state,
            &lifecycle::Input::Fence {
                now: now(),
                generation: self.generation,
                token: sha256(&self.claim),
                identity: sha256(encode(&json!([
                    self.compatibility,
                    current["current_dirty"]
                ]))),
            },
        )
        .map_err(|_| Error("stale_observation_claim".into()))?;
        if compatibility_for(
            store,
            &self.profile,
            &self.source,
            &self.exclusions,
            &self.execution,
            false,
        )? != self.compatibility
            && compatibility_for(
                store,
                &self.profile,
                &self.source,
                &self.exclusions,
                &self.execution,
                true,
            )? != self.compatibility
        {
            return Err(Error("stale_observation_claim".into()));
        }
        let (root, _) = checked_root(
            store,
            &self.profile,
            &binding(store, &self.profile, "source", &self.source)?,
        )?;
        if Some(identity(&root.metadata()?)) != self.root_identity {
            return Err(Error("source root changed during scan".into()));
        }
        Ok(())
    }
    fn flush(&mut self, store: Option<&Store>) -> Result<()> {
        let owned;
        let store = match store {
            Some(store) => store,
            None => {
                owned = self.writer()?;
                &owned
            }
        };
        self.install_filter(store)?;
        self.fence(store)?;
        let context = self.context.clone();
        let opened;
        let root = if let Some(root) = context.root.as_ref() {
            root
        } else {
            opened = checked_root(
                store,
                &self.profile,
                &binding(store, &self.profile, "source", &self.source)?,
            )?
            .0;
            &opened
        };
        let mut actual_scopes = self.scopes.clone();
        for (path, state, detail) in &mut actual_scopes {
            if state != "complete" {
                continue;
            }
            let verification = (|| -> Result<()> {
                let current = reopen(root, path, Some(&detail["identity"]))?;
                let stamp = version(&current.metadata()?);
                if stamp["mtime_ns"] != detail["mtime_ns"]
                    || stamp["ctime_ns"] != detail["ctime_ns"]
                {
                    return Err(Error("directory_changed_before_publication".into()));
                }
                Ok(())
            })();
            if let Err(error) = verification {
                *state = "failed".into();
                detail["reason"] = json!(error.0);
            }
        }
        if self.generated {
            crate::artifact_mutation::owned_root(
                store,
                &self.profile,
                &binding(store, &self.profile, "source", &self.source)?,
            )?;
            for index in 0..self.entries.len() {
                if self.entries[index]["status"] == "missing" {
                    self.entries[index] = Self::generated_entry_at(
                        root,
                        self.entries[index]["path"].as_str().unwrap(),
                    )?;
                }
            }
        }
        let sequence = self.sequence + 1;
        let profile = &self.profile;
        let source = &self.source;
        store.transaction(|db|{
            self.fence(store)?;
            if db.prepare("SELECT 1 FROM observation_batches WHERE job_id=? AND sequence=?")?.exists(params![self.job,sequence])?{return Err(Error("duplicate_observation_batch".into()));}
            let mut changed=false;
            for (path,state,_) in &actual_scopes { if state=="running"{let prefix=if path.is_empty(){String::new()}else{format!("{path}/")};db.execute("DELETE FROM observation_seen WHERE job_id=? AND file_id IN(SELECT id FROM files WHERE location=? AND path>=? AND path<? AND instr(substr(path,?),'/')=0)",params![self.job,source,prefix,format!("{prefix}\u{10ffff}"),prefix.chars().count() as i64+1])?;}}
            for entry in &self.entries {
                let identifier=uuid::Uuid::new_v5(&uuid::Uuid::parse_str(&self.database_id).map_err(|_|Error("invalid catalog identity".into()))?,encode(&json!([source,entry["path"]])).as_bytes()).to_string();
                let old=rows(db,"SELECT * FROM observations WHERE profile=? AND file_id=?",&[profile.to_string().into(),identifier.clone().into()])?;
                let status=entry["status"].as_str().unwrap_or("present");
                changed|=old.first().is_none_or(|old|old["status"]!=status||["size","mtime_ns","device","inode"].iter().any(|k|old[k]!=entry[k]));
                db.execute("INSERT INTO files VALUES(?,?,?) ON CONFLICT(location,path) DO NOTHING",params![identifier,source,entry["path"].as_str()])?;
                db.execute("INSERT INTO observations VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(profile,file_id) DO UPDATE SET size=excluded.size,mtime_ns=excluded.mtime_ns,device=excluded.device,inode=excluded.inode,status=excluded.status,scan_id=excluded.scan_id",params![profile,identifier,entry["size"].as_i64(),entry["mtime_ns"].as_i64(),entry["device"].as_i64(),entry["inode"].as_i64(),status,self.scan])?;
                if status=="missing" {
                    continue;
                }
                let default_health=json!({"status":"unknown","reason":"header_not_checked","bytes_read":0});let health=entry.get("media_header").filter(|value| !value.is_null()).unwrap_or(&default_health);let old=rows(db,"SELECT status,reason FROM media_header_checks WHERE profile=? AND file_id=?",&[profile.to_string().into(),identifier.clone().into()])?;
                changed|=old.first().is_none_or(|old|old["status"]!=health["status"]||old["reason"]!=health["reason"]);
                if health["status"]=="invalid" {db.execute("UPDATE file_facts SET status='invalidated' WHERE profile=? AND file_id=?",params![profile,identifier])?;}
                db.execute("INSERT INTO media_header_checks VALUES(?,?,?,?,?,?,?,?,?,?) ON CONFLICT(profile,file_id) DO UPDATE SET scan_id=excluded.scan_id,size=excluded.size,mtime_ns=excluded.mtime_ns,device=excluded.device,inode=excluded.inode,status=excluded.status,reason=excluded.reason,bytes_read=excluded.bytes_read",params![profile,identifier,self.scan,entry["size"].as_i64(),entry["mtime_ns"].as_i64(),entry["device"].as_i64(),entry["inode"].as_i64(),health["status"].as_str(),health["reason"].as_str(),health["bytes_read"].as_i64()])?;
                db.execute("INSERT OR IGNORE INTO observation_seen VALUES(?,?)",params![self.job,identifier])?;
            }
            for (path,state,detail) in &actual_scopes {
                db.execute("INSERT INTO observation_scopes VALUES(?,?,?,?,?) ON CONFLICT(job_id,path) DO UPDATE SET state=excluded.state,detail=excluded.detail,updated_at=excluded.updated_at",params![self.job,path,state,encode(detail),now()])?;
                if state=="complete" {
                    let prefix=if path.is_empty(){String::new()}else{format!("{path}/")};
                    let count=db.execute("INSERT INTO observations(profile,file_id,size,mtime_ns,device,inode,status,scan_id) SELECT ?,f.id,0,0,0,0,'missing',? FROM files f WHERE scan_file_in_scope(f.path) AND f.location=? AND f.path>=? AND f.path<? AND (instr(substr(f.path,?),'/')=0 OR NOT EXISTS(SELECT 1 FROM observation_scopes child WHERE child.job_id=? AND child.path=substr(f.path,1,?+instr(substr(f.path,?),'/')-1))) AND NOT EXISTS(SELECT 1 FROM observation_seen s WHERE s.job_id=? AND s.file_id=f.id) ON CONFLICT(profile,file_id) DO UPDATE SET status='missing',scan_id=excluded.scan_id WHERE observations.status!='missing'",params![profile,self.scan,source,prefix,format!("{prefix}\u{10ffff}"),prefix.chars().count() as i64+1,self.job,prefix.chars().count() as i64,prefix.chars().count() as i64+1,self.job])?;changed|=count>0;
                }
            }
            db.execute("INSERT INTO observation_batches VALUES(?,?,?,?,?)",params![self.job,sequence,self.entries.len() as i64,self.bytes as i64,now()])?;
            db.execute("UPDATE scans SET observed=(SELECT count(*) FROM observation_seen WHERE job_id=?) WHERE id=?",params![self.job,self.scan])?;
            db.execute("UPDATE observation_jobs SET lease_until=? WHERE id=? AND claim_token=?",params![now()+3600.0,self.job,self.claim])?;
            if changed{enqueue(db,profile,&self.job)?;}Ok(())
        })?;
        self.sequence = sequence;
        self.entries.clear();
        self.scopes.clear();
        self.bytes = 0;
        self.last = Instant::now();
        Ok(())
    }
    fn install_filter(&self, store: &Store) -> Result<()> {
        let filter = self.filter.clone();
        let excluded_paths = self.exclusions.clone();
        let started = Instant::now();
        store.db.create_scalar_function(
            "scan_file_in_scope",
            1,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |ctx| {
                let path: String = ctx.get(0)?;
                if (!filter.include.is_empty() || !filter.exclude.is_empty())
                    && started.elapsed() > Duration::from_secs(1)
                {
                    return Err(rusqlite::Error::UserFunctionError(Box::new(Error(
                        "regex_finalization_budget".into(),
                    ))));
                }
                filter
                    .matches(&path)
                    .map(|included| included && !excluded(&path, &excluded_paths))
                    .map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))
            },
        )?;
        Ok(())
    }
    fn generated_entry(&self, path: &str) -> Result<Value> {
        Self::generated_entry_at(
            self.root
                .as_ref()
                .ok_or_else(|| Error("publisher root unavailable".into()))?,
            path,
        )
    }
    fn generated_entry_at(root: &File, path: &str) -> Result<Value> {
        let result = (|| {
            let (parent, leaf) = crate::filesystem_mutation::parent(root, path, false)?;
            let (mode, mut stamp) = stat_at(&parent, &leaf)?;
            if u64::from(mode) != u64::from(libc::S_IFREG) {
                return Err(Error("generated artifact is not a regular file".into()));
            }
            stamp.as_object_mut().unwrap().remove("ctime_ns");
            stamp["path"] = json!(path);
            stamp["media_header"] =
                json!({"status":"unknown","reason":"header_not_checked","bytes_read":0});
            Ok(stamp)
        })();
        match result {
            Err(error) if error.0.contains("(os error 2)") => Ok(
                json!({"path":path,"status":"missing","size":0,"mtime_ns":0,"device":0,"inode":0}),
            ),
            other => other,
        }
    }
}

fn excluded(path: &str, exclusions: &Value) -> bool {
    exclusions.as_array().is_some_and(|rows| {
        rows.iter()
            .filter_map(Value::as_str)
            .any(|p| path == p || path.starts_with(&format!("{p}/")))
    })
}
fn reopen(root: &File, path: &str, expected: Option<&Value>) -> Result<File> {
    let mut fd = root.try_clone()?;
    let device = root.metadata()?.dev();
    if !path.is_empty() {
        crate::layout::relative_path(path)?;
        for part in path.split('/') {
            fd = open_at(&fd, part, true)?;
            if fd.metadata()?.dev() != device {
                return Err(Error(
                    "nested filesystem boundary; register this mount as a separate location".into(),
                ));
            }
        }
    }
    let metadata = fd.metadata()?;
    if expected.is_some_and(|v| *v != json!([metadata.dev(), metadata.ino()])) {
        return Err(Error("directory replaced during scan".into()));
    }
    Ok(fd)
}
fn scan(
    store: &Store,
    profile: &str,
    source: &str,
    args: &Value,
    admitted: Option<&mut Option<Admitted>>,
) -> Result<Value> {
    if !store.owns_writer_lock() || !store.db.is_autocommit() {
        return Err(Error(
            "native scan requires a writer outside an active transaction".into(),
        ));
    }
    if let Some(request) = args["request_id"].as_str() {
        return resume_scan(
            store,
            profile,
            source,
            request,
            args,
            admitted,
            Reuse::Resumed,
        );
    }
    if args["after"].as_f64().is_some_and(|after| after > now()) {
        return Err(Error("observation_barrier".into()));
    }
    let continue_id = args
        .get("continue_id")
        .or(args.get("continue_request"))
        .and_then(Value::as_str);
    if args.get("scopes").is_some_and(|v| !v.is_null()) && continue_id.is_none() {
        return Err(Error(
            "--scope selects outstanding work; use --continue-request".into(),
        ));
    }
    let continuation = if let Some(id) = continue_id {
        let rows = rows(
            &store.db,
            "SELECT r.job_id,r.guarantees,j.state,j.execution,j.compatibility,j.exclusions,j.dirty_generation FROM observation_requests r JOIN observation_jobs j ON j.id=r.job_id WHERE r.id=? AND r.profile=?",
            &[id.to_string().into(), profile.to_string().into()],
        )?;
        let old = rows
            .first()
            .ok_or_else(|| Error("unknown_observation_request".into()))?
            .clone();
        if matches!(old["state"].as_str(), Some("queued" | "running")) {
            return Err(Error(
                "observation still active; resume the existing request".into(),
            ));
        }
        let old_execution = catabolic_core::decode_json(old["execution"].as_str().unwrap())?;
        let old_exclusions = catabolic_core::decode_json(old["exclusions"].as_str().unwrap())?;
        if compatibility_for(
            store,
            profile,
            source,
            &old_exclusions,
            &old_execution,
            catabolic_core::decode_json(old["guarantees"].as_str().unwrap())?["require_complete"]
                == true,
        )? != old["compatibility"].as_str().unwrap()
        {
            return Err(Error(
                "observation policy or binding changed; request a new scan".into(),
            ));
        }
        let watermark: i64 = store.db.query_row(
            "SELECT dirty_generation FROM observation_sources WHERE profile=? AND source=?",
            [profile, source],
            |r| r.get(0),
        )?;
        if watermark > old["dirty_generation"].as_i64().unwrap() {
            return Err(Error(
                "source changed since observation; request a new scan".into(),
            ));
        }
        if let Some(scopes) = args.get("scopes").filter(|v| !v.is_null()) {
            for path in paths(scopes, true)? {
                if !store.db.prepare("SELECT 1 FROM observation_scopes WHERE job_id=? AND path=? AND state NOT IN ('complete','excluded')")?.exists(params![old["job_id"].as_str(),path])?{return Err(Error("scope is not outstanding".into()));}
            }
        }
        Some(old)
    } else {
        None
    };
    binding(store, profile, "source", source)?;
    let policy = effective_policy(store, profile, source)?;
    ScanFilter::new(&policy)?;
    let mut limits = policy["budgets"].clone();
    if args["extended"] == true {
        for(k,v)in json!({"directory_entries":1000000,"depth":4096,"directory_seconds":300,"metadata_seconds":10,"errors":64,"total_seconds":1800,"total_entries":5000000}).as_object().unwrap(){limits[k]=v.clone();}
    }
    if let Some(overrides) = args.get("budgets") {
        for (k, v) in overrides
            .as_object()
            .ok_or_else(|| Error("unknown scan budget".into()))?
        {
            limits[k] = v.clone();
        }
    }
    limits = budgets(Some(&limits))?;
    let mut exclusions = paths(&policy["exclusions"], false)?;
    if let Some(exclude) = args.get("exclude") {
        exclusions.extend(paths(exclude, false)?);
    }
    exclusions.sort();
    exclusions.dedup();
    let exclusions = if let Some(old) = &continuation {
        catabolic_core::decode_json(old["exclusions"].as_str().unwrap())?
    } else {
        json!(exclusions)
    };
    let mut execution = json!({"budgets":limits,"scopes":[""],"policy_revision":policy["revision"],"extensions":policy["extensions"],"include_regex":policy["include_regex"],"exclude_regex":policy["exclude_regex"]});
    if let Some(old) = &continuation {
        let old_execution = catabolic_core::decode_json(old["execution"].as_str().unwrap())?;
        execution["scopes"] = old_execution["scopes"].clone();
        execution["continuation_of"] = old["job_id"].clone();
        execution["continuation_scopes"] = args
            .get("scopes")
            .filter(|v| !v.is_null())
            .map(|v| paths(v, true).map(|v| json!(v)))
            .transpose()?
            .unwrap_or(Value::Null);
    }
    let require_complete = if let Some(old) = &continuation {
        catabolic_core::decode_json(old["guarantees"].as_str().unwrap())?["require_complete"]
            == true
    } else {
        args["require_complete"].as_bool().unwrap_or(false)
    };
    let reuse_completed = continuation.is_none()
        && args["reuse_completed"]
            .as_bool()
            .unwrap_or(args["requester"].is_string());
    let max_age = args["max_age"].as_f64().unwrap_or(0.0);
    let after = args["after"].as_f64().unwrap_or(0.0);
    if !max_age.is_finite() || max_age < 0.0 || !after.is_finite() || after < 0.0 {
        return Err(Error("invalid_observation_freshness".into()));
    }
    let compatibility = compatibility_for(
        store,
        profile,
        source,
        &exclusions,
        &execution,
        require_complete,
    )?;
    if let Some(requester) = args["requester"].as_str() {
        let old = rows(
            &store.db,
            "SELECT id,guarantees FROM observation_requests WHERE profile=? AND source=? AND requester=?",
            &[
                profile.to_string().into(),
                source.to_string().into(),
                requester.to_string().into(),
            ],
        )?;
        if let Some(old) = old.first() {
            let expected = catabolic_core::decode_json(old["guarantees"].as_str().unwrap())?;
            if expected["exclusions"] != exclusions
                || expected["execution"] != execution
                || expected["max_age"].as_f64() != Some(max_age)
                || expected["require_complete"] != require_complete
                || expected["reuse_completed"] != reuse_completed
            {
                return Err(Error("observation_request_guarantees_changed".into()));
            }
            return resume_scan(
                store,
                profile,
                source,
                old["id"].as_str().unwrap(),
                &json!({"request_id":old["id"]}),
                admitted,
                Reuse::Resumed,
            );
        }
    }
    recover(store)?;
    let job = uuid::Uuid::new_v4().to_string();
    let scan_id = uuid::Uuid::new_v4().to_string();
    let claim = uuid::Uuid::new_v4().to_string();
    let request = uuid::Uuid::new_v4().to_string();
    let started = now();
    let guard_path = store.path.with_file_name(format!(
        "{}.observation-{job}",
        store.path.file_name().unwrap().to_string_lossy()
    ));
    let guard = crate::writer_lock(&guard_path)?;
    let barrier = if let Some(old) = &continuation {
        catabolic_core::decode_json(old["guarantees"].as_str().unwrap())?["minimum_start"]
            .as_f64()
            .unwrap()
    } else if reuse_completed {
        after.max(started - max_age)
    } else {
        after.max(started)
    };
    let parent_job = continuation.as_ref().and_then(|old| old["job_id"].as_str());
    let guarantees = json!({"minimum_start":barrier,"after":if continuation.is_some(){barrier}else{after},"max_age":max_age,"exclusions":exclusions,"reuse_completed":reuse_completed,"require_complete":require_complete,"coverage":"full_source","method":"guarded-inventory-v2","execution":execution,"parent_job":parent_job});
    if reuse_completed {
        let completed = rows(
            &store.db,
            "SELECT j.* FROM observation_jobs j JOIN observation_sources s USING(profile,source) WHERE j.profile=? AND j.source=? AND j.compatibility=? AND j.dirty_generation>=s.dirty_generation AND j.state IN('complete','unavailable') AND j.started_at>=? AND j.parent_job IS NULL ORDER BY j.generation DESC LIMIT 100",
            &[
                profile.to_string().into(),
                source.to_string().into(),
                compatibility.clone().into(),
                barrier.into(),
            ],
        )?;
        for old in completed {
            if require_complete && old["state"] != "complete" {
                continue;
            }
            let old_execution = catabolic_core::decode_json(old["execution"].as_str().unwrap())?;
            if old_execution["scopes"] != json!([""]) {
                continue;
            }
            store.transaction(|db|{db.execute("INSERT INTO observation_requests(id,profile,source,requester,guarantees,job_id,created_at) VALUES(?,?,?,?,?,?,?)",params![request,profile,source,args["requester"].as_str(),encode(&guarantees),old["id"].as_str(),started])?;Ok(())})?;
            return scan_result(
                store,
                old["id"].as_str().unwrap(),
                &request,
                &guarantees,
                catabolic_core::decode_json(old["report"].as_str().unwrap_or("{}"))?,
                Some(Reuse::Completed),
            );
        }
    }
    if continuation.is_none() {
        let watermark: i64 = store
            .db
            .query_row(
                "SELECT dirty_generation FROM observation_sources WHERE profile=? AND source=?",
                [profile, source],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let previous = rows(
            &store.db,
            "SELECT * FROM observation_jobs WHERE profile=? AND source=? AND compatibility=? AND dirty_generation>=? AND state='failed' ORDER BY generation DESC LIMIT 100",
            &[
                profile.to_string().into(),
                source.to_string().into(),
                compatibility.clone().into(),
                watermark.into(),
            ],
        )?;
        for old in previous {
            let report = catabolic_core::decode_json(old["report"].as_str().unwrap_or("{}"))?;
            if report["deferred"] != true {
                continue;
            }
            let resolved=store.db.prepare("WITH RECURSIVE descendants(id) AS(SELECT id FROM observation_jobs WHERE parent_job=? UNION ALL SELECT j.id FROM observation_jobs j JOIN descendants d ON j.parent_job=d.id) SELECT 1 FROM descendants d JOIN observation_jobs j ON j.id=d.id WHERE j.state='complete' LIMIT 1")?.exists([old["id"].as_str().unwrap()])?;
            if resolved {
                continue;
            }
            store.transaction(|db|{db.execute("INSERT INTO observation_requests(id,profile,source,requester,guarantees,job_id,created_at) VALUES(?,?,?,?,?,?,?)",params![request,profile,source,args["requester"].as_str(),encode(&guarantees),old["id"].as_str(),started])?;Ok(())})?;
            return scan_result(
                store,
                old["id"].as_str().unwrap(),
                &request,
                &guarantees,
                report,
                Some(Reuse::Deferred),
            );
        }
    }
    if continuation.is_none() {
        let inflight = rows(
            &store.db,
            "SELECT j.* FROM observation_jobs j JOIN observation_sources s USING(profile,source) WHERE j.profile=? AND j.source=? AND j.compatibility=? AND j.dirty_generation>=s.dirty_generation AND (j.state='queued' OR (j.state='running' AND j.lease_until>? AND j.started_at>=?)) ORDER BY j.generation DESC LIMIT 1",
            &[
                profile.to_string().into(),
                source.to_string().into(),
                compatibility.clone().into(),
                started.into(),
                barrier.into(),
            ],
        )?;
        if let Some(old) = inflight.first() {
            store.transaction(|db| { db.execute("INSERT INTO observation_requests(id,profile,source,requester,guarantees,job_id,created_at) VALUES(?,?,?,?,?,?,?)", params![request,profile,source,args["requester"].as_str(),encode(&guarantees),old["id"].as_str(),started])?; Ok(()) })?;
            if old["state"] == "queued" {
                return resume_scan(
                    store,
                    profile,
                    source,
                    &request,
                    &json!({}),
                    admitted,
                    Reuse::Inflight,
                );
            }
            return scan_result(
                store,
                old["id"].as_str().unwrap(),
                &request,
                &guarantees,
                catabolic_core::decode_json(old["report"].as_str().unwrap_or("{}"))?,
                Some(Reuse::Inflight),
            );
        }
    }
    if store.db.query_row(
        "SELECT count(*) FROM observation_jobs WHERE state='running'",
        [],
        |r| r.get::<_, i64>(0),
    )? >= 4
        || store
            .db
            .prepare(
                "SELECT 1 FROM observation_jobs WHERE state='running' AND profile=? AND source=?",
            )?
            .exists([profile, source])?
    {
        if continuation.is_some() {
            return Err(Error("observation_capacity".into()));
        }
        // Capacity limits the executing workers, not durable caller demand.
        // A subsequent resume claims this queued generation after a slot frees.
        store.transaction(|db| {
            db.execute("INSERT OR IGNORE INTO observation_sources VALUES(?,?,0)",[profile,source])?;
            let watermark:i64=db.query_row("SELECT dirty_generation FROM observation_sources WHERE profile=? AND source=?",[profile,source],|r|r.get(0))?;
            let generation:i64=db.query_row("SELECT coalesce(max(generation),0)+1 FROM observation_jobs WHERE profile=? AND source=?",[profile,source],|r|r.get(0))?;
            db.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,execution,guarded) VALUES(?,?,?,?,?,?,?,'queued',?,?,1)",params![job,profile,source,compatibility,encode(&exclusions),watermark,generation,started,encode(&execution)])?;
            db.execute("INSERT INTO observation_requests(id,profile,source,requester,guarantees,job_id,created_at) VALUES(?,?,?,?,?,?,?)",params![request,profile,source,args["requester"].as_str(),encode(&guarantees),job,started])?;
            Ok(())
        })?;
        let mut pending = scan_result(
            store,
            &job,
            &request,
            &guarantees,
            json!({}),
            Some(Reuse::Created),
        )?;
        pending["observations"][0]["blocker"] = json!("observation_capacity");
        return Ok(pending);
    }
    let _watermark=store.transaction(|db|{
        db.execute("INSERT OR IGNORE INTO observation_sources VALUES(?,?,0)",[profile,source])?;
        let watermark:i64=db.query_row("SELECT dirty_generation FROM observation_sources WHERE profile=? AND source=?",[profile,source],|r|r.get(0))?;
        let previous:i64=db.query_row("SELECT coalesce(max(generation),0) FROM observation_jobs WHERE profile=? AND source=?",[profile,source],|r|r.get(0))?;
        let lifecycle::Effect::Acquire(lease) = lifecycle::reduce(&lifecycle::State { generation:previous, lease:None }, &lifecycle::Input::Acquire { now:started,duration:3600.0,generation:lifecycle::Generation::Next,token:sha256(&claim),identity:sha256(encode(&json!([compatibility,watermark]))),owner_alive:false }).map_err(|e|Error(format!("observation claim rejected: {e:?}")))? else {return Err(Error("invalid observation claim effect".into()));};
        let generation = lease.generation;
        db.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,started_at,lease_until,worker_pid,scan_id,guarded,execution,claim_token) VALUES(?,?,?,?,?,?,?,'running',?,?,?,?,?,1,?,?)",params![job,profile,source,compatibility,encode(&exclusions),watermark,generation,started,started,lease.until,std::process::id(),scan_id,encode(&execution),claim])?;
        db.execute("INSERT INTO observation_requests(id,profile,source,requester,guarantees,job_id,created_at) VALUES(?,?,?,?,?,?,?)",params![request,profile,source,args["requester"].as_str(),encode(&guarantees),job,started])?;
        db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES(?,?,?,0,0,'[]')",params![scan_id,profile,source])?;
        db.execute("INSERT INTO meta VALUES(?,?)",params![format!("scan:{scan_id}:scope"),encode(&json!({"exclude":exclusions,"extensions":execution["extensions"],"include_regex":execution["include_regex"],"exclude_regex":execution["exclude_regex"]}))])?;
        if let Some(parent)=parent_job {
            db.execute("UPDATE observation_jobs SET parent_job=? WHERE id=?",params![parent,job])?;
            db.execute("INSERT INTO observation_scopes SELECT ?,path,CASE WHEN state NOT IN ('complete','excluded') THEN 'pending' ELSE state END,detail,updated_at FROM observation_scopes WHERE job_id=?",params![job,parent])?;
            db.execute("INSERT INTO observation_seen SELECT ?,file_id FROM observation_seen WHERE job_id=?",params![job,parent])?;
            db.execute("UPDATE scans SET observed=(SELECT count(*) FROM observation_seen WHERE job_id=?) WHERE id=?",params![job,scan_id])?;
            db.execute("INSERT OR IGNORE INTO meta SELECT ?,value FROM meta WHERE key=?",params![format!("observation:{job}:root"),format!("observation:{parent}:root")])?;
            if let Some(scopes)=execution["continuation_scopes"].as_array(){
                for row in rows(db,"SELECT path FROM observation_scopes WHERE job_id=? AND state='pending'",&[job.clone().into()])?{
                    let path=row["path"].as_str().unwrap();if !scopes.iter().filter_map(Value::as_str).any(|p|path==p||path.starts_with(&format!("{p}/"))){db.execute("UPDATE observation_scopes SET state='deferred',detail=? WHERE job_id=? AND path=?",params![encode(&json!({"reason":"not_selected_for_continuation"})),job,path])?;}
                }
            }
        }else{db.execute("INSERT INTO observation_scopes VALUES(?,'','pending','{}',?)",params![job,started])?;}
        Ok(watermark)
    })?;
    run_or_defer(
        store,
        profile,
        source,
        args,
        admitted,
        Admitted {
            guard,
            job,
            scan_id,
            request,
            claim,
            guarantees,
            execution,
            exclusions,
            compatibility,
            reuse: Reuse::Created,
        },
    )
}
fn resume_scan(
    store: &Store,
    profile: &str,
    source: &str,
    request: &str,
    args: &Value,
    admitted: Option<&mut Option<Admitted>>,
    reuse: Reuse,
) -> Result<Value> {
    if [
        "extended",
        "budgets",
        "scopes",
        "exclude",
        "continue_id",
        "continue_request",
    ]
    .iter()
    .any(|k| {
        args.get(*k)
            .is_some_and(|v| !v.is_null() && *v != json!(false))
    }) {
        return Err(Error(
            "use --continue-request to amend execution budgets".into(),
        ));
    }
    recover(store)?;
    let rows = rows(
        &store.db,
        "SELECT r.id AS request_id,r.guarantees,r.state AS request_state,j.* FROM observation_requests r JOIN observation_jobs j ON j.id=r.job_id WHERE r.id=? AND r.profile=?",
        &[request.to_string().into(), profile.to_string().into()],
    )?;
    let old = rows
        .first()
        .ok_or_else(|| Error("unknown_observation_request".into()))?;
    let guarantees = catabolic_core::decode_json(old["guarantees"].as_str().unwrap())?;
    let execution = catabolic_core::decode_json(old["execution"].as_str().unwrap())?;
    let exclusions = catabolic_core::decode_json(old["exclusions"].as_str().unwrap())?;
    let report = catabolic_core::decode_json(old["report"].as_str().unwrap_or("{}"))?;
    let job = old["id"].as_str().unwrap().to_string();
    if old["request_state"] == "cancelled" {
        return scan_result(store, &job, request, &guarantees, report, None);
    }
    if execution["policy_revision"] != effective_policy(store, profile, source)?["revision"] {
        return Err(Error(
            "observation policy changed; request a new scan".into(),
        ));
    }
    let compatibility = compatibility_for(
        store,
        profile,
        source,
        &exclusions,
        &execution,
        guarantees["require_complete"] == true,
    )?;
    let watermark: i64 = store.db.query_row(
        "SELECT dirty_generation FROM observation_sources WHERE profile=? AND source=?",
        [profile, source],
        |r| r.get(0),
    )?;
    let compatible = compatibility == old["compatibility"].as_str().unwrap()
        && watermark <= old["dirty_generation"].as_i64().unwrap();
    if !compatible
        || old["state"] == "unavailable"
        || (old["state"] == "failed"
            && report["resume_progress"] != true
            && report["deferred"] != true)
    {
        let reuse = readmit(
            store,
            profile,
            source,
            request,
            &guarantees,
            &compatibility,
            watermark,
        )?;
        return resume_scan(store, profile, source, request, args, admitted, reuse);
    }
    if old["state"] == "complete" || report["deferred"] == true {
        return scan_result(store, &job, request, &guarantees, report, Some(reuse));
    }
    if old["state"] == "running" {
        return scan_result(store, &job, request, &guarantees, report, Some(reuse));
    }
    if guarantees["minimum_start"]
        .as_f64()
        .is_some_and(|barrier| barrier > now())
    {
        return Err(Error("observation_barrier".into()));
    }
    if store.db.query_row(
        "SELECT count(*) FROM observation_jobs WHERE state='running'",
        [],
        |r| r.get::<_, i64>(0),
    )? >= 4
        || store
            .db
            .prepare(
                "SELECT 1 FROM observation_jobs WHERE state='running' AND profile=? AND source=?",
            )?
            .exists([profile, source])?
    {
        let mut pending = scan_result(store, &job, request, &guarantees, report, Some(reuse))?;
        pending["observations"][0]["blocker"] = json!("observation_capacity");
        return Ok(pending);
    }
    if crate::source::children_forbidden()
        && (old["state"] != "queued"
            || !old["worker_pid"].is_null()
            || !old["claim_token"].is_null())
        && !crate::native_observation_companion::has_cleanup_witness(store, &job)?
    {
        return Err(Error("observation_owner_reconciliation_required".into()));
    }
    let id =
        uuid::Uuid::parse_str(&job).map_err(|_| Error("invalid observation identity".into()))?;
    let guard = store.path.with_file_name(format!(
        "{}.observation-{id}",
        store.path.file_name().unwrap().to_string_lossy()
    ));
    let guard = crate::writer_lock(&guard)?;
    let claim = uuid::Uuid::new_v4().to_string();
    let scan_id = old["scan_id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    store.transaction(|db|{
        let lifecycle::Effect::Acquire(lease) = lifecycle::reduce(&lifecycle::State { generation:old["generation"].as_i64().unwrap(), lease:None },&lifecycle::Input::Acquire { now:now(),duration:3600.0,generation:lifecycle::Generation::Resume,token:sha256(&claim),identity:sha256(encode(&json!([old["compatibility"],old["dirty_generation"]]))),owner_alive:false }).map_err(|e|Error(format!("observation claim rejected: {e:?}")))? else {return Err(Error("invalid observation claim effect".into()));};
        db.execute("UPDATE observation_jobs SET state='running',guarded=1,started_at=CASE WHEN scan_id IS NULL THEN ? ELSE started_at END,lease_until=?,worker_pid=?,scan_id=?,claim_token=? WHERE id=?",params![now(),lease.until,std::process::id(),scan_id,claim,job])?;
        if old["scan_id"].is_null(){
            db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES(?,?,?,0,0,'[]')",params![scan_id,profile,source])?;
            db.execute("INSERT INTO meta VALUES(?,?)",params![format!("scan:{scan_id}:scope"),encode(&json!({"exclude":exclusions,"extensions":execution["extensions"],"include_regex":execution["include_regex"],"exclude_regex":execution["exclude_regex"]}))])?;
            for path in paths(&execution["scopes"],true)?{db.execute("INSERT OR IGNORE INTO observation_scopes VALUES(?,?,'pending','{}',?)",params![job,path,now()])?;}
        }
        Ok(())
    })?;
    run_or_defer(
        store,
        profile,
        source,
        args,
        admitted,
        Admitted {
            guard,
            job,
            scan_id,
            request: request.to_string(),
            claim,
            guarantees,
            execution,
            exclusions,
            compatibility: old["compatibility"].as_str().unwrap().to_string(),
            reuse,
        },
    )
}
fn readmit(
    store: &Store,
    profile: &str,
    source: &str,
    request: &str,
    guarantees: &Value,
    compatibility: &str,
    watermark: i64,
) -> Result<Reuse> {
    let now = now();
    let minimum = guarantees["minimum_start"].as_f64().unwrap_or(0.0);
    store.transaction(|db| {
        let candidates=rows(db,"SELECT * FROM observation_jobs WHERE profile=? AND source=? AND compatibility=? AND dirty_generation>=? ORDER BY generation DESC LIMIT 100",&[profile.to_string().into(),source.to_string().into(),compatibility.to_string().into(),watermark.into()])?;
        let reusable=candidates.iter().find(|row| {
            row["state"]=="queued" || (row["state"]=="running" && row["lease_until"].as_f64().unwrap_or(0.0)>now && row["started_at"].as_f64().unwrap_or(0.0)>=minimum)
                || (row["state"]=="complete" && guarantees["reuse_completed"]==true && row["started_at"].as_f64().unwrap_or(0.0)>=minimum && row["parent_job"].is_null())
        });
        let (job,reuse)=if let Some(row)=reusable {(row["id"].as_str().unwrap().to_string(),if row["state"]=="complete" {Reuse::Completed} else {Reuse::Inflight})} else {
            let id=uuid::Uuid::new_v4().to_string();
            let generation:i64=db.query_row("SELECT coalesce(max(generation),0)+1 FROM observation_jobs WHERE profile=? AND source=?",[profile,source],|r|r.get(0))?;
            db.execute("INSERT INTO observation_jobs(id,profile,source,compatibility,exclusions,dirty_generation,generation,state,requested_at,execution,parent_job,guarded) VALUES(?,?,?,?,?,?,?,'queued',?,?,?,1)",params![id,profile,source,compatibility,encode(&guarantees["exclusions"]),watermark,generation,now,encode(&guarantees["execution"]),guarantees["parent_job"].as_str()])?;
            (id,Reuse::Created)
        };
        db.execute("UPDATE observation_requests SET job_id=? WHERE id=? AND profile=? AND state='active'",params![job,request,profile])?;
        Ok(reuse)
    })
}
struct Admitted {
    reuse: Reuse,
    guard: crate::WriterLock,
    job: String,
    scan_id: String,
    request: String,
    claim: String,
    guarantees: Value,
    execution: Value,
    exclusions: Value,
    compatibility: String,
}
fn run_or_defer(
    store: &Store,
    profile: &str,
    source: &str,
    args: &Value,
    deferred: Option<&mut Option<Admitted>>,
    admitted: Admitted,
) -> Result<Value> {
    if let Some(deferred) = deferred {
        *deferred = Some(admitted);
        return Ok(Value::Null);
    }
    match prepare_scan(store, profile, source, args, admitted)? {
        Prepared::Result(value) => Ok(value),
        Prepared::Scan(mut scan, context) => {
            let outcome = scan.walk(Some(store));
            scan.record_outcome(outcome);
            let job = scan.job.clone();
            let claim = scan.claim.clone();
            let result = finish_scan(store, *scan, context);
            if let Err(error) = &result {
                fail_claim(store, &job, &claim, error)?;
            }
            result
        }
    }
}
fn fail_claim(store: &Store, job: &str, claim: &str, error: &Error) -> Result<()> {
    store.transaction(|db| {
        db.execute("UPDATE observation_jobs SET state='failed',completed_at=?,lease_until=0,report=? WHERE id=? AND state='running' AND claim_token=?",params![now(),encode(&json!({"complete":false,"scan_id":null,"availability":"unknown","inventory_retained":true,"errors":[error.0],"resume_progress":true})),job,claim])?;
        Ok(())
    })
}
struct ScanContext {
    reuse: Reuse,
    request: String,
    guarantees: Value,
    validation: Value,
    policy: Value,
    _guard: crate::WriterLock,
}
enum Prepared {
    Result(Value),
    Scan(Box<Scan>, ScanContext),
}
fn prepare_scan(
    store: &Store,
    profile: &str,
    source: &str,
    args: &Value,
    admitted: Admitted,
) -> Result<Prepared> {
    let Admitted {
        reuse,
        guard,
        job,
        scan_id,
        request,
        claim,
        guarantees,
        execution,
        exclusions,
        compatibility,
    } = admitted;
    let binding = binding(store, profile, "source", source)?;
    let policy = effective_policy(store, profile, source)?;
    let generated = store
        .db
        .prepare("SELECT 1 FROM generated_locations WHERE profile=? AND location=?")?
        .exists([profile, source])?;
    let opened: Result<(File, Value)> = (|| {
        let (root, validation) = checked_root(store, profile, &binding)?;
        if generated {
            crate::artifact_mutation::owned_root(store, profile, &binding)?;
        }
        Ok((root, validation))
    })();
    let (root, validation) = match opened {
        Ok(v) => v,
        Err(error) => {
            let report = json!({"scan_id":scan_id,"location":source,"complete":false,"observed":0,"published":0,"excluded":exclusions,"extensions":execution["extensions"],"availability":"unavailable","inventory_retained":true,"errors":[error.0]});
            store.transaction(|db|{db.execute("UPDATE scans SET errors=?,finished_at=CURRENT_TIMESTAMP WHERE id=?",params![encode(&report["errors"]),scan_id])?;db.execute("UPDATE observation_jobs SET state='unavailable',completed_at=?,lease_until=0,report=? WHERE id=?",params![now(),encode(&report),job])?;Ok(())})?;
            return scan_result(store, &job, &request, &guarantees, report, Some(reuse))
                .map(Prepared::Result);
        }
    };
    let root_identity = identity(&root.metadata()?);
    let pinned: Option<String> = store
        .db
        .query_row(
            "SELECT value FROM meta WHERE key=?",
            [format!("observation:{job}:root")],
            |r| r.get(0),
        )
        .optional()?;
    if pinned
        .map(|raw| catabolic_core::decode_json(&raw))
        .transpose()?
        .is_some_and(|p| p != json!([root_identity.0, root_identity.1]))
    {
        let report = json!({"scan_id":scan_id,"location":source,"complete":false,"observed":0,"published":0,"excluded":exclusions,"extensions":execution["extensions"],"blocker":"source_root_replaced","errors":["source root changed during scan recovery"],"deferred":false,"inventory_retained":true});
        store.transaction(|db|{db.execute("UPDATE observation_jobs SET state='failed',completed_at=?,lease_until=0,report=? WHERE id=?",params![now(),encode(&report),job])?;Ok(())})?;
        return scan_result(store, &job, &request, &guarantees, report, Some(reuse))
            .map(Prepared::Result);
    }
    let database_identity = identity(&std::fs::metadata(&store.path)?);
    store.transaction(|db| {
        db.execute(
            "INSERT OR IGNORE INTO meta VALUES(?,?)",
            params![
                format!("observation:{job}:root"),
                encode(&json!([root_identity.0, root_identity.1]))
            ],
        )?;
        Ok(())
    })?;
    store.transaction(|db| {
        db.execute(
            "UPDATE observation_scopes SET state='pending' WHERE job_id=? AND state='running'",
            [&job],
        )?;
        Ok(())
    })?;
    let sequence: i64 = store.db.query_row(
        "SELECT coalesce(max(sequence),0) FROM observation_batches WHERE job_id=?",
        [&job],
        |r| r.get(0),
    )?;
    let queue_file = tempfile::NamedTempFile::new()?;
    let queue = Connection::open(queue_file.path())?;
    queue.execute_batch("PRAGMA cache_size=-1024; CREATE TABLE queue(path TEXT PRIMARY KEY,state TEXT,detail TEXT); CREATE INDEX pending ON queue(state);")?;
    let mut statement = store
        .db
        .prepare("SELECT path,state,detail FROM observation_scopes WHERE job_id=?")?;
    let mut cursor = statement.query([&job])?;
    let mut directories = 0;
    while let Some(row) = cursor.next()? {
        queue.execute(
            "INSERT INTO queue VALUES(?,?,?)",
            params![
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?
            ],
        )?;
        directories += 1;
    }
    drop(cursor);
    drop(statement);
    let generation: i64 = store.db.query_row(
        "SELECT generation FROM observation_jobs WHERE id=?",
        [&job],
        |r| r.get(0),
    )?;
    if generated {
        queue.execute_batch("CREATE TABLE artifacts(path TEXT PRIMARY KEY)")?;
        let mut statement=store.db.prepare("SELECT path FROM processing_artifacts WHERE profile=? AND location=? AND state='ready' ORDER BY path LIMIT ?")?;
        let mut registry = statement.query(params![
            profile,
            source,
            execution["budgets"]["total_entries"]
                .as_i64()
                .unwrap()
                .saturating_add(1)
        ])?;
        while let Some(row) = registry.next()? {
            queue.execute(
                "INSERT OR IGNORE INTO artifacts VALUES(?)",
                [row.get::<_, String>(0)?],
            )?;
            let pages: i64 = queue.pragma_query_value(None, "page_count", |r| r.get(0))?;
            let size: i64 = queue.pragma_query_value(None, "page_size", |r| r.get(0))?;
            if pages.saturating_mul(size)
                > execution["budgets"]["temporary_bytes"].as_i64().unwrap()
            {
                break;
            }
        }
    }
    let context = Arc::new(PublicationContext {
        path: store.path.clone(),
        database_id: store.database_id.clone(),
        profile: profile.to_string(),
        source: source.to_string(),
        job: job.clone(),
        scan: scan_id.clone(),
        claim,
        generation,
        root: Some(root),
        database_identity,
        execution: execution.clone(),
        filter: ScanFilter::new(&execution)?,
        generated,
        exclusions: exclusions.clone(),
        compatibility,
    });
    let publisher = Publisher(Arc::new(Mutex::new(Publication {
        context: context.clone(),
        root_identity: Some(root_identity),
        sequence,
        entries: Vec::new(),
        bytes: 0,
        last: Instant::now(),
        scopes: Vec::new(),
        initial_scopes: None,
        blocker: None,
        prefiltered_entries: true,
        eager_scopes: generated,
    })));
    let scan = Scan {
        context,
        publisher,
        callbacks: None,
        queue,
        queue_file,
        started: Instant::now(),
        visited: 0,
        directories,
        blocker: None,
        errors: Vec::new(),
        crash_after_scope: args["crash_after_scope"].as_str().map(str::to_owned),
        statistics: Statistics::default(),
    };
    Ok(Prepared::Scan(
        Box::new(scan),
        ScanContext {
            reuse,
            request,
            guarantees,
            validation,
            policy,
            _guard: guard,
        },
    ))
}
fn finish_scan(store: &Store, scan: Scan, context: ScanContext) -> Result<Value> {
    let ScanContext {
        reuse,
        request,
        guarantees,
        validation,
        policy,
        _guard,
    } = context;
    let job = &scan.job;
    let scan_id = &scan.scan;
    let source = &scan.source;
    let execution = &scan.execution;
    let exclusions = &scan.exclusions;
    let counts = rows(
        &store.db,
        "SELECT state,count(*) AS n FROM observation_scopes WHERE job_id=? GROUP BY state",
        &[job.clone().into()],
    )?;
    let counts: serde_json::Map<_, _> = counts
        .into_iter()
        .map(|r| (r["state"].as_str().unwrap().to_string(), r["n"].clone()))
        .collect();
    let outstanding = rows(
        &store.db,
        "SELECT path,state,detail FROM observation_scopes WHERE job_id=? AND state NOT IN('complete','excluded','registry_complete') ORDER BY path LIMIT 101",
        &[job.clone().into()],
    )?;
    let complete = outstanding.is_empty() && scan.blocker.is_none();
    let observed: i64 =
        store
            .db
            .query_row("SELECT observed FROM scans WHERE id=?", [&scan_id], |r| {
                r.get(0)
            })?;
    let deferred = scan.blocker.as_deref().is_some_and(|v| {
        matches!(
            v,
            "temporary_storage_low"
                | "temporary_storage_budget"
                | "total_time_budget"
                | "total_entry_budget"
                | "directory_queue_budget"
                | "regex_filter_timeout"
                | "regex_finalization_budget"
        )
    }) || counts.get("deferred").is_some();
    let header_counts = rows(
        &store.db,
        "SELECT status,count(*) AS n FROM media_header_checks WHERE scan_id=? GROUP BY status",
        &[scan_id.clone().into()],
    )?;
    let header_counts: serde_json::Map<_, _> = header_counts
        .into_iter()
        .map(|r| (r["status"].as_str().unwrap().to_string(), r["n"].clone()))
        .collect();
    let mut report = json!({"scan_id":scan_id,"location":source,"complete":complete,"observed":observed,"published":observed,"excluded":exclusions,"extensions":execution["extensions"],"include_regex":execution["include_regex"],"exclude_regex":execution["exclude_regex"],"coverage_kind":if execution["extensions"].is_null() && execution["include_regex"]==json!([]) && execution["exclude_regex"]==json!([]){"all_files"}else{"filtered_files"},"coverage":counts,"outstanding":outstanding.iter().take(100).collect::<Vec<_>>(),"outstanding_truncated":outstanding.len()>100,"errors":scan.errors,"blocker":scan.blocker,"budgets":execution["budgets"],"batches":scan.publisher.sequence()?,"validation":validation,"availability":"available","inventory_retained":!complete,"requested_scopes":execution["scopes"],"policy_revision":policy["revision"],"deferred":deferred,"media_headers":{"coverage":"zero-filled container header screen; not a probe or decode","counts":header_counts},"progress":{"entries_visited":scan.visited,"directory_records":scan.directories,"batches":scan.publisher.sequence()?,"buffered_bytes":0}});
    report::finish(store, job, scan_id, &scan.statistics, &mut report)?;
    store.transaction(|db| {
        scan.fence(store)?;
        db.execute(
            "UPDATE scans SET complete=?,errors=?,finished_at=CURRENT_TIMESTAMP WHERE id=?",
            params![complete, encode(&report["errors"]), scan_id],
        )?;
        db.execute(
            "INSERT OR REPLACE INTO meta VALUES(?,?)",
            params![format!("scan:{scan_id}:validation"), encode(&validation)],
        )?;
        db.execute(
            "UPDATE observation_jobs SET state=?,completed_at=?,lease_until=0,report=? WHERE id=?",
            params![
                if complete { "complete" } else { "failed" },
                now(),
                encode(&report),
                job
            ],
        )?;
        Ok(())
    })?;
    scan_result(store, job, &request, &guarantees, report, Some(reuse))
}

fn scan_result(
    store: &Store,
    job: &str,
    request: &str,
    guarantees: &Value,
    report: Value,
    reuse: Option<Reuse>,
) -> Result<Value> {
    let mut row = rows(
        &store.db,
        "SELECT * FROM observation_jobs WHERE id=?",
        &[job.to_string().into()],
    )?
    .remove(0);
    row["report"] = report.clone();
    row["request_id"] = json!(request);
    row["guarantees"] = guarantees.clone();
    row["job_state"] = row["state"].clone();
    report::observation(&mut row, reuse)?;
    let request_state: String = store.db.query_row(
        "SELECT state FROM observation_requests WHERE id=? AND profile=?",
        params![request, row["profile"].as_str()],
        |r| r.get(0),
    )?;
    row["request_state"] = json!(request_state);
    if request_state == "cancelled" {
        row["state"] = json!("cancelled");
    }
    let complete = row["state"] == "complete";
    Ok(json!({"complete":complete,"scans":[report],"observations":[row]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn available_space_widens_block_counts_before_saturating_bytes() {
        assert_eq!(available_bytes(u32::MAX, 4096u64), 17_592_186_040_320);
        assert_eq!(available_bytes(u32::MAX, 4096u32), 17_592_186_040_320);
        assert_eq!(available_bytes(u64::MAX, 4096u64), u64::MAX);
        assert_eq!(available_bytes(0u32, u64::MAX), 0);
    }

    fn catalog() -> (tempfile::TempDir, Store, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.sqlite3");
        crate::migration::initialize(&path).unwrap();
        let store = Store::open(&path, true, false).unwrap();
        let source = root.path().join("source");
        std::fs::create_dir(&source).unwrap();
        execute(
            &store,
            "default",
            "bind",
            &json!({"source":"media","root":source}),
        )
        .unwrap();
        (root, store, source)
    }
    fn inventory(store: &Store) -> BTreeMap<String, String> {
        store.db.prepare("SELECT f.path,o.status FROM files f JOIN observations o ON o.file_id=f.id ORDER BY f.path").unwrap().query_map([],|r|Ok((r.get(0)?,r.get(1)?))).unwrap().collect::<std::result::Result<_,_>>().unwrap()
    }
    #[test]
    fn regex_strict_end_backtracking_budget_and_admission_bounds() {
        let filter = ScanFilter::new(
            &json!({"extensions":null,"include_regex":[r"foo\Z"],"exclude_regex":[]}),
        )
        .unwrap();
        // Check anchor translation on the production-compiled regex without
        // making correctness depend on the caller's ten-millisecond wall budget.
        assert!(filter.include[0].is_match("foo").unwrap());
        assert!(!filter.include[0].is_match("foo\n").unwrap());
        let filter = ScanFilter::new(
            &json!({"extensions":null,"include_regex":[r"(?i)(a|b|ab)*(?>c)"],"exclude_regex":[]}),
        )
        .unwrap();
        assert_eq!(
            filter.matches(&"ab".repeat(50)).unwrap_err().0,
            "regex_filter_timeout"
        );
        assert!(patterns(&json!(["(?V1)a"])).is_err());
        assert!(patterns(&json!([""])).is_err());
        assert!(patterns(&json!(vec!["a"; 33])).is_err());
    }
    #[test]
    fn guarded_partial_scopes_publish_positive_and_retain_unvisited_absence() {
        let (_root, store, source) = catalog();
        std::fs::create_dir(source.join("costly")).unwrap();
        std::fs::create_dir(source.join("healthy")).unwrap();
        for n in 0..8 {
            std::fs::write(source.join(format!("costly/{n}.txt")), format!("{n}")).unwrap();
        }
        std::fs::write(source.join("healthy/old.txt"), "old").unwrap();
        assert_eq!(
            execute(&store, "default", "scan", &json!({"source":"media"})).unwrap()["complete"],
            true
        );
        std::fs::remove_file(source.join("costly/7.txt")).unwrap();
        std::fs::remove_file(source.join("healthy/old.txt")).unwrap();
        std::fs::write(source.join("healthy/new.txt"), "new").unwrap();
        let result = execute(
            &store,
            "default",
            "scan",
            &json!({"source":"media","budgets":{"directory_entries":3,"batch_records":2}}),
        )
        .unwrap();
        assert_eq!(result["complete"], false);
        assert_eq!(result["scans"][0]["deferred"], true);
        let files = inventory(&store);
        assert_eq!(files["costly/7.txt"], "present");
        assert_eq!(files["healthy/old.txt"], "missing");
        assert_eq!(files["healthy/new.txt"], "present");
        execute(&store, "default", "scan", &json!({"source":"media"})).unwrap();
        assert_eq!(inventory(&store)["costly/7.txt"], "missing");
    }
    #[test]
    fn exclusions_filters_removed_directories_and_headers() {
        let (_root, store, source) = catalog();
        std::fs::create_dir(source.join("removed")).unwrap();
        std::fs::write(source.join("removed/old.txt"), "old").unwrap();
        std::fs::write(source.join("retained.txt"), "retain").unwrap();
        std::fs::write(source.join("zero.mkv"), [0u8; 128]).unwrap();
        std::fs::write(source.join("empty.txt"), "").unwrap();
        std::fs::write(source.join("ignored.weird"), "other").unwrap();
        execute(&store, "default", "scan", &json!({"source":"media"})).unwrap();
        assert!(!inventory(&store).contains_key("ignored.weird"));
        let invalid: i64 = store
            .db
            .query_row(
                "SELECT count(*) FROM media_header_checks WHERE status='invalid'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(invalid, 2);
        std::fs::remove_dir_all(source.join("removed")).unwrap();
        std::fs::remove_file(source.join("retained.txt")).unwrap();
        execute(
            &store,
            "default",
            "scan",
            &json!({"source":"media","exclude":["retained.txt"]}),
        )
        .unwrap();
        assert_eq!(inventory(&store)["removed/old.txt"], "missing");
        assert_eq!(inventory(&store)["retained.txt"], "present");
    }
    #[test]
    fn source_trust_revision_does_not_relax_file_or_output_identity() {
        let (root, store, source) = catalog();
        std::fs::write(source.join("one.txt"), "one").unwrap();
        execute(&store, "default", "scan", &json!({"source":"media"})).unwrap();
        let policy = execute(
            &store,
            "default",
            "trust",
            &json!({"source":"media","policy":"path","apply":true}),
        )
        .unwrap();
        assert_eq!(policy["revision"], 1);
        assert_eq!(
            execute(
                &store,
                "default",
                "trust",
                &json!({"source":"media","policy":"path","apply":true})
            )
            .unwrap()["revision"],
            1
        );
        std::fs::rename(&source, root.path().join("old-source")).unwrap();
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("new.txt"), "new").unwrap();
        let result = execute(&store, "default", "scan", &json!({"source":"media"})).unwrap();
        assert_eq!(result["complete"], true);
        assert_eq!(result["scans"][0]["validation"]["identity_verified"], false);
        assert_eq!(inventory(&store)["one.txt"], "missing");
        assert_eq!(inventory(&store)["new.txt"], "present");
    }
    #[test]
    fn strict_unavailable_root_retains_inventory_and_symlinks_are_not_followed() {
        let (root, store, source) = catalog();
        std::fs::write(source.join("one.txt"), "one").unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(&outside, source.join("escape")).unwrap();
        execute(&store, "default", "scan", &json!({"source":"media"})).unwrap();
        assert_eq!(inventory(&store).len(), 1);
        std::fs::rename(&source, root.path().join("old-source")).unwrap();
        std::fs::create_dir(&source).unwrap();
        let result = execute(&store, "default", "scan", &json!({"source":"media"})).unwrap();
        assert_eq!(result["complete"], false);
        assert_eq!(inventory(&store)["one.txt"], "present");
    }
    #[test]
    fn unsupported_modes_refuse_before_creating_jobs() {
        let (_root, store, _source) = catalog();
        for args in [
            json!({"source":"media","continue_id":"missing"}),
            json!({"source":"media","budgets":{"total_entries":0}}),
        ] {
            assert!(execute(&store, "default", "scan", &args).is_err());
        }
        assert_eq!(
            store
                .db
                .query_row("SELECT count(*) FROM observation_jobs", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
