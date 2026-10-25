//! Atomic empty Motion edition creation. The host supplies independent exact
//! work/allocation grants; request JSON never supplies arbitrary item metadata,
//! graph policy, identity context authority, files, or equivalence evidence.
use crate::{
    Error, Store,
    catalog_commands::{CommandStoreError, CommandStoreErrorCode},
    catalog_identity_mutation::{
        self, ContextChange, IdentityAuthority, IdentityMutation, TimelineChange, TimelineState,
    },
    catalog_relationships::{
        self, RelationshipAuthority, RelationshipEdge, RelationshipKindRule, RelationshipMutation,
    },
    curation,
};
use catabolic_core::catalog_command::CommandEnvelope;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyEditionRequest {
    pub contract_version: u32,
    pub work_id: String,
    /// Motion reference, not a caller-selected native item identity.
    pub edition_id: String,
    pub timeline_id: String,
    pub relationship_id: String,
    pub context_id: String,
    pub label: String,
    pub request_digest: String,
}
/// Never deserializable. The embedding adapter must independently authorize
/// all containing libraries and allocate these exact IDs under its writer.
#[derive(Clone, Debug)]
pub struct EmptyEditionAuthority {
    pub host_namespace: String,
    pub profile: String,
    pub work_id: String,
    pub expected_work_revision: i64,
    pub edition_id: String,
    pub timeline_id: String,
    pub relationship_id: String,
    pub context_id: String,
    pub context_key: String,
}
fn fail(code: CommandStoreErrorCode, message: &str) -> CommandStoreError {
    CommandStoreError {
        code,
        error: Error(message.into()),
    }
}
fn invalid(message: &str) -> CommandStoreError {
    fail(CommandStoreErrorCode::InvalidOperation, message)
}
fn conflict(message: &str) -> CommandStoreError {
    fail(CommandStoreErrorCode::OperationConflict, message)
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

pub(crate) fn create(
    store: &Store,
    command: &CommandEnvelope,
    request: &EmptyEditionRequest,
    authority: &EmptyEditionAuthority,
    admitted_at_us: i64,
) -> Result<Value, CommandStoreError> {
    if store.db.is_autocommit()
        || authority.host_namespace != command.key().host_namespace()
        || authority.expected_work_revision < 1
        || authority.profile.is_empty()
        || authority.context_key.is_empty()
        || authority.work_id != request.work_id
        || authority.edition_id != request.edition_id
        || authority.timeline_id != request.timeline_id
        || authority.relationship_id != request.relationship_id
        || authority.context_id != request.context_id
    {
        return Err(fail(
            CommandStoreErrorCode::AuthorityDenied,
            "empty edition is outside exact host grants",
        ));
    }
    if request.request_digest.len() != 64
        || !request
            .request_digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("empty edition request digest malformed"));
    }
    if request.contract_version != 1
        || ![
            &request.work_id,
            &request.edition_id,
            &request.timeline_id,
            &request.relationship_id,
            &request.context_id,
        ]
        .into_iter()
        .all(|id| identifier(id))
        || request.timeline_id != request.edition_id
        || request.label.trim() != request.label
        || request.label.is_empty()
        || request.label.chars().count() > 200
        || request.label.chars().any(char::is_control)
    {
        return Err(invalid("unsupported empty edition request"));
    }
    let db = &store.db;
    let work:Option<(String,String,i64,bool)>=db.query_row(
        "SELECT i.kind,i.metadata,r.revision,EXISTS(SELECT 1 FROM catalog_logical_retirements l WHERE l.item_id=i.id) FROM items i JOIN catalog_entity_revisions r ON r.entity_kind='items' AND r.entity_key=json_array(i.id) AND r.deleted=0 WHERE i.id=?",
        [&request.work_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let (kind, metadata, revision, retired) =
        work.ok_or_else(|| conflict("edition work no longer exists"))?;
    let metadata: Value =
        serde_json::from_str(&metadata).map_err(|_| invalid("edition work metadata malformed"))?;
    let structure = &metadata["motion_catalog"];
    if retired || revision != authority.expected_work_revision {
        return Err(conflict("edition work head or retirement changed"));
    }
    if !matches!(kind.as_str(), "video" | "custom:motion_audio")
        || structure["contract_version"] != 1
        || structure["item_id"] != request.work_id
        || structure["kind"] != if kind == "video" { "video" } else { "audio" }
        || matches!(structure["media_type"].as_str(), Some("series" | "season"))
        || structure["media_type"].as_str().is_none()
    {
        return Err(invalid("work cannot own an empty Motion edition"));
    }
    let native_edition = format!("motion.edition:{}", request.edition_id);
    // Identity and relation owners also enforce their retained key ledgers.
    // Check item tombstones explicitly: create-only must never reuse an ID.
    let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM items WHERE id=?1) OR EXISTS(SELECT 1 FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key=json_array(?1))",[&native_edition],|r|r.get(0))?;
    if exists {
        return Err(conflict("edition item ID was already used"));
    }
    let used_identity: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_timelines WHERE id=?1) OR EXISTS(SELECT 1 FROM item_relationships WHERE id=?2) OR EXISTS(SELECT 1 FROM catalog_segmentation_contexts WHERE id=?3) OR EXISTS(SELECT 1 FROM catalog_entity_revisions WHERE (entity_kind='catalog_timelines' AND entity_key=json_array(?1)) OR (entity_kind='item_relationships' AND entity_key=json_array(?2)) OR (entity_kind='catalog_segmentation_contexts' AND entity_key=json_array(?3)))",
        params![request.timeline_id, request.relationship_id, request.context_id], |row| row.get(0),
    )?;
    if used_identity {
        return Err(conflict(
            "allocated edition structural identity was already used",
        ));
    }
    let edition = json!({"id":request.edition_id,"item_id":request.work_id,"label":request.label,"revision":1});
    let timeline = json!({"id":request.timeline_id,"edition_id":request.edition_id,"revision":1,"duration_ms":null,"order_group_id":null,"order_position":null});
    let item = json!({"id":native_edition,"kind":"custom:motion_edition","metadata":{"motion_edition":{"contract_version":1,"source_edition":edition,"source_timelines":[timeline],"editorial_context":{"id":request.context_id,"authority_namespace":authority.host_namespace,"context_key":authority.context_key},"origin":"native_empty_edition","creation_request_digest":request.request_digest}}});
    let created = curation::item_put_typed(store, &item).map_err(|e| {
        fail(
            match e.code {
                curation::ItemPutErrorCode::InvalidInput => CommandStoreErrorCode::InvalidOperation,
                curation::ItemPutErrorCode::Conflict => CommandStoreErrorCode::OperationConflict,
                curation::ItemPutErrorCode::Internal => CommandStoreErrorCode::StoreFailure,
            },
            &e.error.to_string(),
        )
    })?;
    let edge = RelationshipEdge {
        source: request.work_id.clone(),
        target: native_edition.clone(),
        kind: "custom:motion_edition".into(),
    };
    let relationship = RelationshipMutation::Create {
        id: request.relationship_id.clone(),
        source: edge.source.clone(),
        target: edge.target.clone(),
        kind: edge.kind.clone(),
        metadata: json!({}),
    };
    let relation_grant = RelationshipAuthority {
        host_namespace: authority.host_namespace.clone(),
        create_edges: BTreeSet::from([edge.clone()]),
        kind_rules: BTreeMap::from([(edge.kind, RelationshipKindRule::Acyclic)]),
        delete_edges: BTreeMap::new(),
    };
    let relationship =
        catalog_relationships::apply(store, command, &relationship, &relation_grant)?;
    let identity = IdentityMutation {
        contract_version: 1,
        profile: authority.profile.clone(),
        context: ContextChange {
            id: request.context_id.clone(),
            authority_namespace: authority.host_namespace.clone(),
            context_key: authority.context_key.clone(),
            expected_revision: None,
        },
        timelines: vec![TimelineChange {
            timeline_id: request.timeline_id.clone(),
            edition_item_id: native_edition.clone(),
            expected_revision: None,
            identity_state: TimelineState::Provisional,
            coverage: None,
        }],
        versions: vec![],
        evidence: vec![],
    };
    let identity_grant = IdentityAuthority {
        authority_namespace: authority.host_namespace.clone(),
        context_keys: BTreeSet::from([authority.context_key.clone()]),
        allow_declared_evidence: false,
        allow_verified_evidence: false,
    };
    let identity = catalog_identity_mutation::apply_identity(
        store,
        &serde_json::to_value(identity).map_err(|_| invalid("identity serialization failed"))?,
        command,
        &identity_grant,
        admitted_at_us,
    )
    .map_err(|e| {
        use catalog_identity_mutation::IdentityErrorCode as C;
        fail(
            match e.code {
                C::InvalidInput | C::Unsupported => CommandStoreErrorCode::InvalidOperation,
                C::AuthorityDenied => CommandStoreErrorCode::AuthorityDenied,
                C::Conflict => CommandStoreErrorCode::OperationConflict,
                C::StoreFailure => CommandStoreErrorCode::StoreFailure,
            },
            &e.error.to_string(),
        )
    })?;
    let item_revision:i64=db.query_row("SELECT revision FROM catalog_entity_revisions WHERE entity_kind='items' AND entity_key=json_array(?) AND deleted=0",[&native_edition],|r|r.get(0))?;
    let timeline_revision: i64 = db.query_row(
        "SELECT current_revision FROM catalog_timelines WHERE id=? AND edition_item_id=?",
        params![request.timeline_id, native_edition],
        |r| r.get(0),
    )?;
    Ok(
        json!({"operation":"empty_edition_create","contract_version":1,"work_id":request.work_id,"edition_id":request.edition_id,"timeline_id":request.timeline_id,"item":created,"relationship":relationship,"identity":identity,"item_revision_baselines":{(native_edition):item_revision},"timeline_revision":timeline_revision}),
    )
}
