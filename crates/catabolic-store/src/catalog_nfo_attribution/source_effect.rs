//! Private source-effect capabilities. Neither a generic metadata permit nor a
//! caller-provided namespace can authorize NFO source/assertion changes. The
//! retained row is an independent deferred COMMIT barrier, not durable authority.
use crate::{Error, Result, Store};
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
use rusqlite::{OptionalExtension, params};

fn refused(message: &str) -> Error {
    Error(format!("catalog_nfo_source_effect_refused: {message}"))
}

/// Issued only by an already verified PublicPut/RefreshStep capability. Dropping
/// this value intentionally does not remove the SQLite deferred barrier.
pub(crate) struct NfoSourceEffect {
    subject: String,
    revision: i64,
    key: CommandKey,
    incarnation: String,
    digest: String,
    generation: i64,
    raw: String,
}

pub(super) fn begin_publication_in(
    store: &Store,
    outer: &CommandEnvelope,
    subject: &str,
    revision: i64,
    raw: &str,
) -> Result<NfoSourceEffect> {
    if store.schema_version != 41 || store.db.is_autocommit() || revision < 1 {
        return Err(refused("publication requires candidate41 retained writer"));
    }
    let mut budget = super::ProofBudget::new(store.catalog_operation_budget()?);
    budget.serialized(
        &(
            subject,
            revision,
            outer.key(),
            outer.admission_incarnation(),
            outer.digest().as_str(),
            raw,
        ),
        64,
    )?;
    let exact: bool = store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_state WHERE id=1 AND catalog_id=?1 AND incarnation=?2) AND EXISTS(SELECT 1 FROM catalog_metadata_permits WHERE item_id=?3 AND incarnation=?2 AND barrier_id=1) AND NOT EXISTS(SELECT 1 FROM catalog_commit_barrier WHERE id=1)",
        params![outer.key().catalog_id(), outer.admission_incarnation(), subject], |r| r.get(0))?;
    if !exact {
        return Err(refused("exact enclosing metadata barrier absent"));
    }
    let current: Option<i64> = store.db.query_row("SELECT revision FROM catalog_metadata_sources WHERE item_id=? AND namespace='motion.nfo' AND record_id='document'", [subject], |r| r.get(0)).optional()?;
    if current.unwrap_or(0).checked_add(1) != Some(revision) {
        return Err(refused("source step revision differs"));
    }
    let old: Option<i64> = store.db.query_row("SELECT revision FROM catalog_nfo_source_effect_permits WHERE item_id=? AND incarnation=? AND command_catalog_id=? AND command_host_namespace=? AND command_operation_id=? AND outer_digest=? AND mode='publication'", params![subject,outer.admission_incarnation(),outer.key().catalog_id(),outer.key().host_namespace(),outer.key().operation_id(),outer.digest().as_str()], |r|r.get(0)).optional()?;
    if let Some(old) = old {
        // A multi-observation batch may advance this exact subject again, but
        // only after its preceding source step has an actual typed state head.
        let published:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_nfo_states s JOIN catalog_nfo_state_revisions h ON h.item_id=s.item_id AND h.revision=s.current_revision WHERE s.item_id=? AND h.source_revision=?)",params![subject,old],|r|r.get(0))?;
        if current != Some(old) || !published {
            return Err(refused("previous step not published"));
        }
        store.db.execute("UPDATE catalog_nfo_source_effect_permits SET revision=?,raw_mutation_json=? WHERE item_id=? AND revision=?",params![revision,raw,subject,old])?;
    } else {
        store.db.execute("INSERT INTO catalog_nfo_source_effect_permits(item_id,incarnation,revision,command_catalog_id,command_host_namespace,command_operation_id,outer_digest,raw_mutation_json,mode) VALUES (?,?,?,?,?,?,?,?,'publication')",params![subject,outer.admission_incarnation(),revision,outer.key().catalog_id(),outer.key().host_namespace(),outer.key().operation_id(),outer.digest().as_str(),raw])?;
    }
    Ok(NfoSourceEffect {
        subject: subject.into(),
        revision,
        key: outer.key().clone(),
        incarnation: outer.admission_incarnation().into(),
        digest: outer.digest().as_str().into(),
        generation: outer.expected_generation(),
        raw: raw.into(),
    })
}
impl NfoSourceEffect {
    pub(crate) fn authorize_tags_in(
        &self,
        store: &Store,
        command: &CommandEnvelope,
        subject: &str,
        namespace: &str,
        record: &str,
        revision: i64,
    ) -> Result<()> {
        if command.key() != &self.key
            || command.admission_incarnation() != self.incarnation
            || command.digest().as_str() != self.digest
            || command.expected_generation() != self.generation
            || subject != self.subject
            || namespace != "motion.nfo"
            || record != "document"
            || revision != self.revision
        {
            return Err(refused("exact source-tag capability differs"));
        }
        let exact:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_nfo_source_effect_permits WHERE item_id=? AND incarnation=? AND revision=? AND command_catalog_id=? AND command_host_namespace=? AND command_operation_id=? AND outer_digest=? AND raw_mutation_json=? AND mode='publication')",params![self.subject,self.incarnation,self.revision,self.key.catalog_id(),self.key.host_namespace(),self.key.operation_id(),self.digest,self.raw],|r|r.get(0))?;
        if !exact {
            return Err(refused("retained exact source-tag capability absent"));
        }
        Ok(())
    }
}

/// Only terminal typed-state publication calls this, after its complete source,
/// assertion, provenance and state postchecks. The enclosing metadata barrier
/// remains until every such release and terminal check succeeds.
pub(super) fn finish_publication_in(
    store: &Store,
    outer: &CommandEnvelope,
    subject: &str,
    revision: i64,
) -> Result<()> {
    let exact:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_nfo_source_effect_permits p JOIN catalog_metadata_sources d ON d.item_id=p.item_id AND d.namespace='motion.nfo' AND d.record_id='document' AND d.revision=p.revision JOIN catalog_nfo_states s ON s.item_id=p.item_id JOIN catalog_nfo_state_revisions h ON h.item_id=s.item_id AND h.revision=s.current_revision AND h.source_revision=d.revision WHERE p.item_id=? AND p.revision=? AND p.incarnation=? AND p.command_catalog_id=? AND p.command_host_namespace=? AND p.command_operation_id=? AND p.outer_digest=? AND p.mode='publication' AND h.command_catalog_id=p.command_catalog_id AND h.command_host_namespace=p.command_host_namespace AND h.command_operation_id=p.command_operation_id)",params![subject,revision,outer.admission_incarnation(),outer.key().catalog_id(),outer.key().host_namespace(),outer.key().operation_id(),outer.digest().as_str()],|r|r.get(0))?;
    if !exact {
        return Err(refused("terminal source/state/command links differ"));
    }
    if store.db.execute("DELETE FROM catalog_nfo_source_effect_permits WHERE item_id=? AND revision=? AND incarnation=?",params![subject,revision,outer.admission_incarnation()])? !=1 { return Err(refused("source barrier release differs")); }
    Ok(())
}

/// Fresh, descriptor-validated import only. This is not a bootstrap/provenance
/// grant. Original public revisions/identity/origin still need bootstrap proof.
pub(crate) struct ImportedSourceEffect<'a> {
    outer: &'a CommandEnvelope,
    subject: &'a str,
    source: &'a crate::catalog_import_typed::ImportedContribution,
    tags: Option<&'a crate::catalog_import_typed::ImportedTagSet>,
}
pub(crate) fn begin_import_in<'a>(
    store: &Store,
    validated: &crate::catalog_import::ValidatedNfoImport<'a>,
    subject: &str,
) -> Result<ImportedSourceEffect<'a>> {
    let (subject, source, tags) = validated.source(subject)?;
    let outer = validated.command();
    if store.schema_version != 41
        || store.db.is_autocommit()
        || source.record_id != "document"
        || source.kind != crate::catalog_import_typed::ImportedSourceKind::Provider
        || tags.is_some_and(|t| {
            t.source_revision != source.source_revision || !t.excluded_tags.is_empty()
        })
    {
        return Err(refused("import source contract/paired tags differ"));
    }
    let absent:bool=store.db.query_row("SELECT NOT EXISTS(SELECT 1 FROM catalog_nfo_bootstrap) AND NOT EXISTS(SELECT 1 FROM catalog_nfo_states WHERE item_id=?1) AND NOT EXISTS(SELECT 1 FROM catalog_metadata_sources WHERE item_id=?1 AND namespace='motion.nfo') AND NOT EXISTS(SELECT 1 FROM catalog_metadata_history WHERE item_id=?1 AND namespace='motion.nfo') AND NOT EXISTS(SELECT 1 FROM item_tags WHERE item_id=?1 AND source='motion.nfo') AND NOT EXISTS(SELECT 1 FROM catalog_metadata_tag_history WHERE item_id=?1 AND namespace='motion.nfo') AND EXISTS(SELECT 1 FROM catalog_state WHERE id=1 AND catalog_id=?2 AND incarnation=?3)",params![subject,outer.key().catalog_id(),outer.admission_incarnation()],|r|r.get(0))?;
    if !absent {
        return Err(refused(
            "fresh pre-bootstrap import required; existing ownership needs composition",
        ));
    }
    let mut budget = super::ProofBudget::new(store.catalog_operation_budget()?);
    budget.serialized(
        &(
            subject,
            outer.key(),
            outer.admission_incarnation(),
            outer.digest().as_str(),
        ),
        64,
    )?;
    store.db.execute("INSERT INTO catalog_nfo_source_effect_permits(item_id,incarnation,revision,command_catalog_id,command_host_namespace,command_operation_id,outer_digest,raw_mutation_json,mode) VALUES (?,?,1,?,?,?,?,?,'import')",params![subject,outer.admission_incarnation(),outer.key().catalog_id(),outer.key().host_namespace(),outer.key().operation_id(),outer.digest().as_str(),""])?;
    Ok(ImportedSourceEffect {
        outer,
        subject,
        source,
        tags,
    })
}
impl ImportedSourceEffect<'_> {
    fn check_in(&self, store: &Store, command: &CommandEnvelope) -> Result<()> {
        if store.schema_version != 41
            || store.db.is_autocommit()
            || command.key() != self.outer.key()
            || command.admission_incarnation() != self.outer.admission_incarnation()
        {
            return Err(refused("import child identity differs"));
        }
        let exact:bool=store.db.query_row("SELECT NOT EXISTS(SELECT 1 FROM catalog_nfo_bootstrap) AND EXISTS(SELECT 1 FROM catalog_nfo_source_effect_permits WHERE item_id=? AND incarnation=? AND revision=1 AND command_catalog_id=? AND command_host_namespace=? AND command_operation_id=? AND outer_digest=? AND mode='import')",params![self.subject,self.outer.admission_incarnation(),self.outer.key().catalog_id(),self.outer.key().host_namespace(),self.outer.key().operation_id(),self.outer.digest().as_str()],|r|r.get(0))?;
        if !exact {
            return Err(refused("retained validated-import capability absent"));
        }
        Ok(())
    }
    pub(crate) fn authorize_tags_in(
        &self,
        store: &Store,
        command: &CommandEnvelope,
        tags: &crate::catalog_import_typed::ImportedTagSet,
    ) -> Result<()> {
        self.check_in(store, command)?;
        if self.tags != Some(tags) {
            return Err(refused("import tags differ from complete validated input"));
        }
        Ok(())
    }
    pub(crate) fn authorize_v1_in(
        &self,
        store: &Store,
        command: &CommandEnvelope,
        mutation: &crate::catalog_metadata_store::MetadataMutation,
    ) -> Result<()> {
        use crate::catalog_metadata_store::MetadataChange;
        self.check_in(store, command)?;
        let MetadataChange::Patch {
            subject,
            namespace,
            record_id,
            expected_revision,
            edits_json,
            evidence_json,
            ..
        } = &mutation.change
        else {
            return Err(refused("import source must be first Patch"));
        };
        let mut budget = super::ProofBudget::new(store.catalog_operation_budget()?);
        budget.serialized(&(edits_json, evidence_json, self.source), 64)?;
        let fields = self
            .source
            .fields
            .as_object()
            .ok_or_else(|| refused("import fields not object"))?;
        let edits = ImportEdits(fields);
        let evidence = ImportEvidence {
            import_operation_id: self.outer.key().operation_id(),
            source_evidence: &self.source.evidence,
            source_revision: self.source.source_revision,
        };
        budget.serialized(&edits, 64)?;
        budget.serialized(&evidence, 64)?;
        if subject != self.subject
            || namespace != "motion.nfo"
            || record_id != "document"
            || *expected_revision != 0
            || edits_json != &serde_json::to_string(&edits)?
            || evidence_json != &serde_json::to_string(&evidence)?
        {
            return Err(refused(
                "first imported Patch differs from frozen contribution",
            ));
        }
        budget.serialized(mutation, 64)?;
        self.bind_raw_in(store, &serde_json::to_string(mutation)?)
    }
    pub(crate) fn authorize_v2_in(
        &self,
        store: &Store,
        command: &CommandEnvelope,
        subject: &str,
        namespace: &str,
        record: &str,
        fields: &serde_json::Value,
        evidence: &serde_json::Value,
    ) -> Result<()> {
        self.check_in(store, command)?;
        let mut budget = super::ProofBudget::new(store.catalog_operation_budget()?);
        budget.serialized(&(fields, evidence), 64)?;
        if subject != self.subject
            || namespace != "motion.nfo"
            || record != "document"
            || fields != &self.source.fields
            || evidence.as_object().map(|m| m.len()) != Some(3)
            || evidence
                .get("import_operation_id")
                .and_then(serde_json::Value::as_str)
                != Some(self.outer.key().operation_id())
            || evidence
                .get("source_revision")
                .and_then(serde_json::Value::as_i64)
                != Some(self.source.source_revision)
            || evidence.get("source_evidence") != Some(&self.source.evidence)
        {
            return Err(refused("import source differs from frozen contribution"));
        }
        // Exact existing owner wire, not an independently replayable command.
        budget.serialized(command.payload(), 64)?;
        self.bind_raw_in(store, &command.payload().to_string())
    }
    fn bind_raw_in(&self, store: &Store, raw: &str) -> Result<()> {
        if raw.len() > crate::catalog_metadata_store::MAX_RAW_MUTATION_BYTES {
            return Err(refused("import raw effect exceeds bound"));
        }
        if store.db.execute("UPDATE catalog_nfo_source_effect_permits SET raw_mutation_json=? WHERE item_id=? AND raw_mutation_json='' AND mode='import'",params![raw,self.subject])?!=1 {return Err(refused("import source effect already consumed"));}
        Ok(())
    }
    pub(crate) fn finish_in(self, store: &Store, time: i64) -> Result<()> {
        self.check_in(store, self.outer)?;
        let exact:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_nfo_source_effect_permits p JOIN catalog_metadata_history h ON h.item_id=p.item_id AND h.namespace='motion.nfo' AND h.record_id='document' AND h.revision=1 JOIN catalog_metadata_sources s ON s.item_id=h.item_id AND s.namespace=h.namespace AND s.record_id=h.record_id AND s.revision=h.revision WHERE p.item_id=?1 AND p.raw_mutation_json=h.raw_mutation_json AND h.command_catalog_id=p.command_catalog_id AND h.command_host_namespace=p.command_host_namespace AND h.command_operation_id=p.command_operation_id AND s.fields_json=h.fields_json AND s.evidence_json=h.evidence_json AND s.schema_fingerprint=h.schema_fingerprint) AND NOT EXISTS(SELECT 1 FROM catalog_metadata_history WHERE item_id=?1 AND namespace='motion.nfo' AND (record_id<>'document' OR revision<>1))",[self.subject],|r|r.get(0))?;
        if !exact {
            return Err(refused("import source/history completion differs"));
        }
        let mut budget = super::ProofBudget::new(store.catalog_operation_budget()?);
        #[derive(serde::Serialize)]
        struct Note<'a> {
            import_operation_id: &'a str,
            source_revision: i64,
        }
        let note = Note {
            import_operation_id: self.outer.key().operation_id(),
            source_revision: self.source.source_revision,
        };
        budget.serialized(&note, 64)?;
        budget.charge(crate::operation_budget::BudgetCharge {
            bytes: 64,
            work: 1,
            ..Default::default()
        })?;
        let timestamp = chrono::DateTime::from_timestamp_micros(time)
            .ok_or_else(|| refused("invalid import time"))?
            .to_rfc3339();
        let note = serde_json::to_string(&note)?;
        let expected = self.tags.map_or(0, |t| t.tags.len());
        let count: i64 = store.db.query_row(
            "SELECT count(*) FROM item_tags WHERE item_id=? AND source='motion.nfo'",
            [self.subject],
            |r| r.get(0),
        )?;
        if usize::try_from(count).ok() != Some(expected) {
            return Err(refused("complete imported assertion count differs"));
        }
        for name in self.tags.into_iter().flat_map(|t| &t.tags) {
            // Typed imports accept native normalized names. This is the existing
            // owner's normalizer, not the stricter archived-name proof policy.
            super::import_provenance::reserve_normalization(name, &mut budget)?;
            let canonical = crate::tagging::tag_name(&serde_json::json!(name))?;
            let exact:bool=store.db.query_row("SELECT EXISTS(SELECT 1 FROM item_tags a JOIN tag_names n ON n.tag_id=a.tag_id WHERE a.item_id=? AND a.source='motion.nfo' AND a.active=1 AND a.confidence IS NULL AND a.note=?3 AND a.created_at=?4 AND a.updated_at=?4 AND n.name=?2)",params![self.subject,canonical,note,timestamp],|r|r.get(0))?;
            if !exact {
                return Err(refused("imported requested-name assertion missing"));
            }
        }
        if store.db.execute(
            "DELETE FROM catalog_nfo_source_effect_permits WHERE item_id=? AND mode='import'",
            [self.subject],
        )? != 1
        {
            return Err(refused("import barrier release differs"));
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
pub(crate) struct ImportEvidence<'a> {
    pub(crate) import_operation_id: &'a str,
    pub(crate) source_evidence: &'a serde_json::Value,
    pub(crate) source_revision: i64,
}
pub(crate) struct ImportEdits<'a>(pub(crate) &'a serde_json::Map<String, serde_json::Value>);
impl serde::Serialize for ImportEdits<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        #[derive(serde::Serialize)]
        struct Set<'a> {
            op: &'static str,
            value: &'a serde_json::Value,
        }
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.0 {
            map.serialize_entry(key, &Set { op: "set", value })?;
        }
        map.end()
    }
}

#[cfg(test)]
mod tests;
