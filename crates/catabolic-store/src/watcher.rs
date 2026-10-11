// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Durable local-owner watcher definitions, schedules and fenced report/event
//! runs. Unsupported reaction owners are refused before admission.
use crate::{Error, Result, Store, migration::rows};
use catabolic_core::{coordination as lifecycle, decode_json, encode, sha256};
use chrono::{DateTime, Datelike, LocalResult, TimeZone, Timelike, Utc};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{collections::BTreeSet, io::Read};
fn time() -> f64 {
    crate::filesystem_mutation::now()
}
fn integer(value: &Value, key: &str, low: i64, high: i64) -> Result<i64> {
    value[key]
        .as_i64()
        .filter(|v| (low..=high).contains(v))
        .ok_or_else(|| Error(format!("{key} must be an integer between {low} and {high}")))
}
fn strings(value: &Value, minimum: usize, maximum: usize) -> Result<Vec<String>> {
    let list = value
        .as_array()
        .filter(|v| (minimum..=maximum).contains(&v.len()))
        .ok_or_else(|| Error("invalid watcher list".into()))?;
    let mut result = BTreeSet::new();
    for item in list {
        result.insert(
            item.as_str()
                .ok_or_else(|| Error("watcher list must contain text".into()))?
                .to_string(),
        );
    }
    Ok(result.into_iter().collect())
}
fn allowed(value: &Value, names: &[&str]) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| Error("watcher definition must be an object".into()))?;
    if object.keys().any(|key| !names.contains(&key.as_str())) {
        return Err(Error(
            "extra fields are not permitted in watcher definitions".into(),
        ));
    }
    Ok(())
}

pub fn cron_fields(expression: &str) -> Result<Vec<BTreeSet<u32>>> {
    let tokens: Vec<_> = expression.split_whitespace().collect();
    if tokens.len() != 5 {
        return Err(Error("cron requires minute hour day month weekday".into()));
    }
    let mut result = Vec::new();
    for (token, (low, high)) in tokens
        .iter()
        .zip([(0, 59), (0, 23), (1, 31), (1, 12), (0, 6)])
    {
        let mut values = BTreeSet::new();
        for part in token.split(',') {
            let (base, stride) = if let Some((base, step)) = part.split_once('/') {
                let stride = step
                    .parse::<u32>()
                    .ok()
                    .filter(|v| *v >= 1)
                    .ok_or_else(|| Error("invalid cron step".into()))?;
                (base, stride)
            } else {
                (part, 1)
            };
            let (start, end) = if base == "*" {
                (low, high)
            } else if base.contains('-') {
                let (start, end) = base.split_once('-').unwrap();
                let start = start
                    .parse::<u32>()
                    .map_err(|_| Error("invalid cron range".into()))?;
                let end = end
                    .parse::<u32>()
                    .map_err(|_| Error("invalid cron range".into()))?;
                (start, end)
            } else {
                let v = base
                    .parse::<u32>()
                    .map_err(|_| Error("cron uses numeric lists, ranges, steps and *".into()))?;
                (v, v)
            };
            if start < low || start > end || end > high {
                return Err(Error("cron field out of range".into()));
            }
            for value in (start..=end).step_by(stride as usize) {
                values.insert(value);
            }
        }
        result.push(values);
    }
    Ok(result)
}
// Python ZoneInfo uses the host TZif dataset before package fallbacks. Read the
// same bounded public files rather than silently scheduling with older bundled
// political timezone rules. POSIX recurrence beyond the explicit horizon is
// conservatively refused when the host and bundled datasets differ.
struct HostZone {
    transitions: Vec<(i64, usize)>,
    offsets: Vec<(i32, bool)>,
    default: usize,
    recurring: bool,
    bundled: Option<chrono_tz::Tz>,
    same_version: bool,
}
impl HostZone {
    fn load(name: &str) -> Result<Self> {
        if name.is_empty()
            || name.starts_with('/')
            || name
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err(Error("unknown timezone".into()));
        }
        let path = std::path::Path::new("/usr/share/zoneinfo").join(name);
        let mut raw = Vec::new();
        std::fs::File::open(&path)
            .map_err(|_| Error("unknown timezone".into()))?
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut raw)
            .map_err(|_| Error("invalid timezone data".into()))?;
        if raw.len() > 2 * 1024 * 1024 {
            return Err(Error("timezone data exceeds limit".into()));
        }
        fn counts(raw: &[u8], header: usize) -> Result<[usize; 6]> {
            if raw.get(header..header + 4) != Some(b"TZif") {
                return Err(Error("invalid timezone data".into()));
            }
            let mut result = [0; 6];
            for (i, count) in result.iter_mut().enumerate() {
                let bytes = raw
                    .get(header + 20 + i * 4..header + 24 + i * 4)
                    .ok_or_else(|| Error("truncated timezone data".into()))?;
                *count = u32::from_be_bytes(bytes.try_into().unwrap()) as usize;
                if *count > 100000 {
                    return Err(Error("timezone data exceeds limit".into()));
                }
            }
            Ok(result)
        }
        fn block_size(c: [usize; 6], width: usize) -> usize {
            c[3] * width + c[3] + c[4] * 6 + c[5] + c[2] * (width + 4) + c[1] + c[0]
        }
        let first = counts(&raw, 0)?;
        let (header, width) = if matches!(raw.get(4), Some(b'2' | b'3' | b'4')) {
            (44 + block_size(first, 4), 8)
        } else {
            (0, 4)
        };
        let c = counts(&raw, header)?;
        if c[4] == 0 || c[4] > 256 {
            return Err(Error("invalid timezone type count".into()));
        }
        let start = header + 44;
        let end = start + block_size(c, width);
        if end > raw.len() {
            return Err(Error("truncated timezone data".into()));
        }
        let mut offsets = Vec::with_capacity(c[4]);
        let types = start + c[3] * width + c[3];
        for index in 0..c[4] {
            let position = types + index * 6;
            let offset = i32::from_be_bytes(raw[position..position + 4].try_into().unwrap());
            if offset.unsigned_abs() > 86400 {
                return Err(Error("timezone offset exceeds limit".into()));
            }
            offsets.push((offset, raw[position + 4] != 0));
        }
        let mut transitions = Vec::with_capacity(c[3]);
        for index in 0..c[3] {
            let position = start + index * width;
            let timestamp = if width == 8 {
                i64::from_be_bytes(raw[position..position + 8].try_into().unwrap())
            } else {
                i32::from_be_bytes(raw[position..position + 4].try_into().unwrap()) as i64
            };
            let kind = raw[start + c[3] * width + index] as usize;
            if kind >= offsets.len()
                || transitions
                    .last()
                    .is_some_and(|(last, _)| *last >= timestamp)
            {
                return Err(Error("invalid timezone transitions".into()));
            }
            transitions.push((timestamp, kind));
        }
        let default = offsets.iter().position(|(_, dst)| !*dst).unwrap_or(0);
        let recurring = raw[end..].contains(&b',');
        let same_version = std::fs::read_to_string("/usr/share/zoneinfo/tzdata.zi")
            .ok()
            .and_then(|v| v.lines().next().map(str::to_string))
            .is_some_and(|v| v == format!("# version {}", chrono_tz::IANA_TZDB_VERSION));
        Ok(Self {
            transitions,
            offsets,
            default,
            recurring,
            bundled: name.parse().ok(),
            same_version,
        })
    }
    fn local(&self, timestamp: i64) -> Result<(DateTime<Utc>, bool)> {
        let index = self
            .transitions
            .partition_point(|(time, _)| *time <= timestamp);
        if self.recurring
            && self
                .transitions
                .last()
                .is_some_and(|(last, _)| timestamp > *last)
        {
            if self.same_version
                && let Some(zone) = self.bundled
            {
                let current = DateTime::<Utc>::from_timestamp(timestamp, 0)
                    .ok_or_else(|| Error("watcher date is out of range".into()))?;
                let local = current.with_timezone(&zone);
                let fold = matches!(zone.from_local_datetime(&local.naive_local()),LocalResult::Ambiguous(_,second) if second.timestamp()==timestamp);
                return Ok((local.naive_local().and_utc(), fold));
            }
            return Err(Error("timezone recurrence exceeds the host TZif horizon; matching timezone data is required".into()));
        }
        let kind = if index == 0 {
            self.default
        } else {
            self.transitions[index - 1].1
        };
        let offset = self.offsets[kind].0;
        let fold = if index > 0 {
            let transition = self.transitions[index - 1].0;
            let before = if index == 1 {
                self.default
            } else {
                self.transitions[index - 2].1
            };
            let difference = i64::from(self.offsets[before].0 - offset);
            difference > 0 && timestamp - transition < difference
        } else {
            false
        };
        let local_timestamp = timestamp
            .checked_add(i64::from(offset))
            .ok_or_else(|| Error("watcher date is out of range".into()))?;
        let local = DateTime::<Utc>::from_timestamp(local_timestamp, 0)
            .ok_or_else(|| Error("watcher date is out of range".into()))?;
        Ok((local, fold))
    }
}
pub fn next_due(schedule: &Value, now: f64) -> Result<f64> {
    if !now.is_finite() {
        return Err(Error("invalid watcher clock".into()));
    }
    match schedule["kind"].as_str() {
        Some("manual") => Ok(0.0),
        Some("interval") => Ok(now + integer(schedule, "seconds", 1, 31536000)? as f64),
        Some("cron") => {
            let fields = cron_fields(
                schedule["cron"]
                    .as_str()
                    .ok_or_else(|| Error("cron must be text".into()))?,
            )?;
            let zone = HostZone::load(schedule["timezone"].as_str().unwrap_or("UTC"))?;
            let mut timestamp = ((now / 60.0).floor() as i64)
                .checked_mul(60)
                .ok_or_else(|| Error("watcher date is out of range".into()))?;
            for _ in 0..2 * 366 * 24 * 60 {
                timestamp = timestamp
                    .checked_add(60)
                    .ok_or_else(|| Error("watcher date is out of range".into()))?;
                let (local, fold) = zone.local(timestamp)?;
                if fold {
                    continue;
                }
                let values = [
                    local.minute(),
                    local.hour(),
                    local.day(),
                    local.month(),
                    local.weekday().num_days_from_sunday(),
                ];
                if values
                    .iter()
                    .zip(&fields)
                    .all(|(value, allowed)| allowed.contains(value))
                {
                    return Ok(timestamp as f64);
                }
            }
            Err(Error("cron has no occurrence within two years".into()))
        }
        _ => Err(Error("unknown watcher schedule".into())),
    }
}
fn normalize(store: &Store, profile: &str, input: &Value) -> Result<Value> {
    allowed(
        input,
        &[
            "version",
            "authority",
            "plan",
            "schedule",
            "events",
            "debounce_seconds",
            "maximum_delay_seconds",
            "max_age_seconds",
            "require_complete_inventory",
            "observe_after_request",
            "reaction",
            "mapping",
            "operation_id",
            "destination",
            "max_jobs",
            "max_changes",
            "max_removals",
            "retry_seconds",
        ],
    )?;
    let mut value = json!({"version":1,"authority":"local_owner","plan":null,"schedule":{"kind":"manual","seconds":60,"cron":"0 * * * *","timezone":"UTC"},"events":false,"debounce_seconds":5,"maximum_delay_seconds":60,"max_age_seconds":60,"require_complete_inventory":false,"observe_after_request":false,"reaction":"report","mapping":null,"operation_id":null,"destination":null,"max_jobs":10,"max_changes":100,"max_removals":0,"retry_seconds":30});
    for (k, v) in input.as_object().unwrap() {
        if k != "schedule" {
            value[k] = v.clone();
        }
    }
    if value["version"] != 1 || value["authority"] != "local_owner" {
        return Err(Error("unsupported watcher version or authority".into()));
    }
    if let Some(schedule) = input.get("schedule") {
        allowed(schedule, &["kind", "seconds", "cron", "timezone"])?;
        for (k, v) in schedule.as_object().unwrap() {
            value["schedule"][k] = v.clone();
        }
    }
    integer(&value["schedule"], "seconds", 1, 31536000)?;
    let zone = value["schedule"]["timezone"]
        .as_str()
        .ok_or_else(|| Error("timezone must be text".into()))?;
    HostZone::load(zone)?;
    if !matches!(
        value["schedule"]["kind"].as_str(),
        Some("manual" | "interval" | "cron")
    ) {
        return Err(Error("unknown watcher schedule".into()));
    }
    if value["schedule"]["kind"] == "cron" {
        cron_fields(
            value["schedule"]["cron"]
                .as_str()
                .ok_or_else(|| Error("cron must be text".into()))?,
        )?;
    }
    for key in [
        "events",
        "require_complete_inventory",
        "observe_after_request",
    ] {
        if !value[key].is_boolean() {
            return Err(Error(format!("{key} must be boolean")));
        }
    }
    for (key, low, high) in [
        ("debounce_seconds", 0, 3600),
        ("maximum_delay_seconds", 1, 86400),
        ("max_age_seconds", 0, 86400),
        ("max_jobs", 1, 100),
        ("max_changes", 0, 10000),
        ("max_removals", 0, 10000),
        ("retry_seconds", 1, 86400),
    ] {
        integer(&value, key, low, high)?;
    }
    let plan = &mut value["plan"];
    match plan["kind"].as_str() {
        Some("observation") => {
            allowed(plan, &["kind", "sources", "exclusions"])?;
            let sources = strings(&plan["sources"], 1, 1000)?;
            for source in &sources {
                crate::query::name(source)?;
                crate::observation::binding(store, profile, "source", source)?;
            }
            plan["sources"] = json!(sources);
            let exclusions = strings(plan.get("exclusions").unwrap_or(&json!([])), 0, 1000)?;
            for path in &exclusions {
                crate::layout::relative_path(path)?;
            }
            plan["exclusions"] = json!(exclusions);
            if input.get("reaction").is_some_and(|v| !v.is_null()) {
                return Err(Error("observation plans have no reaction".into()));
            }
            if !value["operation_id"].is_null() || !value["destination"].is_null() {
                return Err(Error("observation plans do not process media".into()));
            }
            value["reaction"] = Value::Null;
        }
        Some("query") => {
            allowed(plan, &["kind", "query_id"])?;
            let id = plan["query_id"]
                .as_str()
                .ok_or_else(|| Error("query_id must be text".into()))?;
            crate::definitions::query_get(store, profile, id)?;
        }
        Some("fallback" | "projection") => {
            return Err(Error(
                "native watcher fallback/projection evaluation is not yet qualified".into(),
            ));
        }
        _ => return Err(Error("unknown watcher plan".into())),
    }
    if value["plan"]["kind"] != "observation"
        && !matches!(
            value["reaction"].as_str(),
            Some("report" | "event" | "processing" | "http" | "projection")
        )
    {
        return Err(Error(
            "query/fallback/projection plans require a reaction".into(),
        ));
    }
    if matches!(
        value["reaction"].as_str(),
        Some("processing" | "http" | "projection")
    ) {
        return Err(Error(
            "native watcher processing/HTTP/projection reactions are not yet qualified".into(),
        ));
    }
    if !value["mapping"].is_null() {
        return Err(Error("mapping requires an HTTP reaction".into()));
    }
    Ok(value)
}
fn get(store: &Store, profile: &str, name: &str) -> Result<Value> {
    let mut rows = rows(
        &store.db,
        "SELECT w.*,d.definition,d.revision,d.digest FROM watchers w JOIN watcher_definitions d ON d.id=w.definition_id WHERE w.profile=? AND w.name=?",
        &[profile.to_string().into(), name.to_string().into()],
    )?;
    let mut row = rows.pop().ok_or_else(|| Error("unknown watcher".into()))?;
    row["definition"] = decode_json(row["definition"].as_str().unwrap())?;
    Ok(row)
}
pub fn execute(store: &Store, profile: &str, action: &str, args: &Value) -> Result<Value> {
    let name = args["name"].as_str().unwrap_or("");
    let now = args
        .get("now")
        .map(|v| {
            v.as_f64()
                .filter(|n| n.is_finite())
                .ok_or_else(|| Error("invalid watcher clock".into()))
        })
        .transpose()?
        .unwrap_or_else(time);
    match action {
        "get" => get(store, profile, name),
        "list" => Ok(json!(rows(
            &store.db,
            "SELECT name,definition_id,enabled,next_due,pending_generation,completed_generation,active_run FROM watchers WHERE profile=? ORDER BY name LIMIT 1000",
            &[profile.to_string().into()]
        )?)),
        "history" => Ok(json!(rows(
            &store.db,
            "SELECT * FROM watcher_runs WHERE profile=? AND watcher=? ORDER BY started_at DESC LIMIT 100",
            &[profile.to_string().into(), name.to_string().into()]
        )?)),
        "put" => {
            crate::query::name(name)?;
            let value = normalize(store, profile, &args["definition"])?;
            let serialized = encode(&value);
            let digest = sha256(&serialized);
            store.transaction(|db|{
                let old=db.query_row("SELECT enabled,active_run FROM watchers WHERE profile=? AND name=?",[profile,name],|r|Ok((r.get::<_,bool>(0)?,r.get::<_,Option<String>>(1)?))).optional()?;
                if old.is_none()&&db.query_row("SELECT count(*) FROM watchers WHERE profile=?",[profile],|r|r.get::<_,i64>(0))?>=1000{return Err(Error("watcher_definition_limit".into()));}
                if old.is_some_and(|(enabled,active)|enabled||active.is_some()){return Err(Error("disable and finish watcher before replacing definition".into()));}
                let prior:Option<String>=db.query_row("SELECT id FROM watcher_definitions WHERE profile=? AND name=? AND digest=?",params![profile,name,digest],|r|r.get(0)).optional()?;
                let id=if let Some(id)=prior{id}else{let revision:i64=db.query_row("SELECT coalesce(max(revision),0)+1 FROM watcher_definitions WHERE profile=? AND name=?",[profile,name],|r|r.get(0))?;let id=uuid::Uuid::new_v4().to_string();db.execute("INSERT INTO watcher_definitions VALUES(?,?,?,?,?,?,?)",params![id,profile,name,revision,serialized,digest,now])?;id};
                db.execute("INSERT INTO watchers(profile,name,definition_id) VALUES(?,?,?) ON CONFLICT(profile,name) DO UPDATE SET definition_id=excluded.definition_id,baseline=NULL,pending_reaction=NULL,next_due=0,retry_at=0,requested_generation=completed_generation",params![profile,name,id])?;Ok(())
            })?;
            get(store, profile, name)
        }
        "admit" => {
            get(store, profile, name)?;
            store.transaction(|db|{db.execute("UPDATE watchers SET pending_generation=pending_generation+1,requested_generation=pending_generation+1,retry_at=0 WHERE profile=? AND name=?",[profile,name])?;Ok(())})?;
            get(store, profile, name)
        }
        "enable" | "disable" => {
            let row = get(store, profile, name)?;
            let enabled = action == "enable";
            normalize(store, profile, &row["definition"])?;
            if !enabled && !row["active_run"].is_null() {
                return Err(Error("watcher run active".into()));
            }
            let next = if enabled {
                next_due(&row["definition"]["schedule"], now)?
            } else {
                0.0
            };
            store.transaction(|db|{if !enabled{db.execute("UPDATE watchers SET retry_at=0,requested_generation=completed_generation WHERE profile=? AND name=?",[profile,name])?;db.execute("DELETE FROM watcher_owners WHERE profile=? AND watcher=?",[profile,name])?;}db.execute("UPDATE watchers SET enabled=?,next_due=? WHERE profile=? AND name=?",params![enabled,next,profile,name])?;Ok(())})?;
            get(store, profile, name)
        }
        "due" => {
            let row = get(store, profile, name)?;
            Ok(json!({"due":due(&row,now)?}))
        }
        "schedule" => Ok(json!({"next_due":next_due(&args["schedule"],now)?})),
        "run" => run(
            store,
            profile,
            name,
            args["trigger"].as_str().unwrap_or("manual"),
            now,
        ),
        "recover" => recover(store, profile, now),
        _ => Err(Error(format!("unknown watcher action: {action}"))),
    }
}
pub fn due(row: &Value, now: f64) -> Result<bool> {
    let value = if row["definition"].is_string() {
        decode_json(row["definition"].as_str().unwrap())?
    } else {
        row["definition"].clone()
    };
    let integer = |key: &str| row[key].as_i64().unwrap_or(0);
    let timestamp = |key: &str| row[key].as_f64().unwrap_or(0.0);
    let explicit = integer("requested_generation") > integer("completed_generation");
    if integer("enabled") == 0 && !explicit
        || !row["active_run"].is_null() && timestamp("lease_until") > now
        || timestamp("retry_at") > now
    {
        return Ok(false);
    }
    if explicit || !row["pending_reaction"].is_null() || timestamp("retry_at") != 0.0 {
        return Ok(true);
    }
    if value["schedule"]["kind"] != "manual" && timestamp("next_due") <= now {
        return Ok(true);
    }
    Ok(value["events"] == true
        && integer("pending_generation") > integer("completed_generation")
        && !row["event_first"].is_null()
        && now
            >= (timestamp("event_last") + value["debounce_seconds"].as_f64().unwrap())
                .min(timestamp("event_first") + value["maximum_delay_seconds"].as_f64().unwrap()))
}
fn require_run(
    store: &Store,
    profile: &str,
    name: &str,
    run: &str,
    definition: &Value,
) -> Result<()> {
    let row = get(store, profile, name)?;
    let generation: Option<i64> = store
        .db
        .query_row(
            "SELECT generation FROM watcher_runs WHERE id=? AND profile=? AND watcher=?",
            [run, profile, name],
            |r| r.get(0),
        )
        .optional()?;
    let generation = generation.ok_or_else(|| Error("stale_watcher_run".into()))?;
    let state = lifecycle::State {
        generation,
        lease: row["active_run"].as_str().map(|active| lifecycle::Lease {
            generation,
            token: sha256(active),
            identity: sha256(encode(&row["definition_id"])),
            until: row["lease_until"].as_f64().unwrap_or(0.0),
        }),
    };
    lifecycle::reduce(
        &state,
        &lifecycle::Input::Fence {
            now: time(),
            generation,
            token: sha256(run),
            identity: sha256(encode(definition)),
        },
    )
    .map_err(|_| Error("stale_watcher_run".into()))?;
    Ok(())
}
fn evaluate(store: &Store, profile: &str, plan: &Value) -> Result<Value> {
    let read = Store::open(&store.path, false, false)?;
    let mut evaluation = crate::selection::Evaluation::new(read, profile, 60_000, 100_000, false)?;
    crate::plans::evaluate(&mut evaluation, plan)
}

fn run(store: &Store, profile: &str, name: &str, trigger: &str, now: f64) -> Result<Value> {
    let row = get(store, profile, name)?;
    let value = normalize(store, profile, &row["definition"])?;
    if trigger == "scheduled" && !due(&row, now)? {
        return Err(Error("watcher_not_due".into()));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let state = lifecycle::State {
        generation: row["completed_generation"].as_i64().unwrap(),
        lease: row["active_run"].as_str().map(|active| lifecycle::Lease {
            generation: row["pending_generation"].as_i64().unwrap(),
            token: sha256(active),
            identity: sha256(encode(&row["definition_id"])),
            until: row["lease_until"].as_f64().unwrap_or(0.0),
        }),
    };
    let lifecycle::Effect::Acquire(lease) = lifecycle::reduce(
        &state,
        &lifecycle::Input::Acquire {
            now,
            duration: 7200.0,
            generation: lifecycle::Generation::AtLeast(row["pending_generation"].as_i64().unwrap()),
            token: sha256(&id),
            identity: sha256(encode(&row["definition_id"])),
            owner_alive: true,
        },
    )
    .map_err(|e| {
        Error(if e == lifecycle::Rejection::LiveOwner {
            "watcher_already_running".into()
        } else {
            format!("watcher claim rejected: {e:?}")
        })
    })?
    else {
        return Err(Error("invalid watcher claim effect".into()));
    };
    let generation = lease.generation;
    store.transaction(|db|{
        if !row["active_run"].is_null(){db.execute("UPDATE watcher_runs SET state='interrupted',completed_at=? WHERE id=?",params![now,row["active_run"].as_str()])?;}
        db.execute("UPDATE watchers SET active_run=?,lease_until=?,pending_generation=? WHERE profile=? AND name=?",params![id,lease.until,generation,profile,name])?;
        if trigger=="manual"{db.execute("UPDATE watchers SET requested_generation=max(requested_generation,?) WHERE profile=? AND name=?",params![generation,profile,name])?;}
        db.execute("INSERT INTO watcher_runs(id,profile,watcher,definition_id,generation,trigger,scheduled_at,started_at,state,worker_pid) VALUES(?,?,?,?,?,?,?,?,'observing',?)",params![id,profile,name,row["definition_id"].as_str(),generation,trigger,if row["next_due"].as_f64().unwrap()!=0.0{row["next_due"].as_f64().unwrap()}else{now},now,std::process::id()])?;Ok(())
    })?;
    let attempt = (|| -> Result<Value> {
        let sources = if value["plan"]["kind"] == "observation" {
            strings(&value["plan"]["sources"], 1, 1000)?
        } else {
            store
                .db
                .prepare(
                    "SELECT owner FROM bindings WHERE profile=? AND kind='source' ORDER BY owner",
                )?
                .query_map([profile], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<_, _>>()?
        };
        let exclusions = if value["plan"]["kind"] == "observation" {
            value["plan"]["exclusions"].clone()
        } else {
            json!([])
        };
        let mut evidence = Vec::new();
        for source in sources {
            let observed = crate::observation::execute(
                store,
                profile,
                "scan",
                &json!({"source":source,"exclude":exclusions,"max_age":value["max_age_seconds"],"require_complete":value["require_complete_inventory"],"after":if value["observe_after_request"]==true{now}else{0.0},"requester":format!("watcher:{}:{generation}",row["definition_id"].as_str().unwrap())}),
            )?;
            evidence.push(observed["observations"][0].clone());
        }
        store.transaction(|db| {
            db.execute(
                "UPDATE watcher_runs SET result=? WHERE id=?",
                params![
                    encode(&json!({"observations":evidence,"complete":false,"phase":"observing"})),
                    id
                ],
            )?;
            Ok(())
        })?;
        if value["require_complete_inventory"] == true
            && evidence.iter().any(|e| e["state"] != "complete")
        {
            return Err(Error("complete_inventory_required".into()));
        }
        require_run(store, profile, name, &id, &row["definition_id"])?;
        if value["plan"]["kind"] == "observation" {
            if evidence.iter().any(|e| e["state"] != "complete") {
                return Err(Error("observation_incomplete".into()));
            }
            return finish(
                store,
                profile,
                name,
                &row,
                &value,
                &id,
                generation,
                now,
                json!({"run_id":id,"definition_id":row["definition_id"],"observations":evidence,"reaction":null,"complete":true}),
            );
        }
        let evaluated = evaluate(store, profile, &value["plan"])?;
        let previous = row["baseline"].as_str().map(decode_json).transpose()?;
        let comparison =
            crate::plans::compare(previous.as_ref().unwrap_or(&Value::Null), &evaluated)?;
        let baseline = comparison["baseline"].clone();
        let changed = comparison["changed"] == true;
        store.transaction(|db|{db.execute("UPDATE watchers SET baseline=?,pending_reaction=? WHERE profile=? AND name=? AND active_run=?",params![encode(&baseline),id,profile,name,id])?;db.execute("UPDATE watcher_runs SET state='reacting',result=? WHERE id=?",params![encode(&json!({"observations":evidence,"evaluation":comparison,"plan_id":null,"complete":false,"phase":"planned","retry_of":row["pending_reaction"]})),id])?;Ok(())})?;
        require_run(store, profile, name, &id, &row["definition_id"])?;
        if value["reaction"] == "event" && (changed || !row["pending_reaction"].is_null()) {
            store.transaction(|db| {
                crate::notifications::emit(
                    db,
                    profile,
                    "watcher_changed",
                    "info",
                    name,
                    &json!([id]),
                )?;
                Ok(())
            })?;
        }
        finish(
            store,
            profile,
            name,
            &row,
            &value,
            &id,
            generation,
            now,
            json!({"run_id":id,"definition_id":row["definition_id"],"observations":evidence,"evaluation":comparison,"reaction":null,"complete":true}),
        )
    })();
    match attempt {
        Ok(report) => Ok(report),
        Err(error) => {
            let waiting = matches!(
                error.0.as_str(),
                "observation_pending" | "observation_already_claimed"
            );
            store.transaction(|db|{db.execute("UPDATE watcher_runs SET state=?,completed_at=?,error=? WHERE id=?",params![if waiting{"waiting"}else{"failed"},time(),error.0.chars().take(4000).collect::<String>(),id])?;db.execute("UPDATE watchers SET active_run=NULL,lease_until=0,retry_at=? WHERE profile=? AND name=? AND active_run=?",params![time()+if waiting{1.0}else{value["retry_seconds"].as_f64().unwrap()},profile,name,id])?;Ok(())})?;
            Ok(json!({"run_id":id,"complete":false,"error":error.0}))
        }
    }
}
#[allow(clippy::too_many_arguments)]
fn finish(
    store: &Store,
    profile: &str,
    name: &str,
    row: &Value,
    value: &Value,
    id: &str,
    generation: i64,
    now: f64,
    report: Value,
) -> Result<Value> {
    require_run(store, profile, name, id, &row["definition_id"])?;
    store.transaction(|db|{db.execute("UPDATE watcher_runs SET state='complete',completed_at=?,result=? WHERE id=?",params![time(),encode(&report),id])?;db.execute("UPDATE watchers SET completed_generation=?,active_run=NULL,lease_until=0,pending_reaction=NULL,last_run=?,retry_at=0,next_due=?,event_first=CASE WHEN pending_generation<=? THEN NULL ELSE event_first END,event_last=CASE WHEN pending_generation<=? THEN NULL ELSE event_last END WHERE profile=? AND name=? AND active_run=?",params![generation,id,next_due(&value["schedule"],now)?,generation,generation,profile,name,id])?;Ok(())})?;
    Ok(report)
}
fn recover(store: &Store, profile: &str, now: f64) -> Result<Value> {
    let active = rows(
        &store.db,
        "SELECT w.name,w.active_run,r.worker_pid FROM watchers w JOIN watcher_runs r ON r.id=w.active_run WHERE w.profile=?",
        &[profile.to_string().into()],
    )?;
    let mut recovered = Vec::new();
    for row in active {
        let Some(pid) = row["worker_pid"]
            .as_i64()
            .filter(|p| (1..=i64::from(i32::MAX)).contains(p))
        else {
            continue;
        };
        // SAFETY: signal zero tests liveness and sends no signal.
        if unsafe { libc::kill(pid as i32, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        {
            continue;
        }
        let changed=store.transaction(|db|{let changed=db.execute("UPDATE watchers SET active_run=NULL,lease_until=0,retry_at=? WHERE profile=? AND name=? AND active_run=? AND EXISTS(SELECT 1 FROM watcher_runs WHERE id=? AND worker_pid=?)",params![now,profile,row["name"].as_str(),row["active_run"].as_str(),row["active_run"].as_str(),pid])?;if changed>0{db.execute("UPDATE watcher_runs SET state='interrupted',completed_at=? WHERE id=? AND worker_pid=?",params![now,row["active_run"].as_str(),pid])?;}Ok(changed)})?;
        if changed > 0 {
            recovered.push(row["active_run"].clone());
        }
    }
    crate::observation::execute(store, profile, "recover", &json!({}))?;
    Ok(json!({"recovered":recovered}))
}
