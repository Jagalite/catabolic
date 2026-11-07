// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Compare the production row adapter to the prior scheduling contract.
use catabolic_store::watcher;
use serde_json::{Value, json};

fn prior(row: &Value, now: f64) -> bool {
    let definition = &row["definition"];
    let integer = |key: &str| row[key].as_i64().unwrap_or(0);
    let timestamp = |key: &str| row[key].as_f64().unwrap_or(0.0);
    let explicit = integer("requested_generation") > integer("completed_generation");
    if (integer("enabled") == 0 && !explicit)
        || (!row["active_run"].is_null() && timestamp("lease_until") > now)
        || timestamp("retry_at") > now
    {
        return false;
    }
    if explicit || !row["pending_reaction"].is_null() || timestamp("retry_at") != 0.0 {
        return true;
    }
    if definition["schedule"]["kind"] != "manual" && timestamp("next_due") <= now {
        return true;
    }
    definition["events"] == true
        && integer("pending_generation") > integer("completed_generation")
        && !row["event_first"].is_null()
        && now
            >= (timestamp("event_last") + definition["debounce_seconds"].as_f64().unwrap()).min(
                timestamp("event_first") + definition["maximum_delay_seconds"].as_f64().unwrap(),
            )
}
#[test]
fn row_mapping_preserves_generation_and_deadline_admission() {
    for bits in 0..512u32 {
        for retry in [0.0, 1.0, 4.0] {
            for now in [0.0, 1.0, 2.0, 4.0] {
                let row = json!({"enabled":i32::from(bits&1!=0),"requested_generation":i32::from(bits&2!=0),"completed_generation":0,"pending_generation":i32::from(bits&4!=0),"active_run":if bits&8!=0{json!("run")}else{Value::Null},"lease_until":if bits&16!=0{5.0}else{0.0},"retry_at":retry,"pending_reaction":if bits&32!=0{json!("reaction")}else{Value::Null},"next_due":if bits&64!=0{1.0}else{5.0},"event_first":if bits&128!=0{json!(0.0)}else{Value::Null},"event_last":1.0,"definition":{"schedule":{"kind":if bits&256!=0{"manual"}else{"interval"}},"events":true,"debounce_seconds":1.0,"maximum_delay_seconds":3.0}});
                assert_eq!(
                    watcher::due(&row, now).unwrap(),
                    prior(&row, now),
                    "{row} at {now}"
                );
            }
        }
    }
}
