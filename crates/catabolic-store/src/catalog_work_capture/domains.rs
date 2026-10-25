//! Bounded descriptive metadata, identity and item-tag dependency closure.
//! These facts confer neither tag mutation authority nor identity ownership.
use super::*;

pub(super) fn capture(
    db: &Connection,
    budget: &mut Budget,
    facts: &mut Map<String, Value>,
    owners: &str,
    schema: u32,
) -> Result<()> {
    for (name, sql) in [
        (
            "identities",
            "SELECT * FROM identities WHERE item_id IN(SELECT value FROM json_each(?)) ORDER BY namespace,value",
        ),
        (
            "catalog_legacy_identity_evidence",
            "SELECT * FROM catalog_legacy_identity_evidence WHERE item_id IN(SELECT value FROM json_each(?)) ORDER BY namespace,value,item_id",
        ),
        (
            "catalog_identity_pins",
            "SELECT * FROM catalog_identity_pins WHERE item_id IN(SELECT value FROM json_each(?)) ORDER BY item_id,namespace",
        ),
        (
            "catalog_identity_pin_revisions",
            "SELECT r.* FROM catalog_identity_pin_revisions r JOIN catalog_identity_pins h ON h.item_id=r.item_id AND h.namespace=r.namespace AND h.current_revision=r.revision WHERE h.item_id IN(SELECT value FROM json_each(?)) ORDER BY r.item_id,r.namespace,r.revision",
        ),
        (
            "item_tags",
            "SELECT * FROM item_tags WHERE item_id IN(SELECT value FROM json_each(?)) ORDER BY id",
        ),
        (
            "catalog_item_tag_suppressions",
            "SELECT * FROM catalog_item_tag_suppressions WHERE item_id IN(SELECT value FROM json_each(?)) ORDER BY item_id,tag_id",
        ),
        // Retained old-tag decisions are necessary after vocabulary merges even
        // when the current suppression head no longer names that retired ID.
        (
            "catalog_tag_suppression_revisions",
            "SELECT * FROM catalog_tag_suppression_revisions WHERE subject_type='item' AND subject_id IN(SELECT value FROM json_each(?)) ORDER BY subject_type,subject_id,tag_id,revision",
        ),
    ] {
        facts.insert(name.into(), json!(budget.rows(db, sql, &[owners.into()])?));
    }
    let corrupt: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_identity_pins p LEFT JOIN catalog_identity_pin_revisions r ON r.item_id=p.item_id AND r.namespace=p.namespace AND r.revision=p.current_revision WHERE p.item_id IN(SELECT value FROM json_each(?1)) AND (r.item_id IS NULL OR r.value IS NOT p.value)) OR EXISTS(SELECT 1 FROM catalog_item_tag_suppressions s LEFT JOIN catalog_tag_suppression_revisions r ON r.subject_type='item' AND r.subject_id=s.item_id AND r.tag_id=s.tag_id AND r.revision=s.revision WHERE s.item_id IN(SELECT value FROM json_each(?1)) AND r.subject_id IS NULL)",
        [owners], |r| r.get(0))?;
    if corrupt {
        return Err(fail("incomplete identity pin or tag suppression head"));
    }

    let mut tags = BTreeSet::new();
    for name in [
        "item_tags",
        "catalog_item_tag_suppressions",
        "catalog_tag_suppression_revisions",
    ] {
        for row in facts[name]
            .as_array()
            .ok_or_else(|| fail("missing tag facts"))?
        {
            tags.insert(text(row, "tag_id")?.to_owned());
        }
    }
    // Follow the full connected vocabulary dependency component, not a global
    // vocabulary dump. A disconnected vocabulary edit cannot change this work.
    // Every expansion consumes the enclosing native row/byte/time budget.
    let mut pending = tags.clone();
    while !pending.is_empty() {
        let frontier = encoded(&pending.into_iter().collect::<Vec<_>>())?;
        pending = BTreeSet::new();
        for (sql, left, right) in [
            (
                "SELECT * FROM tag_parents WHERE child_id IN(SELECT value FROM json_each(?1)) OR parent_id IN(SELECT value FROM json_each(?1)) ORDER BY child_id,parent_id",
                "child_id",
                "parent_id",
            ),
            (
                "SELECT * FROM catalog_tag_redirects WHERE source_tag_id IN(SELECT value FROM json_each(?1)) OR target_tag_id IN(SELECT value FROM json_each(?1)) ORDER BY source_tag_id",
                "source_tag_id",
                "target_tag_id",
            ),
            (
                "SELECT * FROM catalog_tag_merges WHERE source_tag_id IN(SELECT value FROM json_each(?1)) OR original_target_tag_id IN(SELECT value FROM json_each(?1)) ORDER BY source_tag_id",
                "source_tag_id",
                "original_target_tag_id",
            ),
        ] {
            for row in budget.rows(db, sql, &[frontier.clone()])? {
                for field in [left, right] {
                    let id = text(&row, field)?.to_owned();
                    if tags.insert(id.clone()) {
                        pending.insert(id);
                    }
                }
            }
        }
        budget.check()?;
    }
    let encoded_tags = encoded(&tags.into_iter().collect::<Vec<_>>())?;
    for (name, sql) in [
        (
            "tags",
            "SELECT * FROM tags WHERE id IN(SELECT value FROM json_each(?)) ORDER BY id",
        ),
        (
            "tag_names",
            "SELECT * FROM tag_names WHERE tag_id IN(SELECT value FROM json_each(?)) ORDER BY name",
        ),
        (
            "tag_parents",
            "SELECT * FROM tag_parents WHERE child_id IN(SELECT value FROM json_each(?1)) OR parent_id IN(SELECT value FROM json_each(?1)) ORDER BY child_id,parent_id",
        ),
        (
            "catalog_tag_redirects",
            "SELECT * FROM catalog_tag_redirects WHERE source_tag_id IN(SELECT value FROM json_each(?1)) OR target_tag_id IN(SELECT value FROM json_each(?1)) ORDER BY source_tag_id",
        ),
        (
            "catalog_tag_merges",
            "SELECT * FROM catalog_tag_merges WHERE source_tag_id IN(SELECT value FROM json_each(?1)) OR original_target_tag_id IN(SELECT value FROM json_each(?1)) ORDER BY source_tag_id",
        ),
    ] {
        facts.insert(
            name.into(),
            json!(budget.rows(db, sql, &[encoded_tags.clone()])?),
        );
    }
    let unknown_tag: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM json_each(?1) k WHERE NOT EXISTS(SELECT 1 FROM tags t WHERE t.id=k.value) AND NOT EXISTS(SELECT 1 FROM catalog_tag_merges m WHERE m.source_tag_id=k.value))", [&encoded_tags], |r|r.get(0))?;
    if unknown_tag {
        return Err(fail(
            "tag dependency lacks live or retired vocabulary owner",
        ));
    }
    let corrupt: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM item_tags a LEFT JOIN tags t ON t.id=a.tag_id WHERE a.item_id IN(SELECT value FROM json_each(?1)) AND t.id IS NULL) OR EXISTS(SELECT 1 FROM catalog_tag_redirects r LEFT JOIN catalog_tag_merges m ON m.source_tag_id=r.source_tag_id LEFT JOIN tags t ON t.id=r.target_tag_id WHERE (r.source_tag_id IN(SELECT value FROM json_each(?2)) OR r.target_tag_id IN(SELECT value FROM json_each(?2))) AND (m.source_tag_id IS NULL OR t.id IS NULL))", params![owners,encoded_tags], |r|r.get(0))?;
    if corrupt {
        return Err(fail("incomplete tag vocabulary or redirect provenance"));
    }

    if schema == 40 {
        for (name, sql) in [
            (
                "catalog_metadata_schemas",
                "SELECT * FROM catalog_metadata_schemas WHERE item_id IN(SELECT value FROM json_each(?)) ORDER BY item_id,schema_fingerprint",
            ),
            (
                "catalog_metadata_schema_heads",
                "SELECT * FROM catalog_metadata_schema_heads WHERE item_id IN(SELECT value FROM json_each(?)) ORDER BY item_id",
            ),
            (
                "catalog_metadata_tag_history",
                "SELECT h.* FROM catalog_metadata_tag_history h JOIN catalog_metadata_sources s ON s.item_id=h.item_id AND s.namespace=h.namespace AND s.record_id=h.record_id AND s.revision=h.revision WHERE s.item_id IN(SELECT value FROM json_each(?)) ORDER BY h.item_id,h.namespace,h.record_id,h.revision",
            ),
            (
                "catalog_metadata_tag_suppressions",
                "SELECT * FROM catalog_metadata_tag_suppressions WHERE item_id IN(SELECT value FROM json_each(?)) ORDER BY item_id,tag_id",
            ),
        ] {
            facts.insert(name.into(), json!(budget.rows(db, sql, &[owners.into()])?));
        }
        let corrupt: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_metadata_subjects s LEFT JOIN catalog_metadata_schema_heads h ON h.item_id=s.item_id LEFT JOIN catalog_metadata_schemas r ON r.item_id=h.item_id AND r.schema_fingerprint=h.schema_fingerprint LEFT JOIN catalog_metadata_projection p ON p.item_id=s.item_id AND p.schema_fingerprint=h.schema_fingerprint WHERE s.item_id IN(SELECT value FROM json_each(?1)) AND (h.item_id IS NULL OR r.item_id IS NULL OR p.item_id IS NULL)) OR EXISTS(SELECT 1 FROM catalog_metadata_sources s LEFT JOIN catalog_metadata_schemas r ON r.item_id=s.item_id AND r.schema_fingerprint=s.schema_fingerprint WHERE s.item_id IN(SELECT value FROM json_each(?1)) AND r.item_id IS NULL) OR EXISTS(SELECT 1 FROM catalog_metadata_schemas s LEFT JOIN catalog_metadata_schemas p ON p.item_id=s.item_id AND p.schema_fingerprint=s.prior_fingerprint WHERE s.item_id IN(SELECT value FROM json_each(?1)) AND s.prior_fingerprint IS NOT NULL AND p.item_id IS NULL) OR EXISTS(SELECT 1 FROM catalog_metadata_tag_suppressions m LEFT JOIN catalog_item_tag_suppressions s ON s.item_id=m.item_id AND s.tag_id=m.tag_id WHERE m.item_id IN(SELECT value FROM json_each(?1)) AND (s.item_id IS NULL OR m.revision>s.revision))", [owners],|r|r.get(0))?;
        if corrupt {
            return Err(fail(
                "incomplete metadata registry or suppression ownership",
            ));
        }
    }
    Ok(())
}

pub(super) fn revisions(
    db: &Connection,
    budget: &mut Budget,
    facts: &Map<String, Value>,
    revisions: &mut BTreeMap<(String, String), Value>,
) -> Result<()> {
    for (table, keys) in [
        ("identities", &["namespace", "value"][..]),
        (
            "catalog_legacy_identity_evidence",
            &["namespace", "value", "item_id"][..],
        ),
        ("catalog_identity_pins", &["item_id", "namespace"][..]),
        (
            "catalog_identity_pin_revisions",
            &["item_id", "namespace", "revision"][..],
        ),
        ("item_tags", &["id"][..]),
        ("tags", &["id"][..]),
        ("tag_names", &["name"][..]),
        ("tag_parents", &["child_id", "parent_id"][..]),
        ("catalog_item_tag_suppressions", &["item_id", "tag_id"][..]),
        (
            "catalog_tag_suppression_revisions",
            &["subject_type", "subject_id", "tag_id", "revision"][..],
        ),
        ("catalog_tag_redirects", &["source_tag_id"][..]),
        ("catalog_tag_merges", &["source_tag_id"][..]),
        (
            "catalog_metadata_schemas",
            &["item_id", "schema_fingerprint"][..],
        ),
        ("catalog_metadata_schema_heads", &["item_id"][..]),
        (
            "catalog_metadata_tag_history",
            &["item_id", "namespace", "record_id", "revision"][..],
        ),
        (
            "catalog_metadata_tag_suppressions",
            &["item_id", "tag_id"][..],
        ),
    ] {
        if let Some(rows) = facts.get(table).and_then(Value::as_array) {
            for row in rows {
                let key = keys
                    .iter()
                    .map(|key| {
                        row.get(*key)
                            .cloned()
                            .ok_or_else(|| fail("missing domain revision key"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                capture_revision(db, budget, revisions, table, json!(key), true)?;
            }
        }
    }
    Ok(())
}
