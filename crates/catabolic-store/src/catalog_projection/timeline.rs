//! Named current timeline facts, including zero-version provisional timelines.
//! Descriptive identity facts only. No available-file or playback authority.
use super::*;
use rusqlite::OptionalExtension;

pub fn named_timeline(
    store: &Store,
    authority: &TrustedProjectionAuthority,
    timeline_id: &str,
    edition_item_id: &str,
) -> Result<Value> {
    if !matches!(store.schema_version, 39 | 40)
        || !store.db.is_autocommit()
        || [timeline_id, edition_item_id]
            .iter()
            .any(|id| id.is_empty() || id.len() > 256 || id.chars().any(char::is_control))
    {
        return Err(Error("unsupported named timeline request".into()));
    }
    store.transaction_available()?;
    store.transaction(|db| {
        let budget=store.catalog_operation_budget()?;
        budget.check().map_err(|e|Error(format!("timeline capture budget: {e:?}")))?;
        let (catalog,incarnation,generation):(String,String,i64)=db.query_row("SELECT catalog_id,incarnation,generation FROM catalog_state WHERE id=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if catalog!=authority.catalog_id || incarnation!=authority.incarnation || generation!=authority.expected_generation {return Err(Error("timeline capture identity or generation changed".into()));}
        let timeline=crate::migration::rows(db,"SELECT t.*,r.identity_state,r.coverage_start_us,r.coverage_end_us,r.command_catalog_id,r.command_host_namespace,r.command_operation_id FROM catalog_timelines t JOIN catalog_timeline_revisions r ON r.timeline_id=t.id AND r.revision=t.current_revision WHERE t.id=? AND t.edition_item_id=?",&[timeline_id.to_owned().into(),edition_item_id.to_owned().into()])?;
        if timeline.len()!=1 {return Err(Error("named timeline or current head is missing".into()));}
        let timeline=&timeline[0];
        let metadata_bytes:Option<i64>=db.query_row("SELECT length(CAST(metadata AS BLOB)) FROM items WHERE id=?",[edition_item_id],|r|r.get(0)).optional()?;
        if metadata_bytes.is_none_or(|n|n<0 || n>4*1024*1024) {return Err(Error("edition metadata byte bound".into()));}
        let missing:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM item_relationships edge JOIN catalog_versions v ON v.association_item_id=edge.target_id LEFT JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision WHERE edge.source_id=? AND edge.kind='custom:motion_version' AND r.version_id IS NULL)",[edition_item_id],|r|r.get(0))?;
        if missing {return Err(Error("edition version current head missing".into()));}
        let edition=crate::migration::rows(db,"SELECT i.id,i.kind,i.metadata,r.revision AS native_item_revision FROM items i JOIN catalog_entity_revisions r ON r.entity_kind='items' AND r.entity_key=json_array(i.id) AND r.deleted=0 WHERE i.id=?",&[edition_item_id.to_owned().into()])?;
        if edition.len()!=1 || edition[0]["kind"]!="custom:motion_edition" {return Err(Error("timeline edition owner is unsupported".into()));}
        let metadata:Value=serde_json::from_str(edition[0]["metadata"].as_str().ok_or_else(||Error("edition metadata missing".into()))?)?;
        if !metadata.is_object() {return Err(Error("edition metadata malformed".into()));}
        let retired=crate::migration::rows(db,"SELECT * FROM catalog_logical_retirements WHERE item_id=?",&[edition_item_id.to_owned().into()])?;
        let versions=crate::migration::rows(db,"SELECT v.id,v.association_item_id,v.current_revision,EXISTS(SELECT 1 FROM catalog_version_retirements x WHERE x.version_id=v.id) AS retired FROM catalog_versions v JOIN catalog_version_revisions r ON r.version_id=v.id AND r.revision=v.current_revision WHERE r.timeline_id=? ORDER BY v.id LIMIT 1001",&[timeline_id.to_owned().into()])?;
        if versions.len()>1000 {return Err(Error("timeline version inventory exceeds bound".into()));}
        // Timeline revisions are created by a real identity command which also
        // records its context revision. A batched import can name several;
        // preserve all, never guess a single context from protected metadata.
        let command=["command_catalog_id","command_host_namespace","command_operation_id"].map(|key|timeline[key].as_str().map(str::to_owned).ok_or_else(||Error("timeline command provenance missing".into())));
        let [catalog_key,namespace_key,operation_key]=command;
        let command=[catalog_key?,namespace_key?,operation_key?];
        let context_count:i64=db.query_row("SELECT count(*) FROM catalog_segmentation_revisions WHERE command_catalog_id=? AND command_host_namespace=? AND command_operation_id=?",params![command[0],command[1],command[2]],|r|r.get(0))?;
        if !(1..=1000).contains(&context_count) {return Err(Error("timeline context inventory exceeds bound".into()));}
        let contexts=crate::migration::rows(db,"SELECT c.id,c.authority_namespace,c.context_key,c.current_revision,r.revision AS timeline_command_context_revision FROM catalog_segmentation_revisions r JOIN catalog_segmentation_contexts c ON c.id=r.context_id JOIN catalog_segmentation_revisions current ON current.context_id=c.id AND current.revision=c.current_revision WHERE r.command_catalog_id=? AND r.command_host_namespace=? AND r.command_operation_id=? ORDER BY c.id,r.revision LIMIT 1001",&command.map(Into::into))?;
        if contexts.len()!=context_count as usize {return Err(Error("timeline context provenance missing or exceeds bound".into()));}
        let head:Option<i64>=db.query_row("SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_timelines' AND entity_key=json_array(?) AND deleted=0",[timeline_id],|r|r.get(0)).optional()?;
        let revision:Option<i64>=db.query_row("SELECT revision FROM catalog_entity_revisions WHERE entity_kind='catalog_timeline_revisions' AND entity_key=json_array(?1,?2) AND deleted=0",params![timeline_id,timeline["current_revision"].as_i64()],|r|r.get(0)).optional()?;
        if head.is_none() || revision.is_none() {return Err(Error("timeline invalidation head missing".into()));}
        let value=json!({"contract_version":1,"catalog_id":catalog,"incarnation":incarnation,"generation":generation,"timeline":timeline,"edition":edition[0],"edition_metadata":metadata,"edition_retirement":retired.first(),"active":retired.is_empty(),"revision_contexts":contexts,"versions":versions,"descriptive_only":true,"playable":false});
        if serde_json::to_vec(&value)?.len()>4*1024*1024 {return Err(Error("timeline capture exceeds byte bound".into()));}
        budget.check().map_err(|e|Error(format!("timeline capture budget: {e:?}")))?;
        Ok(value)
    })
}
