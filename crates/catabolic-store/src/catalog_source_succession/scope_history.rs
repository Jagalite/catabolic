//! Fixed candidate-only membership producer manifest. Events are observations of
//! actual row mutations, not command receipts or authorization capabilities.
//! AFTER triggers count only real row effects; OLD direct endpoints survive
//! deletion. Occurrence deletion independently covers cascaded assertion parents.
//! No caller SQL is accepted.
//! Candidate cohort limits are 16,384 subjects and 8 MiB per materialized
//! subject/payload set. They are explicit bounded refusal, not a claim that all
//! legacy assertion payloads already obey that ceiling (free-text claims do not).
//! Dependency/supersession counts retain their independent existing validation.
struct Producer {
    table: &'static str,
    key: &'static str,
    columns: &'static [&'static str],
    subjects: &'static str,
}
const PRODUCERS: &[Producer] = &[
    Producer {
        table: "files",
        columns: &["id", "location", "path"],
        key: "json_array(R.id)",
        subjects: "SELECT 'source_files' AS kind,json_array(R.location) AS key UNION SELECT 'file_identity',json_array(R.id)",
    },
    Producer {
        table: "item_relationships",
        columns: &[
            "id",
            "source_id",
            "target_id",
            "kind",
            "position",
            "metadata",
            "active",
        ],
        key: "json_array(R.id)",
        subjects: "SELECT 'motion_parent_target' AS kind,json_array(R.kind,R.target_id) AS key WHERE R.kind IN ('custom:motion_edition','custom:motion_version') UNION SELECT 'motion_parent_source',json_array(R.kind,R.source_id) WHERE R.kind IN ('custom:motion_edition','custom:motion_version')",
    },
    Producer {
        table: "item_files",
        columns: &[
            "id", "file_id", "item_id", "role", "part", "metadata", "origin", "active",
        ],
        key: "json_array(R.id)",
        subjects: "SELECT 'file_associations' AS kind,json_array(R.file_id) AS key UNION SELECT 'item_associations',json_array(R.item_id) UNION SELECT 'association_identity',json_array(R.id)",
    },
    Producer {
        table: "component_occurrences",
        columns: &[
            "id",
            "component_id",
            "profile",
            "file_id",
            "association_id",
            "item_id",
            "revision",
            "snapshot",
            "association_evidence",
            "item_evidence",
            "storage",
            "locator",
            "probe_job_id",
            "stream_key",
            "probe_path",
            "created_at",
        ],
        key: "json_array(R.id)",
        subjects: "SELECT 'file_components' AS kind,json_array(R.file_id) AS key UNION SELECT 'item_components',json_array(R.item_id) UNION SELECT 'component_occurrences',json_array(R.component_id) UNION SELECT 'association_components',json_array(R.association_id)",
    },
    Producer {
        table: "component_assertions",
        columns: &["id", "occurrence_id", "component_id", "payload", "active"],
        key: "json_array(R.id)",
        subjects: "SELECT 'file_assertions' AS kind,json_array(o.file_id) AS key FROM component_occurrences o WHERE o.id=R.occurrence_id UNION SELECT 'item_assertions',json_array(o.item_id) FROM component_occurrences o WHERE o.id=R.occurrence_id UNION SELECT 'component_assertions',json_array(R.component_id) UNION SELECT 'dependency_assertions',json_array(json_extract(d.value,'$.file_id')) FROM json_each(R.payload,'$.dependencies') d WHERE json_type(d.value,'$.file_id')='text' UNION SELECT 'dependency_assertions',json_array(json_extract(R.payload,'$.compatibility.video_file_id')) WHERE json_type(R.payload,'$.compatibility.video_file_id')='text'",
    },
    Producer {
        table: "catalog_versions",
        columns: &[
            "id",
            "association_item_id",
            "segmentation_context_id",
            "current_revision",
        ],
        key: "json_array(R.id)",
        subjects: "SELECT 'context_versions' AS kind,json_array(R.segmentation_context_id) AS key UNION SELECT 'item_versions',json_array(R.association_item_id) UNION SELECT 'version_identity',json_array(R.id)",
    },
    Producer {
        table: "catalog_version_revisions",
        columns: &[
            "version_id",
            "revision",
            "timeline_id",
            "timeline_revision",
            "state",
            "coverage_start_us",
            "coverage_end_us",
            "equivalence_evidence_id",
            "equivalence_basis",
            "recorded_at_us",
            "command_catalog_id",
            "command_host_namespace",
            "command_operation_id",
        ],
        key: "json_array(R.version_id,R.revision)",
        subjects: "SELECT 'version_revisions' AS kind,json_array(R.version_id) AS key UNION SELECT 'timeline_versions',json_array(R.timeline_id)",
    },
    Producer {
        table: "catalog_bindings",
        columns: &["id", "version_id", "association_id"],
        key: "json_array(R.id)",
        subjects: "SELECT 'version_bindings' AS kind,json_array(R.version_id) AS key UNION SELECT 'association_bindings',json_array(R.association_id) UNION SELECT 'binding_identity',json_array(R.id)",
    },
    Producer {
        table: "catalog_binding_revisions",
        columns: &[
            "binding_id",
            "revision",
            "profile",
            "file_id",
            "file_revision",
            "part",
            "source_kind",
            "source_start_us",
            "source_end_us",
            "timeline_start_us",
            "timeline_end_us",
            "normalization_evidence_id",
            "recorded_at_us",
            "command_catalog_id",
            "command_host_namespace",
            "command_operation_id",
        ],
        key: "json_array(R.binding_id,R.revision)",
        subjects: "SELECT 'binding_revisions' AS kind,json_array(R.binding_id) AS key UNION SELECT 'file_bindings',json_array(R.file_id)",
    },
    Producer {
        table: "catalog_version_bindings",
        columns: &[
            "version_id",
            "version_revision",
            "binding_id",
            "binding_revision",
        ],
        key: "json_array(R.version_id,R.version_revision,R.binding_id)",
        subjects: "SELECT 'version_members' AS kind,json_array(R.version_id) AS key UNION SELECT 'binding_versions',json_array(R.binding_id)",
    },
    Producer {
        table: "catalog_segmentation_sets",
        columns: &["id", "context_id", "profile", "file_id", "file_revision"],
        key: "json_array(R.id)",
        subjects: "SELECT 'context_sets' AS kind,json_array(R.context_id) AS key UNION SELECT 'file_sets',json_array(R.file_id) UNION SELECT 'set_identity',json_array(R.id)",
    },
    Producer {
        table: "catalog_segment_members",
        columns: &[
            "context_id",
            "context_revision",
            "set_id",
            "binding_id",
            "binding_revision",
        ],
        key: "json_array(R.context_id,R.context_revision,R.binding_id)",
        subjects: "SELECT 'context_members' AS kind,json_array(R.context_id) AS key UNION SELECT 'binding_segments',json_array(R.binding_id) UNION SELECT 'set_members',json_array(R.set_id)",
    },
    Producer {
        table: "catalog_segmentation_contexts",
        columns: &[
            "id",
            "authority_namespace",
            "context_key",
            "current_revision",
        ],
        key: "json_array(R.id)",
        subjects: "SELECT 'context_identity' AS kind,json_array(R.id) AS key",
    },
    Producer {
        table: "catalog_segmentation_revisions",
        columns: &[
            "context_id",
            "revision",
            "recorded_at_us",
            "command_catalog_id",
            "command_host_namespace",
            "command_operation_id",
        ],
        key: "json_array(R.context_id,R.revision)",
        subjects: "SELECT 'context_revisions' AS kind,json_array(R.context_id) AS key",
    },
    Producer {
        table: "catalog_timelines",
        columns: &["id", "edition_item_id", "current_revision"],
        key: "json_array(R.id)",
        subjects: "SELECT 'timeline_identity' AS kind,json_array(R.id) AS key UNION SELECT 'item_timelines',json_array(R.edition_item_id)",
    },
    Producer {
        table: "catalog_timeline_revisions",
        columns: &[
            "timeline_id",
            "revision",
            "identity_state",
            "coverage_start_us",
            "coverage_end_us",
            "recorded_at_us",
            "command_catalog_id",
            "command_host_namespace",
            "command_operation_id",
        ],
        key: "json_array(R.timeline_id,R.revision)",
        subjects: "SELECT 'timeline_revisions' AS kind,json_array(R.timeline_id) AS key",
    },
    Producer {
        table: "catalog_identity_evidence",
        columns: &["id", "basis", "evidence_json", "recorded_at_us"],
        key: "json_array(R.id)",
        subjects: "SELECT 'identity_evidence' AS kind,json_array(R.id) AS key",
    },
    Producer {
        table: "catalog_logical_retirements",
        columns: &[
            "item_id",
            "kind",
            "survivor_item_id",
            "children_json",
            "reference_map_json",
            "command_catalog_id",
            "command_host_namespace",
            "command_operation_id",
            "recorded_at_us",
        ],
        key: "json_array(R.item_id)",
        subjects: "SELECT 'item_retirement' AS kind,json_array(R.item_id) AS key",
    },
];

/// Preflight the fixed endpoint guard's installation AND restoration names.
/// The producer manifest and origins() behavior remain unchanged. Three times
/// each requested String/Vec length covers geometric growth with the old buffer
/// still alive; two origin sets can coexist while SQLite replaces its hook.
/// This is logical retained/requested allocation accounting, not allocator RSS.
pub(crate) fn origins_guard_charge(
    budget: &mut impl crate::catalog_identity_mutation::StructuralReadBudget,
) -> crate::Result<usize> {
    use crate::operation_budget::BudgetCharge;
    let overflow = || crate::Error("endpoint authorizer accounting overflow".into());
    budget.charge(BudgetCharge {
        work: 1,
        ..Default::default()
    })?;
    let count = PRODUCERS.len().checked_mul(3).ok_or_else(overflow)?;
    let mut names = 0usize;
    let mut formatting = 0usize;
    for producer in PRODUCERS {
        for operation in ["insert", "update", "delete"] {
            budget.charge(BudgetCharge {
                work: 1,
                ..Default::default()
            })?;
            let len = "catalog_source_scope_"
                .len()
                .checked_add(producer.table.len())
                .and_then(|n| n.checked_add(1 + operation.len()))
                .ok_or_else(overflow)?;
            names = names
                .checked_add(len.max(8).checked_mul(3).ok_or_else(overflow)?)
                .ok_or_else(overflow)?;
            formatting = formatting
                .checked_add(1 + len.div_ceil(64))
                .ok_or_else(overflow)?;
        }
    }
    let slots = count
        .max(4)
        .checked_mul(std::mem::size_of::<String>())
        .and_then(|n| n.checked_mul(3))
        .ok_or_else(overflow)?;
    let bytes = names
        .checked_add(slots)
        .and_then(|n| n.checked_add(std::mem::size_of::<Vec<String>>()))
        .and_then(|n| n.checked_mul(2))
        .ok_or_else(overflow)?;
    budget.charge(BudgetCharge {
        bytes,
        values: count.checked_mul(2).ok_or_else(overflow)?,
        work: formatting.checked_mul(2).ok_or_else(overflow)?,
        ..Default::default()
    })?;
    Ok(bytes)
}
pub(crate) fn origins() -> Vec<String> {
    PRODUCERS
        .iter()
        .flat_map(|p| {
            ["insert", "update", "delete"]
                .map(|op| format!("catalog_source_scope_{}_{op}", p.table))
        })
        .collect()
}
/// Fifth named candidate DDL part, included identically in install/reference.
/// Counters are per semantic collection subject, never native global generation.
pub(crate) fn schema() -> String {
    let mut sql = String::from(
        r#"
CREATE TABLE catalog_source_scope_heads (
 scope_kind TEXT NOT NULL, scope_key TEXT NOT NULL CHECK(json_valid(scope_key) AND json_type(scope_key)='array'),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0),
 PRIMARY KEY(scope_kind,scope_key)
);
CREATE TABLE catalog_source_scope_events (
 scope_kind TEXT NOT NULL, scope_key TEXT NOT NULL, revision INTEGER NOT NULL,
 producer TEXT NOT NULL, operation TEXT NOT NULL CHECK(operation IN ('insert','update','delete')),
 old_key TEXT CHECK(old_key IS NULL OR (json_valid(old_key) AND json_type(old_key)='array')),
 new_key TEXT CHECK(new_key IS NULL OR (json_valid(new_key) AND json_type(new_key)='array')),
 PRIMARY KEY(scope_kind,scope_key,revision),
 FOREIGN KEY(scope_kind,scope_key) REFERENCES catalog_source_scope_heads(scope_kind,scope_key)
);
CREATE TRIGGER catalog_source_scope_heads_no_delete BEFORE DELETE ON catalog_source_scope_heads
 BEGIN SELECT RAISE(ABORT,'scoped mutation history retained'); END;
CREATE TRIGGER catalog_source_scope_heads_advance BEFORE UPDATE ON catalog_source_scope_heads
 WHEN NEW.scope_kind IS NOT OLD.scope_kind OR NEW.scope_key IS NOT OLD.scope_key
 OR OLD.revision=9223372036854775807 OR NEW.revision!=OLD.revision+1
 BEGIN SELECT RAISE(ABORT,'scoped mutation revision conflict'); END;
CREATE TRIGGER catalog_source_scope_events_no_update BEFORE UPDATE ON catalog_source_scope_events
 BEGIN SELECT RAISE(ABORT,'scoped mutation history immutable'); END;
CREATE TRIGGER catalog_source_scope_events_no_delete BEFORE DELETE ON catalog_source_scope_events
 BEGIN SELECT RAISE(ABORT,'scoped mutation history retained'); END;
"#,
    );
    for producer in PRODUCERS {
        for operation in ["insert", "update", "delete"] {
            let aliases: &[&str] = match operation {
                "insert" => &["NEW"],
                "delete" => &["OLD"],
                _ => &["OLD", "NEW"],
            };
            let subjects = aliases
                .iter()
                .map(|a| producer.subjects.replace("R.", &format!("{a}.")))
                .collect::<Vec<_>>()
                .join(" UNION ");
            let old = if operation == "insert" {
                "NULL".into()
            } else {
                producer.key.replace("R.", "OLD.")
            };
            let new = if operation == "delete" {
                "NULL".into()
            } else {
                producer.key.replace("R.", "NEW.")
            };
            let changed = if operation == "update" {
                format!(
                    "WHEN {}",
                    producer
                        .columns
                        .iter()
                        .map(|c| format!("NEW.\"{c}\" IS NOT OLD.\"{c}\""))
                        .collect::<Vec<_>>()
                        .join(" OR ")
                )
            } else {
                String::new()
            };
            let preflight = if producer.table == "component_assertions" {
                aliases.iter().map(|a| format!("SELECT CASE WHEN length(CAST({a}.payload AS BLOB))>8388608 OR NOT json_valid({a}.payload) THEN RAISE(ABORT,'scoped assertion payload exceeds budget') END;\n")).collect::<String>()
            } else {
                String::new()
            };
            sql.push_str(&format!(r#"
CREATE TRIGGER catalog_source_scope_{table}_{operation} AFTER {event} ON {table}
{changed}
BEGIN
 {preflight}
 SELECT CASE WHEN (SELECT count(*)>16384 OR coalesce(sum(256+length(CAST(kind AS BLOB))+length(CAST(key AS BLOB))),0)>8388608 FROM ({subjects}))
 THEN RAISE(ABORT,'scoped mutation subjects exceed budget') END;
 INSERT INTO catalog_source_scope_heads(scope_kind,scope_key,revision)
 SELECT kind,key,1 FROM ({subjects}) WHERE true
 ON CONFLICT(scope_kind,scope_key) DO UPDATE SET revision=revision+1;
 INSERT INTO catalog_source_scope_events(scope_kind,scope_key,revision,producer,operation,old_key,new_key)
 SELECT s.kind,s.key,h.revision,'{table}','{operation}',{old},{new}
 FROM ({subjects}) s JOIN catalog_source_scope_heads h ON h.scope_kind=s.kind AND h.scope_key=s.key;
END;
"#,table=producer.table,event=operation.to_uppercase()));
        }
    }
    sql.push_str(&retained_schema());
    sql
}

#[cfg(test)]
pub(super) fn assert_manifest_columns_for_test(db: &rusqlite::Connection) {
    for producer in PRODUCERS {
        let actual = db
            .prepare(&format!("PRAGMA table_info({})", producer.table))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            actual, producer.columns,
            "producer column contract: {}",
            producer.table
        );
    }
}

/// Stable anchors include collections with no children/head yet. These are
/// descriptive fences, not execution authority. Exact absence is revision zero.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ScopeHead {
    pub(super) kind: String,
    pub(super) key: String,
    pub(super) revision: i64,
}
pub(super) fn collections(kind: &str) -> &'static [&'static str] {
    match kind {
        "files" => &[
            "file_identity",
            "file_associations",
            "file_components",
            "file_assertions",
            "dependency_assertions",
            "file_bindings",
            "file_sets",
        ],
        "items" => &[
            "item_associations",
            "item_components",
            "item_assertions",
            "item_versions",
            "item_timelines",
            "item_retirement",
        ],
        "item_files" => &[
            "association_identity",
            "association_components",
            "association_bindings",
        ],
        "media_components" => &["component_occurrences", "component_assertions"],
        "catalog_versions" => &[
            "version_identity",
            "version_revisions",
            "version_bindings",
            "version_members",
        ],
        "catalog_bindings" => &[
            "binding_identity",
            "binding_revisions",
            "binding_versions",
            "binding_segments",
        ],
        "catalog_segmentation_contexts" => &[
            "context_identity",
            "context_revisions",
            "context_versions",
            "context_sets",
            "context_members",
        ],
        "catalog_segmentation_sets" => &["set_identity", "set_members"],
        "catalog_timelines" => &[
            "timeline_identity",
            "timeline_revisions",
            "timeline_versions",
        ],
        "catalog_identity_evidence" => &["identity_evidence"],
        _ => &[],
    }
}
pub(super) fn capture(
    store: &crate::Store,
    source: &str,
    files: &std::collections::BTreeMap<String, String>,
    contexts: &[super::Context],
    budget: &mut impl crate::catalog_identity_mutation::StructuralReadBudget,
) -> crate::Result<Vec<ScopeHead>> {
    use crate::operation_budget::BudgetCharge;
    use std::collections::BTreeSet;
    let mut subjects = BTreeSet::<(String, String)>::new();
    let mut add = |budget: &mut dyn crate::catalog_identity_mutation::StructuralReadBudget,
                   kind: &str,
                   key: String|
     -> crate::Result<()> {
        budget.charge(BudgetCharge {
            bytes: 256 + kind.len() + key.len(),
            values: 3,
            work: 1,
            ..Default::default()
        })?;
        let subject = (kind.to_owned(), key);
        if !subjects.contains(&subject) {
            if subjects.len() >= 16384 {
                return Err(super::refuse("scope collection count exceeds bound"));
            }
            budget.charge(BudgetCharge {
                items: 1,
                ..Default::default()
            })?;
            subjects.insert(subject);
        }
        Ok(())
    };
    add(budget, "source_files", serde_json::to_string(&[source])?)?;
    for file in files.keys().map(String::as_str).chain(
        contexts
            .iter()
            .flat_map(|c| c.versions.iter())
            .flat_map(|v| v.version.bindings.iter())
            .map(|b| b.file.file_id.as_str()),
    ) {
        for kind in collections("files") {
            add(budget, kind, serde_json::to_string(&[file])?)?;
        }
    }
    for context in contexts {
        for (entity, key, _) in &context.heads {
            // Structural capture has already bounded/validated every key. Charge
            // the parse/copy before decoding it for stable collection subjects.
            budget.charge(BudgetCharge {
                bytes: key.len(),
                values: 1,
                work: 1,
                ..Default::default()
            })?;
            let value: serde_json::Value = serde_json::from_str(key)?;
            let Some(id) = value.get(0).and_then(serde_json::Value::as_str) else {
                continue;
            };
            for kind in collections(entity) {
                add(budget, kind, serde_json::to_string(&[id])?)?;
            }
            if entity == "items" {
                for relationship in ["custom:motion_edition", "custom:motion_version"] {
                    add(
                        budget,
                        "motion_parent_target",
                        serde_json::to_string(&[relationship, id])?,
                    )?;
                }
            }
        }
    }
    drop(add);
    let input_bytes: usize = subjects
        .iter()
        .map(|(k, v)| 64 + 6 * (k.len() + v.len()))
        .sum();
    if input_bytes > 8 * 1024 * 1024 {
        return Err(super::refuse("scope collection bytes exceed bound"));
    }
    budget.charge(BudgetCharge {
        bytes: input_bytes,
        values: subjects.len() * 3,
        work: subjects.len(),
        ..Default::default()
    })?;
    let input = serde_json::to_string(&subjects)?;
    let mut statement=store.db.prepare("SELECT json_extract(s.value,'$[0]'),json_extract(s.value,'$[1]'),coalesce(h.revision,0) FROM json_each(?1) s LEFT JOIN catalog_source_scope_heads h ON h.scope_kind=json_extract(s.value,'$[0]') AND h.scope_key=json_extract(s.value,'$[1]') ORDER BY 1,2")?;
    let rows = statement
        .query_map([input], |r| {
            Ok(ScopeHead {
                kind: r.get(0)?,
                key: r.get(1)?,
                revision: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if rows.len() != subjects.len() {
        return Err(super::refuse("scope collection incomplete"));
    }
    Ok(rows)
}

// The same mapping is consumed by ephemeral preparation and durable SQL.
fn retained_schema() -> String {
    let entities = [
        "files",
        "items",
        "item_files",
        "media_components",
        "catalog_versions",
        "catalog_bindings",
        "catalog_segmentation_contexts",
        "catalog_segmentation_sets",
        "catalog_timelines",
        "catalog_identity_evidence",
    ];
    let mapping = entities
        .iter()
        .flat_map(|entity| {
            collections(entity)
                .iter()
                .map(move |kind| format!("('{entity}','{kind}')"))
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"
CREATE TABLE catalog_source_succession_scopes (
 profile TEXT NOT NULL, source_id TEXT NOT NULL, succession_revision INTEGER NOT NULL,
 scope_kind TEXT NOT NULL, scope_key TEXT NOT NULL CHECK(json_valid(scope_key) AND json_type(scope_key)='array'),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0),
 PRIMARY KEY(profile,source_id,succession_revision,scope_kind,scope_key),
 FOREIGN KEY(profile,source_id,succession_revision) REFERENCES catalog_source_successions(profile,source_id,revision)
);
CREATE TRIGGER catalog_source_succession_scopes_no_update BEFORE UPDATE ON catalog_source_succession_scopes
 BEGIN SELECT RAISE(ABORT,'source succession scopes immutable'); END;
CREATE TRIGGER catalog_source_succession_scopes_no_delete BEFORE DELETE ON catalog_source_succession_scopes
 BEGIN SELECT RAISE(ABORT,'source succession scopes retained'); END;
CREATE TRIGGER catalog_source_succession_scopes_insert_owner BEFORE INSERT ON catalog_source_succession_scopes
 WHEN NOT EXISTS(SELECT 1 FROM catalog_source_succession_permits p WHERE p.profile=NEW.profile AND p.source_id=NEW.source_id AND p.succession_revision=NEW.succession_revision)
 BEGIN SELECT RAISE(ABORT,'source succession scopes require retained owner'); END;
CREATE VIEW catalog_source_required_scopes AS
 WITH mapping(entity,kind) AS (VALUES {mapping})
 SELECT s.profile,s.source_id,s.revision AS succession_revision,'source_files' AS scope_kind,json_array(s.source_id) AS scope_key FROM catalog_source_successions s
 UNION SELECT h.profile,h.source_id,h.succession_revision,m.kind,json_array(json_extract(h.entity_key,'$[0]'))
 FROM catalog_source_succession_heads h JOIN mapping m ON m.entity=h.entity_kind
 UNION SELECT f.profile,f.source_id,f.succession_revision,m.kind,json_array(f.file_id)
 FROM catalog_source_succession_files f JOIN mapping m ON m.entity='files'
 UNION SELECT h.profile,h.source_id,h.succession_revision,'motion_parent_target',json_array('custom:motion_edition',json_extract(h.entity_key,'$[0]')) FROM catalog_source_succession_heads h WHERE h.entity_kind='items'
 UNION SELECT h.profile,h.source_id,h.succession_revision,'motion_parent_target',json_array('custom:motion_version',json_extract(h.entity_key,'$[0]')) FROM catalog_source_succession_heads h WHERE h.entity_kind='items';
CREATE VIEW catalog_source_current_scope_effects AS
 SELECT s.profile,s.source_id,s.revision FROM catalog_source_successions s
 WHERE CASE WHEN (SELECT count(*)<=16384 AND coalesce(sum(256+length(CAST(r.scope_kind AS BLOB))+length(CAST(r.scope_key AS BLOB))),0)<=8388608 FROM catalog_source_required_scopes r WHERE r.profile=s.profile AND r.source_id=s.source_id AND r.succession_revision=s.revision)
 THEN s.scope_count=(SELECT count(*) FROM catalog_source_succession_scopes h WHERE h.profile=s.profile AND h.source_id=s.source_id AND h.succession_revision=s.revision)
 AND NOT EXISTS(SELECT scope_kind,scope_key FROM catalog_source_required_scopes r WHERE r.profile=s.profile AND r.source_id=s.source_id AND r.succession_revision=s.revision EXCEPT SELECT scope_kind,scope_key FROM catalog_source_succession_scopes h WHERE h.profile=s.profile AND h.source_id=s.source_id AND h.succession_revision=s.revision)
 AND NOT EXISTS(SELECT scope_kind,scope_key FROM catalog_source_succession_scopes h WHERE h.profile=s.profile AND h.source_id=s.source_id AND h.succession_revision=s.revision EXCEPT SELECT scope_kind,scope_key FROM catalog_source_required_scopes r WHERE r.profile=s.profile AND r.source_id=s.source_id AND r.succession_revision=s.revision)
 AND NOT EXISTS(SELECT 1 FROM catalog_source_succession_scopes h LEFT JOIN catalog_source_scope_heads current ON current.scope_kind=h.scope_kind AND current.scope_key=h.scope_key WHERE h.profile=s.profile AND h.source_id=s.source_id AND h.succession_revision=s.revision AND h.revision!=coalesce(current.revision,0))
 ELSE catalog_source_evidence_refused() END;
"#
    )
}
