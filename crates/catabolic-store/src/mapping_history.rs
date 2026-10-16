// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Local-owner destination mapping history. This is deliberately distinct from
//! the authority-scoped HTTP deliveries endpoint: its profile, row ordering,
//! bounded HTTP tail, and combined publication history are the legacy contract.
//! All queries use a read-only Store; no recovery effects or delivery run here.
use crate::{
    DetailedError, DetailedResult, Error, Result, Store, embedding::poll_interrupt, migration::rows,
};
use serde_json::{Value, json};

fn interrupted() -> Result<()> {
    if poll_interrupt() {
        Err(Error("interrupted".into()))
    } else {
        Ok(())
    }
}

pub fn history(store: &Store, profile: &str) -> Result<Vec<Value>> {
    Ok(history_with_subject(store, profile, None)?.0)
}

// Retain at most the selected publication subject from this exact snapshot.
// Values, validation, aggregate order and the HTTP tail still have one owner.
fn history_with_subject(
    store: &Store,
    profile: &str,
    identifier: Option<&str>,
) -> Result<(Vec<Value>, Option<String>)> {
    history_with_subject_detailed(store, profile, identifier).map_err(Into::into)
}

fn history_with_subject_detailed(
    store: &Store,
    profile: &str,
    identifier: Option<&str>,
) -> DetailedResult<(Vec<Value>, Option<String>)> {
    store.db.progress_handler(1000, Some(poll_interrupt))?;
    interrupted()?;
    let mut history = Vec::new();
    let mut selected_subject = None;
    for row in rows(
        &store.db,
        "SELECT id,subject FROM consumer_events WHERE profile=? AND event='mapping_publish' ORDER BY rowid",
        &[profile.to_owned().into()],
    )? {
        interrupted()?;
        let raw = row["subject"]
            .as_str()
            .ok_or_else(|| Error("mapping history subject must be JSON text".into()))?;
        let subject = catabolic_core::decode_json(raw)
            .map_err(|error| DetailedError::mapping_subject_parse(error, raw))?;
        let mut event = subject
            .as_object()
            .cloned()
            .ok_or_else(|| Error("mapping history subject must be a JSON object".into()))?;
        // The frozen dict(id=..., **subject) refuses a duplicate ID, rather than
        // trusting a stored subject to replace the durable event identity.
        if event.insert("id".into(), row["id"].clone()).is_some() {
            return Err(Error("mapping history subject contains an id".into()).into());
        }
        if selected_subject.is_none()
            && identifier.is_some_and(|identifier| row["id"] == identifier)
        {
            selected_subject = Some(raw.to_owned());
        }
        history.push(Value::Object(event));
    }
    let http = rows(
        &store.db,
        "SELECT m.event_id AS id,m.mapping_id AS owner,m.key,m.plan_id,n.state,n.attempts,n.error,3 AS version FROM http_mapping_deliveries m JOIN consumer_events e ON e.id=m.event_id JOIN notification_deliveries n ON n.event_id=m.event_id WHERE e.profile=? ORDER BY e.rowid DESC LIMIT 1000",
        &[profile.to_owned().into()],
    )?;
    interrupted()?;
    history.extend(http.into_iter().rev());
    Ok((history, selected_subject))
}

/// Aggregate before filtering/truncating, including the historical HTTP cap.
/// Taking ownership keeps this query's interruption hook within its Store scope.
fn with_history<T>(
    store: Store,
    profile: &str,
    reduce: impl FnOnce(Vec<Value>) -> Result<T>,
) -> Result<T> {
    store.db.progress_handler(1000, Some(poll_interrupt))?;
    reduce(history(&store, profile)?)
}

pub fn events(store: Store, profile: &str, identifier: Option<&str>) -> Result<Value> {
    with_history(store, profile, |history| select_events(history, identifier))
}

/// Presentation companion for the preserved CLI. The optional raw subject is
/// provenance/order evidence only, never a replacement for the validated value.
/// Complete aggregate validation and selection finish before either is returned.
/// No extra query, Store, public JSON marker or durable state is introduced.
pub fn events_with_subject(
    store: Store,
    profile: &str,
    identifier: Option<&str>,
) -> Result<(Value, Option<String>)> {
    events_with_subject_detailed(store, profile, identifier).map_err(Into::into)
}

/// Additive diagnostic boundary; aggregation and selection still have one owner.
pub fn events_with_subject_detailed(
    store: Store,
    profile: &str,
    identifier: Option<&str>,
) -> DetailedResult<(Value, Option<String>)> {
    let identifier = identifier.filter(|id| !id.is_empty());
    let (history, subject) = history_with_subject_detailed(&store, profile, identifier)?;
    Ok((select_events(history, identifier)?, subject))
}

fn select_events(history: Vec<Value>, identifier: Option<&str>) -> Result<Value> {
    if let Some(identifier) = identifier.filter(|id| !id.is_empty()) {
        return history
            .into_iter()
            .find(|event| event["id"] == identifier)
            .ok_or_else(|| Error("unknown mapping event".into()));
    }
    let truncated = history.len() > 100;
    let mut events = Vec::new();
    for event in history.iter().skip(history.len().saturating_sub(100)) {
        interrupted()?;
        let mut summary = serde_json::Map::new();
        for key in ["id", "owner", "key", "state", "plan_id"] {
            summary.insert(
                key.into(),
                event
                    .get(key)
                    .cloned()
                    .ok_or_else(|| Error(format!("mapping history event is missing {key}")))?,
            );
        }
        events.push(Value::Object(summary));
    }
    Ok(json!({"events":events,"truncated":truncated}))
}

/// Resolve the same aggregate as events, rejecting ambiguous matches. `None`
/// means the actual v1 destination recovery owner is required; it is never a
/// successful or simulated recovery. V2 explicitly retains its M6 gate.
pub fn recover_http(
    store: Store,
    profile: &str,
    identifier: &str,
    apply: bool,
) -> Result<Option<Value>> {
    with_history(store, profile, |history| {
        let mut matches = history
            .into_iter()
            .filter(|event| event["id"] == identifier);
        let event = matches
            .next()
            .ok_or_else(|| Error("mapping_unknown_event".into()))?;
        if matches.next().is_some() {
            return Err(Error("mapping_unknown_event".into()));
        }
        if event["version"].as_f64() == Some(2.0) {
            return Err(Error(crate::publication_mappings::PUBLICATION_GATE.into()));
        }
        if event["version"].as_f64() != Some(3.0) {
            return Ok(None);
        }
        if apply {
            return Err(Error(
                "mapping_http_recovery_uses_notification_queue".into(),
            ));
        }
        let state = event
            .get("state")
            .ok_or_else(|| Error("mapping history event is missing state".into()))?;
        Ok(Some(
            json!({"event_id":identifier,"state":state,"complete":state=="complete","delivery_acknowledged":state=="complete"}),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn catalog() -> (tempfile::TempDir, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("history.db");
        crate::migration::initialize(&path).unwrap();
        (root, path)
    }

    fn put(path: &std::path::Path, id: &str, subject: &str) {
        let store = Store::open(path, true, false).unwrap();
        store.db.execute(
            "INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES (?,'default','mapping_publish','info',?)",
            [id, subject],
        ).unwrap();
    }

    #[test]
    fn mapping_json_parse_provenance_uses_first_failure_without_extra_reads() {
        let (_root, path) = catalog();
        put(&path, "first", r#"{"owner":"good"}"#);
        let raw = "{\"private-fixture-marker\":";
        put(&path, "later", raw);
        put(&path, "last", "[");
        let store = Store::open(&path, false, false).unwrap();
        let selects = Arc::new(AtomicUsize::new(0));
        let observed = selects.clone();
        store
            .db
            .authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
                if matches!(context.action, rusqlite::hooks::AuthAction::Select) {
                    observed.fetch_add(1, Ordering::Relaxed);
                }
                rusqlite::hooks::Authorization::Allow
            }))
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        let error = events_with_subject_detailed(store, "default", Some("first")).unwrap_err();
        assert_eq!(selects.load(Ordering::Relaxed), 1);
        assert_eq!(error.mapping_subject_json(), Some(raw));
        assert_eq!(error.class(), crate::ErrorClass::CatabolicError);
        assert_eq!(error.to_string(), "invalid JSON");
        assert!(!format!("{error:?}").contains("private-fixture-marker"));
        assert!(
            !std::error::Error::source(&error)
                .unwrap()
                .to_string()
                .contains("private-fixture-marker")
        );
        assert_eq!(before, std::fs::read(&path).unwrap());
        assert!(
            DetailedError::from(Error("invalid JSON".into()))
                .mapping_subject_json()
                .is_none()
        );
        assert!(
            DetailedError::unhashable_sort_dictionary()
                .mapping_subject_json()
                .is_none()
        );
    }

    #[test]
    fn mapping_json_parse_provenance_never_tags_nonparse_failures() {
        for raw in ["[]", "null", r#"{"id":"forged"}"#, "{}"] {
            let (_root, path) = catalog();
            put(&path, "event", raw);
            let error = events_with_subject_detailed(
                Store::open(&path, false, false).unwrap(),
                "default",
                None,
            )
            .unwrap_err();
            assert!(error.mapping_subject_json().is_none(), "{raw}");
            assert_eq!(error.class(), crate::ErrorClass::CatabolicError);
        }
    }

    #[test]
    fn selected_subject_and_values_share_the_original_store_snapshot() {
        let (_root, path) = catalog();
        let old = r#"{"z":{"last":1,"first":2},"a":false}"#;
        put(&path, "event", old);
        let writer = Store::open(&path, true, false).unwrap();
        writer
            .db
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        let snapshot = Store::open(&path, false, false).unwrap();
        let selects = Arc::new(AtomicUsize::new(0));
        let observed = selects.clone();
        snapshot
            .db
            .authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
                if matches!(context.action, rusqlite::hooks::AuthAction::Select) {
                    observed.fetch_add(1, Ordering::Relaxed);
                }
                rusqlite::hooks::Authorization::Allow
            }))
            .unwrap();
        writer
            .db
            .execute(
                "UPDATE consumer_events SET subject=? WHERE id='event'",
                [r#"{"a":true,"z":{"first":3,"last":4}}"#],
            )
            .unwrap();
        let (value, raw) =
            events_with_subject_detailed(snapshot, "default", Some("event")).unwrap();
        // Exactly the original publication query and bounded joined HTTP query.
        assert_eq!(selects.load(Ordering::Relaxed), 2);
        assert_eq!(raw.as_deref(), Some(old));
        assert_eq!(
            value,
            json!({"id":"event","z":{"last":1,"first":2},"a":false})
        );
        let (new, raw) = events_with_subject_detailed(
            Store::open(&path, false, false).unwrap(),
            "default",
            Some("event"),
        )
        .unwrap();
        assert_eq!(
            raw.as_deref(),
            Some(r#"{"a":true,"z":{"first":3,"last":4}}"#)
        );
        assert_eq!(new["a"], true);
    }

    #[test]
    fn sidecar_does_not_short_circuit_later_validation_or_change_errors() {
        let (_root, path) = catalog();
        put(&path, "event", r#"{"z":1,"a":2}"#);
        put(&path, "later", "not JSON");
        for raw in ["not JSON", "[]", "null", r#"{"id":"other"}"#] {
            let writer = Store::open(&path, true, false).unwrap();
            writer
                .db
                .execute(
                    "UPDATE consumer_events SET subject=? WHERE id='later'",
                    [raw],
                )
                .unwrap();
            drop(writer);
            let expected = events(
                Store::open(&path, false, false).unwrap(),
                "default",
                Some("event"),
            )
            .unwrap_err();
            let actual = events_with_subject_detailed(
                Store::open(&path, false, false).unwrap(),
                "default",
                Some("event"),
            )
            .unwrap_err();
            assert_eq!(actual.error().0, expected.0);
        }
    }

    #[test]
    fn no_subject_sidecar_for_summaries_empty_identifier_or_unknown_profile() {
        let (_root, path) = catalog();
        put(
            &path,
            "event",
            r#"{"owner":"o","key":"k","state":"s","plan_id":"p"}"#,
        );
        for identifier in [None, Some("")] {
            let (value, raw) = events_with_subject_detailed(
                Store::open(&path, false, false).unwrap(),
                "default",
                identifier,
            )
            .unwrap();
            assert!(raw.is_none());
            assert_eq!(
                value,
                events(
                    Store::open(&path, false, false).unwrap(),
                    "default",
                    identifier
                )
                .unwrap()
            );
        }
        let (value, raw) =
            events_with_subject_detailed(Store::open(&path, false, false).unwrap(), "absent", None)
                .unwrap();
        assert!(raw.is_none());
        assert_eq!(value, json!({"events":[],"truncated":false}));
    }

    #[test]
    fn numeric_http_version_reports_complete_without_mutation() {
        let (_root, path) = catalog();
        put(&path, "event", r#"{"version":3.0,"state":"complete"}"#);
        let before = std::fs::read(&path).unwrap();
        let report = recover_http(
            Store::open(&path, false, false).unwrap(),
            "default",
            "event",
            false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            report,
            json!({"event_id":"event","state":"complete","complete":true,"delivery_acknowledged":true})
        );
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn other_versions_require_actual_destination_owner() {
        let (_root, path) = catalog();
        for (id, version) in [
            ("v1", "1"),
            ("v2", "2"),
            ("text", "\"3\""),
            ("null", "null"),
        ] {
            put(
                &path,
                id,
                &format!(r#"{{"version":{version},"state":"verified"}}"#),
            );
            for apply in [false, true] {
                let result = recover_http(
                    Store::open(&path, false, false).unwrap(),
                    "default",
                    id,
                    apply,
                );
                if version == "2" {
                    assert_eq!(
                        result.unwrap_err().0,
                        crate::publication_mappings::PUBLICATION_GATE
                    );
                } else {
                    assert!(result.unwrap().is_none());
                }
            }
        }
    }

    #[test]
    fn http_apply_refuses_without_catalog_effects() {
        let (_root, path) = catalog();
        put(&path, "event", r#"{"version":3,"state":"pending"}"#);
        let before = std::fs::read(&path).unwrap();
        let error = recover_http(
            Store::open(&path, false, false).unwrap(),
            "default",
            "event",
            true,
        )
        .unwrap_err();
        assert_eq!(error.0, "mapping_http_recovery_uses_notification_queue");
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn stored_subject_cannot_override_event_identity() {
        let (_root, path) = catalog();
        put(
            &path,
            "event",
            r#"{"id":"other","version":3,"state":"pending"}"#,
        );
        let error = events(
            Store::open(&path, false, false).unwrap(),
            "default",
            Some("event"),
        )
        .unwrap_err();
        assert_eq!(error.0, "mapping history subject contains an id");
    }

    #[test]
    fn local_profile_filter_and_complete_subject_are_preserved() {
        let (_root, path) = catalog();
        put(
            &path,
            "event",
            r#"{"state":"pending","version":3,"extra":{"unicode":"Ångström"}}"#,
        );
        assert_eq!(
            events(Store::open(&path, false, false).unwrap(), "absent", None).unwrap(),
            json!({"events":[],"truncated":false})
        );
        assert_eq!(
            events(
                Store::open(&path, false, false).unwrap(),
                "default",
                Some("event")
            )
            .unwrap(),
            json!({"id":"event","state":"pending","version":3,"extra":{"unicode":"Ångström"}})
        );
    }

    #[test]
    fn cancellation_interrupts_history_sql_and_releases_store() {
        let (_root, path) = catalog();
        {
            let store = Store::open(&path, true, false).unwrap();
            store.db.execute_batch("WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<9999) INSERT INTO consumer_events(id,profile,event,severity,subject) SELECT 'event-'||x,'default','mapping_publish','info','{}' FROM n;").unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let check = calls.clone();
        // Initial admission polls once. Later polls before row decoding can only
        // originate in SQLite's progress callback while rows are being loaded.
        let error = crate::embedding::with_interrupt_check(
            move || check.fetch_add(1, Ordering::Relaxed) >= 5,
            || events(Store::open(&path, false, false).unwrap(), "default", None),
        )
        .unwrap_err();
        assert!(error.0.contains("interrupt"), "{error}");
        assert!(calls.load(Ordering::Relaxed) >= 6);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        drop(Store::open(&path, true, false).unwrap());
        assert_eq!(
            events(
                Store::open(&path, false, false).unwrap(),
                "default",
                Some("event-0")
            )
            .unwrap(),
            json!({"id":"event-0"})
        );
    }

    #[test]
    fn cancellation_during_history_decode_releases_read_scope() {
        let (_root, path) = catalog();
        put(&path, "event", r#"{"version":3,"state":"pending"}"#);
        let before = std::fs::read(&path).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let check = calls.clone();
        let error = crate::embedding::with_interrupt_check(
            move || check.fetch_add(1, Ordering::Relaxed) > 0,
            || {
                recover_http(
                    Store::open(&path, false, false).unwrap(),
                    "default",
                    "event",
                    false,
                )
            },
        )
        .unwrap_err();
        assert_eq!(error.0, "interrupted");
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            recover_http(
                Store::open(&path, false, false).unwrap(),
                "default",
                "event",
                false
            )
            .unwrap()
            .is_some()
        );
        drop(Store::open(&path, true, false).unwrap());
    }
}
