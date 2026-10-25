//! Authority-first, database-only current whole-occurrence facts. The frozen
//! component query and decoder retain normalization/currentness ownership.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    Video,
    Audio,
    Subtitle,
    Attachment,
}
impl ComponentKind {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "video" => Self::Video,
            "audio" => Self::Audio,
            "subtitle" => Self::Subtitle,
            "attachment" => Self::Attachment,
            _ => return None,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentField {
    Language,
    Title,
    Codec,
    Default,
    Forced,
    Commentary,
    HearingImpaired,
    VisualImpaired,
    FormatName,
    Storage,
    TechnicallyVerified,
    RecordedRevision,
    LogicalIdentity,
}
impl ComponentField {
    fn column(self) -> &'static str {
        match self {
            Self::Language => "language",
            Self::Title => "title",
            Self::Codec => "codec",
            Self::Default => "default_flag",
            Self::Forced => "forced",
            Self::Commentary => "commentary",
            Self::HearingImpaired => "hearing_impaired",
            Self::VisualImpaired => "visual_impaired",
            Self::FormatName => "format_name",
            Self::Storage => "storage",
            Self::TechnicallyVerified => "technically_verified",
            Self::RecordedRevision => "revision",
            Self::LogicalIdentity => "component_id",
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct TrustedComponentsRead {
    pub(super) by_role: BTreeMap<String, BTreeMap<ComponentKind, BTreeSet<ComponentField>>>,
}
impl TrustedComponentsRead {
    /// Explicit permission for stored WHOLE physical occurrences, including when
    /// the admitted item is a slice. This does not grant interval playability,
    /// paths, content, effect planning, raw evidence or component history.
    pub fn whole_occurrences(
        roles: Vec<String>,
        kinds: Vec<ComponentKind>,
        fields: Vec<ComponentField>,
    ) -> Result<Self> {
        if roles.is_empty()
            || roles.len() > 32
            || kinds.is_empty()
            || kinds.len() > 4
            || fields.len() > 13
        {
            return Err(fail(ScopeErrorCode::InvalidGrant));
        }
        let fields: BTreeSet<_> = fields.into_iter().collect();
        let kinds: BTreeMap<_, _> = kinds
            .into_iter()
            .map(|kind| (kind, fields.clone()))
            .collect();
        let mut by_role = BTreeMap::new();
        for role in roles {
            crate::curation::validate_file_read_role(&role)
                .map_err(|_| fail(ScopeErrorCode::InvalidGrant))?;
            by_role.insert(role, kinds.clone());
        }
        Ok(Self { by_role })
    }
}
struct Occurrence {
    id: String,
    item: String,
    file: String,
    association: String,
    revision: String,
    role: String,
    kind: ComponentKind,
    probe_job: Option<String>,
    index: Option<i64>,
    fields: BTreeSet<ComponentField>,
}
struct Allowed {
    occurrence: Occurrence,
    row: Value,
    fields: BTreeSet<ComponentField>,
}
struct Reader<'a> {
    store: &'a Store,
    cap: &'a Capability,
    members: &'a BTreeMap<String, MemberVisibility>,
    started: Instant,
    budget: ReadBudget,
    visited: usize,
}
impl Reader<'_> {
    fn tick(&mut self) -> Result<()> {
        remaining(self.started, self.budget)?;
        self.visited += 1;
        if self.visited > self.budget.maximum_ids {
            return Err(fail(ScopeErrorCode::IdBudget));
        }
        Ok(())
    }
    /// Only bounded identity columns are read before exact active association,
    /// live membership, role and kind authority. No normalization is run here.
    fn authorize(&self, id: &str) -> Result<Option<Occurrence>> {
        let raw: Option<Vec<String>> = self.store.db.query_row(
            "SELECT CASE WHEN length(CAST(c.id AS BLOB))<=1024 THEN c.id ELSE '' END,
             CASE WHEN length(CAST(c.item_id AS BLOB))<=1024 THEN c.item_id ELSE '' END,
             CASE WHEN length(CAST(c.file_id AS BLOB))<=1024 THEN c.file_id ELSE '' END,
             CASE WHEN length(CAST(c.association_id AS BLOB))<=1024 THEN c.association_id ELSE '' END,
             CASE WHEN length(CAST(c.revision AS BLOB))<=1024 THEN c.revision ELSE '' END,
             CASE WHEN length(CAST(a.role AS BLOB))<=1024 THEN a.role ELSE '' END,m.kind
             FROM main.component_occurrences c JOIN main.item_files a ON a.id=c.association_id
              AND a.item_id=c.item_id AND a.file_id=c.file_id AND a.active=1
             JOIN main.catalog_live_items i ON i.id=c.item_id
             JOIN main.media_components m ON m.id=c.component_id
             WHERE c.id=? AND c.profile=?",
            params![id,self.cap.profile], |r|(0..7).map(|i|r.get(i)).collect()).optional()?;
        let Some(raw) = raw else { return Ok(None) };
        let Some(kind) = ComponentKind::parse(&raw[6]) else {
            return Ok(None);
        };
        let Some(fields) = self
            .members
            .get(&raw[1])
            .and_then(|m| m.components.by_role.get(&raw[5]))
            .and_then(|m| m.get(&kind))
        else {
            return Ok(None);
        };
        if raw[..5].iter().any(String::is_empty) {
            return Err(fail(ScopeErrorCode::ByteBudget));
        }
        Ok(Some(Occurrence {
            id: raw[0].clone(),
            item: raw[1].clone(),
            file: raw[2].clone(),
            association: raw[3].clone(),
            revision: raw[4].clone(),
            role: raw[5].clone(),
            kind,
            probe_job: None,
            index: None,
            fields: fields.clone(),
        }))
    }
    /// Charge every relevant raw JSON input before the existing SQL functions
    /// or decoder can parse it. Unrelated occurrences/jobs are never expanded.
    fn preflight(&self, node: &mut Occurrence) -> Result<bool> {
        let bytes: i64 = self.store.db.query_row(
            "SELECT length(CAST(c.snapshot AS BLOB))+length(CAST(c.association_evidence AS BLOB))+
             length(CAST(c.component_id AS BLOB))+length(CAST(c.locator AS BLOB))+length(CAST(c.probe_path AS BLOB))+
             coalesce(length(CAST(c.probe_job_id AS BLOB)),0)+length(CAST(a.metadata AS BLOB))
             FROM main.component_occurrences c JOIN main.item_files a ON a.id=c.association_id WHERE c.id=?",
            [&node.id], |r|r.get(0))?;
        physical::charge_raw(bytes)?;
        physical::charge_raw(
            (node.id.len()
                + node.item.len()
                + node.file.len()
                + node.association.len()
                + node.revision.len()
                + node.role.len()) as i64,
        )?;
        let (snapshot, association, locator, job): (String,String,String,Option<String>) = self.store.db.query_row(
            "SELECT snapshot,association_evidence,locator,probe_job_id FROM main.component_occurrences WHERE id=?", [&node.id],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        let (Ok(snapshot), Ok(association), Ok(locator)) = (
            catabolic_core::decode_json(&snapshot),
            catabolic_core::decode_json(&association),
            catabolic_core::decode_json(&locator),
        ) else {
            return Ok(false);
        };
        if !snapshot.is_object()
            || !association.is_object()
            || !locator.is_object()
            || association["id"] != node.association
            || association["item_id"] != node.item
            || association["file_id"] != node.file
            || crate::components::revision(&snapshot) != node.revision
        {
            return Ok(false);
        }
        node.probe_job = job;
        node.index = locator["index"].as_i64();
        if let Some(job) = &node.probe_job
            && (node.index.is_none()
                || !self
                    .store
                    .db
                    .prepare("SELECT 1 FROM main.processing_jobs WHERE id=? AND profile=?")?
                    .exists(params![job, self.cap.profile])?)
        {
            return Ok(false);
        }
        // The decoder owns exact recorded revision including policy and volume.
        // This preflights its strings without opening any filesystem path.
        physical::recorded_revision(self.store, &self.cap.profile, &node.file)?;
        for (sql, args) in [
            (
                "SELECT coalesce(length(CAST(result AS BLOB)),0) FROM main.processing_jobs WHERE id=?",
                vec![node.probe_job.clone().unwrap_or_default()],
            ),
            (
                "SELECT length(CAST(data AS BLOB)) FROM main.file_facts WHERE profile=? AND file_id=? AND operation='probe'",
                vec![self.cap.profile.clone(), node.file.clone()],
            ),
            (
                "SELECT length(CAST(payload AS BLOB))+length(CAST(id AS BLOB))+length(CAST(component_id AS BLOB)) FROM main.component_assertions WHERE occurrence_id=? AND active=1",
                vec![node.id.clone()],
            ),
        ] {
            let sql =
                crate::catalog_source_succession::current::view(self.store.schema_version, sql);
            let mut statement = self.store.db.prepare(&sql)?;
            let mut rows = statement.query(rusqlite::params_from_iter(args))?;
            while let Some(row) = rows.next()? {
                physical::charge_raw(row.get(0)?)?;
            }
        }
        Ok(true)
    }
    fn visit(&mut self, id: &str, path: &mut BTreeSet<String>) -> Result<Option<Allowed>> {
        self.tick()?;
        if path.len() >= 32 || !path.insert(id.to_owned()) {
            return Ok(None);
        }
        let result = self.visit_inner(id, path);
        path.remove(id);
        result
    }
    fn visit_inner(&mut self, id: &str, path: &mut BTreeSet<String>) -> Result<Option<Allowed>> {
        let Some(mut node) = self.authorize(id)? else {
            return Ok(None);
        };
        if !self.preflight(&mut node)? {
            return Ok(None);
        }
        let mut fields = node.fields.clone();
        if let Some(job) = &node.probe_job {
            // Exact frozen-query relevance, without LIMIT 1. LEFT JOIN preserves
            // missing retained inputs so they fail closed rather than disappear.
            let mut statement=self.store.db.prepare(
                "SELECT l.occurrence_id,l.component_id,l.relationship,ji.occurrence_id,ji.evidence,
                  length(CAST(l.occurrence_id AS BLOB))+length(CAST(l.component_id AS BLOB))+
                  length(CAST(l.relationship AS BLOB))+coalesce(length(CAST(ji.occurrence_id AS BLOB)),0)+
                  coalesce(length(CAST(ji.evidence AS BLOB)),0)
                 FROM main.component_output_lineage l JOIN main.processing_artifacts ar ON ar.id=l.artifact_id
                 LEFT JOIN main.component_job_inputs ji ON ji.job_id=ar.job_id AND ji.ordinal=l.ordinal
                 WHERE ar.file_id=? AND ar.profile=? AND ar.job_id=? AND l.output_stream_index=?
                 ORDER BY l.artifact_id COLLATE BINARY,l.ordinal")?;
            // Retrieve only lengths first, before any retained evidence is read.
            let mut lengths=self.store.db.prepare(
                "SELECT length(CAST(l.occurrence_id AS BLOB))+length(CAST(l.component_id AS BLOB))+
                  length(CAST(l.relationship AS BLOB))+coalesce(length(CAST(ji.occurrence_id AS BLOB)),0)+
                  coalesce(length(CAST(ji.evidence AS BLOB)),0)
                 FROM main.component_output_lineage l JOIN main.processing_artifacts ar ON ar.id=l.artifact_id
                 LEFT JOIN main.component_job_inputs ji ON ji.job_id=ar.job_id AND ji.ordinal=l.ordinal
                 WHERE ar.file_id=? AND ar.profile=? AND ar.job_id=? AND l.output_stream_index=?")?;
            let mut rows = lengths.query(params![node.file, self.cap.profile, job, node.index])?;
            let mut count = 0;
            while let Some(row) = rows.next()? {
                count += 1;
                if count > self.budget.maximum_ids {
                    return Err(fail(ScopeErrorCode::IdBudget));
                }
                physical::charge_raw(row.get(0)?)?;
            }
            let mut rows =
                statement.query(params![node.file, self.cap.profile, job, node.index])?;
            while let Some(row) = rows.next()? {
                let source: String = row.get(0)?;
                let component: String = row.get(1)?;
                let relationship: String = row.get(2)?;
                let input: Option<String> = row.get(3)?;
                let evidence: Option<String> = row.get(4)?;
                if input.as_deref() != Some(source.as_str())
                    || !["extracted_copy", "extracted_conversion", "mux_copy"]
                        .contains(&relationship.as_str())
                {
                    return Ok(None);
                }
                let Some(endpoint) = self.visit(&source, path)? else {
                    return Ok(None);
                };
                let Some(evidence) = evidence else {
                    return Ok(None);
                };
                let Ok(evidence) = catabolic_core::decode_json(&evidence) else {
                    return Ok(None);
                };
                let retained = &evidence["occurrence"];
                if !retained.is_object()
                    || retained["occurrence_id"] != source
                    || retained["component_id"] != component
                    || retained["profile"] != self.cap.profile
                    || [
                        "occurrence_id",
                        "component_id",
                        "profile",
                        "item_id",
                        "file_id",
                        "association_id",
                        "revision",
                        "kind",
                    ]
                    .iter()
                    .any(|key| retained[*key] != endpoint.row[*key])
                {
                    return Ok(None);
                }
                fields = fields.intersection(&endpoint.fields).copied().collect();
            }
        }
        // No alternative normalizer or currentness algorithm. All paths and
        // endpoints have now been authorized and raw inputs preflighted.
        let row = crate::component_mutation::get(self.store, &self.cap.profile, id, false)?;
        if row["current"] != true {
            return Ok(None);
        }
        Ok(Some(Allowed {
            occurrence: node,
            row,
            fields,
        }))
    }
}
pub(super) fn read_components(
    store: &Store,
    cap: &Capability,
    members: &BTreeMap<String, MemberVisibility>,
    item: &str,
    started: Instant,
    budget: ReadBudget,
) -> Result<Value> {
    let mask = &members[item].components;
    let mut pairs = vec![];
    for (role, kinds) in &mask.by_role {
        for kind in kinds.keys() {
            pairs.push(json!([role, kind]));
        }
    }
    let pairs = encode(&json!(pairs));
    shared_charge(BudgetCharge {
        bytes: pairs.len(),
        values: 1,
        work: 1,
        ..Default::default()
    })?;
    let mut statement = store.db.prepare(
        "SELECT CASE WHEN length(CAST(c.id AS BLOB))<=1024 THEN c.id ELSE '' END
         FROM main.component_occurrences c JOIN main.item_files a ON a.id=c.association_id
          AND a.item_id=c.item_id AND a.file_id=c.file_id AND a.active=1
         JOIN main.media_components m ON m.id=c.component_id
         JOIN main.catalog_live_items i ON i.id=c.item_id
         WHERE c.profile=? AND c.item_id=? AND EXISTS(SELECT 1 FROM json_each(?) p
          WHERE json_extract(p.value,'$[0]')=a.role AND json_extract(p.value,'$[1]')=m.kind)
         ORDER BY c.id COLLATE BINARY",
    )?;
    let mut rows = statement.query(params![cap.profile, item, pairs])?;
    let mut reader = Reader {
        store,
        cap,
        members,
        started,
        budget,
        visited: 0,
    };
    let mut components = vec![];
    let mut output_bytes = 0usize;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        if id.is_empty() {
            return Err(fail(ScopeErrorCode::ByteBudget));
        }
        let Some(allowed) = reader.visit(&id, &mut BTreeSet::new())? else {
            continue;
        };
        let mut scalars = serde_json::Map::new();
        for field in allowed.fields {
            let value = &allowed.row[field.column()];
            if value.is_array() || value.is_object() {
                return Err(fail(ScopeErrorCode::Catalog));
            }
            scalars.insert(
                if field == ComponentField::RecordedRevision {
                    "recorded_revision"
                } else {
                    field.column()
                }
                .into(),
                value.clone(),
            );
        }
        let n = allowed.occurrence;
        let value = json!({"occurrence_id":n.id,"item_id":n.item,
            "file_id":n.file,"association_id":n.association,"association_role":n.role,"kind":n.kind,
            "part":allowed.row["part"],"physical_scope":"whole_occurrence_recorded","recorded_current":true,"fields":scalars});
        let bytes = encode(&value).len();
        output_bytes = output_bytes.saturating_add(bytes);
        if output_bytes > MAX_BYTES {
            return Err(fail(ScopeErrorCode::ByteBudget));
        }
        shared_charge(BudgetCharge {
            bytes,
            values: 1,
            work: bytes / 64 + 1,
            ..Default::default()
        })?;
        components.push(value);
    }
    Ok(
        json!({"contract_version":1,"item_id":item,"physical_scope":"whole_occurrence_recorded","scoped_total":components.len(),"components":components}),
    )
}
