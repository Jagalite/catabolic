// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Read-only report projection of native observation owner facts.
use crate::{Result, Store, migration::rows};
use serde_json::{Value, json};
use std::cell::Cell;

/// Measurements sampled by the existing resource checks, without changing
/// their ordering, admission decisions, or filesystem/SQLite effects.
#[derive(Default)]
pub(super) struct Statistics {
    pub elapsed_seconds: Cell<Option<f64>>,
    pub temporary_bytes: Cell<Option<u64>>,
    pub temporary_free_bytes: Cell<Option<u64>>,
}

#[derive(Clone, Copy)]
pub(super) enum Reuse {
    Created,
    Completed,
    Deferred,
    Inflight,
    Resumed,
}
impl Reuse {
    fn name(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Completed => "completed",
            Self::Deferred => "deferred",
            Self::Inflight => "inflight",
            Self::Resumed => "resumed",
        }
    }
}

pub(super) fn finish(
    store: &Store,
    job: &str,
    scan: &str,
    statistics: &Statistics,
    report: &mut Value,
) -> Result<()> {
    report["media_headers"]["invalid"] = json!(rows(
        &store.db,
        "SELECT h.file_id,f.path,h.reason FROM media_header_checks h JOIN files f ON f.id=h.file_id WHERE h.scan_id=? AND h.status='invalid' ORDER BY f.path LIMIT 100",
        &[scan.to_owned().into()],
    )?);
    report["media_headers"]["invalid_truncated"] = json!(
        report["media_headers"]["counts"]["invalid"]
            .as_u64()
            .unwrap_or(0)
            > 100
    );
    let progress = rows(
        &store.db,
        "SELECT count(*) AS batches,sum(bytes) AS committed_bytes,max(committed_at) AS last_progress_at FROM observation_batches WHERE job_id=?",
        &[job.to_owned().into()],
    )?
    .remove(0);
    report["progress"].as_object_mut().unwrap().extend(
        progress
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    report["progress"]["elapsed_seconds"] = json!(statistics.elapsed_seconds.get());
    report["progress"]["temporary_bytes"] = json!(statistics.temporary_bytes.get());
    report["progress"]["temporary_free_bytes"] = json!(statistics.temporary_free_bytes.get());
    report["suggested_actions"] = if report["complete"] == true {
        json!([])
    } else {
        let demands = rows(
            &store.db,
            "SELECT id FROM observation_requests WHERE job_id=? AND state='active' ORDER BY created_at LIMIT 1",
            &[job.to_owned().into()],
        )?;
        json!([
            {
                "operation":"scan",
                "arguments":{"continue_request":demands.first().map(|r| &r["id"])},
                "choices":["extended","selected_scopes","leave_deferred","repair_then_retry"]
            },
            {
                "operation":"location scan-policy",
                "arguments":{"name":report["location"]},
                "preview_required":true
            }
        ])
    };
    Ok(())
}

pub(super) fn observation(row: &mut Value, reuse: Option<Reuse>) -> Result<()> {
    let execution = catabolic_core::decode_json(row["execution"].as_str().unwrap_or("{}"))?;
    row["coverage"] = json!({
        "source":row["source"],
        "exclusions":catabolic_core::decode_json(row["exclusions"].as_str().unwrap())?,
        "method":if execution.as_object().is_some_and(|v| !v.is_empty()) {
            "guarded-inventory-v2"
        } else {
            "full-inventory-v1"
        },
        "execution":execution,
        "dirty_generation":row["dirty_generation"]
    });
    row["retry_state"] = if row["report"]["deferred"] == true {
        json!("explicit_continuation_required")
    } else {
        match row["job_state"].as_str() {
            Some("queued" | "running") => json!("pending"),
            Some("stale" | "failed" | "unavailable") => json!("retryable"),
            _ => Value::Null,
        }
    };
    if let Some(reuse) = reuse {
        row["reuse"] = json!(reuse.name());
    }
    Ok(())
}
