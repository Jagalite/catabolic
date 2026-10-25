//! Exact native `motion_artwork` contract-1 references and pure transitions.
//!
//! These are descriptive references, NEVER byte-read authority, filesystem
//! locators, proof of image decoding, or permission to publish an artifact. The
//! adapter separately owns authorization, verified Motion asset bytes, aggregate
//! and metadata-source CAS, durable receipts, and atomic persistence.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const CONTRACT_VERSION: u32 = 1;
pub const ASSET_OWNER: &str = "motion_asset_service";
/// Historical exporter text is preserved exactly. Despite its wording, a null
/// selection uses automatic fallback in the existing executable artwork resolver.
pub const SEMANTICS: &str = "exact source references; null selected asset is explicit suppression; bytes remain Motion owned";
pub const MAX_REFERENCE_BYTES: usize = 64 * 1024;
pub const MAX_REFERENCE_NODES: usize = 8192;
pub const MAX_REFERENCE_DEPTH: usize = 32;
pub const MAX_IDENTIFIER_BYTES: usize = 256;
pub const MAX_SOURCE_BYTES: usize = 64;
pub const MAX_ASSET_BYTES: i64 = 8 * 1024 * 1024;
pub const MAX_DIMENSION: i64 = 8192;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetReference {
    /// Historical IDs are opaque; they need not equal the content digest.
    pub asset_id: String,
    pub mime: String,
    pub width: i64,
    pub height: i64,
    pub size_bytes: i64,
    pub content_sha256: String,
    pub owner: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContributionReference {
    pub role: String,
    pub source: String,
    pub asset_id: String,
    pub revision: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionReference {
    pub role: String,
    // The wire requires this key even when its value is null.
    #[serde(deserialize_with = "required_nullable_asset")]
    pub asset_id: Option<String>,
    pub revision: i64,
}
fn required_nullable_asset<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireState {
    contract_version: u32,
    item_id: String,
    contributions: Vec<ContributionReference>,
    selections: Vec<SelectionReference>,
    associated_asset_ids: Vec<String>,
    assets: Vec<AssetReference>,
    semantics: String,
}

/// Validated exact reference closure. Construction is bounded; this deliberately
/// does not implement Deserialize or expose mutable fields that skip validation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ReferenceState(WireState);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferenceError {
    InvalidJson,
    ByteBudget,
    NodeBudget,
    DepthBudget,
    UnsupportedContract,
    WrongItem,
    InvalidIdentifier,
    InvalidRole,
    InvalidSource,
    InvalidRevision,
    InvalidAsset,
    InvalidDigest,
    InvalidProvenance,
    DuplicateReference,
    IncompleteAssetClosure,
    AssetConflict,
    SelectionConflict,
    InvalidSelection,
}
impl std::fmt::Display for ReferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "native artwork reference refused: {self:?}")
    }
}
impl std::error::Error for ReferenceError {}
pub type Result<T> = std::result::Result<T, ReferenceError>;

impl ReferenceState {
    /// Rejects duplicate JSON keys, unknown/missing fields, malformed descriptors
    /// and incomplete ownership closure. Input array ordering is preserved.
    pub fn parse_json(raw: &str, expected_item: &str) -> Result<Self> {
        identifier(expected_item)?;
        syntax_budget(raw)?;
        // Every object is a deny_unknown_fields struct. Serde therefore rejects
        // duplicate struct keys rather than silently retaining the last value.
        let wire: WireState = serde_json::from_str(raw).map_err(|_| ReferenceError::InvalidJson)?;
        let state = Self(wire);
        state.validate(expected_item)?;
        Ok(state)
    }

    pub fn empty(item: &str) -> Result<Self> {
        identifier(item)?;
        Ok(Self(WireState {
            contract_version: CONTRACT_VERSION,
            item_id: item.into(),
            contributions: vec![],
            selections: vec![],
            associated_asset_ids: vec![],
            assets: vec![],
            semantics: SEMANTICS.into(),
        }))
    }

    pub fn item_id(&self) -> &str {
        &self.0.item_id
    }
    pub fn contributions(&self) -> &[ContributionReference] {
        &self.0.contributions
    }
    pub fn selections(&self) -> &[SelectionReference] {
        &self.0.selections
    }
    pub fn associated_asset_ids(&self) -> &[String] {
        &self.0.associated_asset_ids
    }
    pub fn assets(&self) -> &[AssetReference] {
        &self.0.assets
    }

    /// A descriptor is not a byte-access grant, even when its digest matches.
    pub fn asset_reference(&self, asset: &str) -> Option<&AssetReference> {
        self.0
            .assets
            .iter()
            .find(|reference| reference.asset_id == asset)
    }

    pub fn to_json(&self) -> Result<String> {
        bounded_encode(&self.0)
    }

    fn validate(&self, expected_item: &str) -> Result<()> {
        if self.0.contract_version != CONTRACT_VERSION {
            return Err(ReferenceError::UnsupportedContract);
        }
        identifier(&self.0.item_id)?;
        if self.0.item_id != expected_item {
            return Err(ReferenceError::WrongItem);
        }
        if self.0.semantics != SEMANTICS {
            return Err(ReferenceError::InvalidProvenance);
        }
        // Exact value-node count for this fixed shape: root+seven fields, then
        // five per contribution, four per selection, eight per asset descriptor.
        let nodes = 8usize
            .saturating_add(self.0.contributions.len().saturating_mul(5))
            .saturating_add(self.0.selections.len().saturating_mul(4))
            .saturating_add(self.0.associated_asset_ids.len())
            .saturating_add(self.0.assets.len().saturating_mul(8));
        if nodes > MAX_REFERENCE_NODES {
            return Err(ReferenceError::NodeBudget);
        }
        let mut referenced = BTreeSet::new();
        let mut pairs = BTreeSet::new();
        for contribution in &self.0.contributions {
            // The historical contribution table does not admit banner. Banner
            // is a v2 selection role and can select an associated upload.
            contribution_role(&contribution.role)?;
            source(&contribution.source)?;
            identifier(&contribution.asset_id)?;
            if contribution.revision < 1 {
                return Err(ReferenceError::InvalidRevision);
            }
            if !pairs.insert((&contribution.role, &contribution.source)) {
                return Err(ReferenceError::DuplicateReference);
            }
            referenced.insert(contribution.asset_id.as_str());
        }
        let mut roles = BTreeSet::new();
        for selection in &self.0.selections {
            role(&selection.role)?;
            if selection.revision < 1 {
                return Err(ReferenceError::InvalidRevision);
            }
            if !roles.insert(&selection.role) {
                return Err(ReferenceError::DuplicateReference);
            }
            if let Some(asset) = &selection.asset_id {
                identifier(asset)?;
                referenced.insert(asset);
            }
        }
        let mut associations = BTreeSet::new();
        for asset in &self.0.associated_asset_ids {
            identifier(asset)?;
            if !associations.insert(asset) {
                return Err(ReferenceError::DuplicateReference);
            }
            referenced.insert(asset);
        }
        let mut descriptors = BTreeSet::new();
        for asset in &self.0.assets {
            validate_asset(asset)?;
            if !descriptors.insert(asset.asset_id.as_str()) {
                return Err(ReferenceError::DuplicateReference);
            }
        }
        if referenced != descriptors {
            return Err(ReferenceError::IncompleteAssetClosure);
        }
        self.to_json()?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResolvedRole {
    pub role: String,
    pub asset_id: Option<String>,
    pub selection_revision: i64,
    pub pinned: bool,
    pub sources: Vec<String>,
    pub conflict: bool,
}

/// Exactly the executable v2 resolver policy, including automatic fallback for
/// a retained null selection. Uploaded associations alone are not candidates for
/// automatic choice, but may be explicitly selected by plan_select.
pub fn resolve_role(state: &ReferenceState, requested_role: &str) -> Result<ResolvedRole> {
    role(requested_role)?;
    let selection = state.selections().iter().find(|s| s.role == requested_role);
    let pin = selection.and_then(|s| s.asset_id.clone());
    let pinned = pin.is_some();
    let local = state
        .contributions()
        .iter()
        .find(|c| c.role == requested_role && c.source == "local")
        .map(|c| c.asset_id.clone());
    let candidates = state
        .contributions()
        .iter()
        .filter(|c| c.role == requested_role)
        .map(|c| c.asset_id.clone())
        .collect();
    let (asset_id, conflict) = super::resolve(pin, local, &candidates);
    let sources = state
        .contributions()
        .iter()
        .filter(|c| {
            c.role == requested_role
                && asset_id
                    .as_ref()
                    .is_none_or(|selected| selected == &c.asset_id)
        })
        .map(|c| c.source.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(ResolvedRole {
        role: requested_role.into(),
        asset_id,
        selection_revision: selection.map_or(0, |s| s.revision),
        pinned,
        sources,
        conflict,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssociationChange {
    Unchanged,
    Associated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AssociationPlan {
    pub change: AssociationChange,
    pub state: ReferenceState,
}

/// Merge a separately verified upload descriptor and item association. Never
/// creates or verifies image bytes. Reusing an ID with different metadata fails;
/// an already identical association is a true no-op with unchanged wire state.
pub fn plan_associate_upload(
    state: &ReferenceState,
    asset: &AssetReference,
) -> Result<AssociationPlan> {
    validate_asset(asset)?;
    if let Some(existing) = state.asset_reference(&asset.asset_id)
        && existing != asset
    {
        return Err(ReferenceError::AssetConflict);
    }
    if state.associated_asset_ids().contains(&asset.asset_id) {
        return Ok(AssociationPlan {
            change: AssociationChange::Unchanged,
            state: state.clone(),
        });
    }
    let mut next = state.clone();
    if state.asset_reference(&asset.asset_id).is_none() {
        let at = next
            .0
            .assets
            .iter()
            .position(|a| a.asset_id > asset.asset_id)
            .unwrap_or(next.0.assets.len());
        next.0.assets.insert(at, asset.clone());
    }
    let at = next
        .0
        .associated_asset_ids
        .iter()
        .position(|id| id > &asset.asset_id)
        .unwrap_or(next.0.associated_asset_ids.len());
    next.0
        .associated_asset_ids
        .insert(at, asset.asset_id.clone());
    next.validate(state.item_id())?;
    Ok(AssociationPlan {
        change: AssociationChange::Associated,
        state: next,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SelectionPlan {
    pub expected_role_revision: i64,
    pub next_role_revision: i64,
    pub state: ReferenceState,
    pub resolved: ResolvedRole,
}

/// Uses the existing core selection decision, with the v2 candidate union of
/// same-role contributions and item-associated uploads. None retains a revisioned
/// automatic-selection decision. Even selecting the same value advances the role
/// revision, exactly as artwork::select does. Outer aggregate/source CAS remains
/// the adapter's separate obligation.
pub fn plan_select(
    state: &ReferenceState,
    requested_role: &str,
    expected_role_revision: i64,
    requested_asset: Option<&str>,
) -> Result<SelectionPlan> {
    role(requested_role)?;
    if let Some(asset) = requested_asset {
        identifier(asset)?;
    }
    let current = state
        .selections()
        .iter()
        .find(|s| s.role == requested_role)
        .map_or(0, |s| s.revision);
    let candidates: Vec<_> = state
        .contributions()
        .iter()
        .filter(|c| c.role == requested_role)
        .map(|c| c.asset_id.clone())
        .chain(state.associated_asset_ids().iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let revision = super::select(
        current,
        expected_role_revision,
        requested_asset,
        &candidates,
    )
    .map_err(|reason| match reason {
        "invalid_artwork_selection" => ReferenceError::InvalidSelection,
        _ => ReferenceError::SelectionConflict,
    })?;
    let mut next = state.clone();
    let selection = SelectionReference {
        role: requested_role.into(),
        asset_id: requested_asset.map(str::to_owned),
        revision,
    };
    if let Some(existing) = next
        .0
        .selections
        .iter_mut()
        .find(|s| s.role == requested_role)
    {
        *existing = selection;
    } else {
        let at = next
            .0
            .selections
            .iter()
            .position(|s| s.role.as_str() > requested_role)
            .unwrap_or(next.0.selections.len());
        next.0.selections.insert(at, selection);
    }
    // Replacing a historical-only pin can remove the descriptor's last reference.
    // Remove only those now-unreferenced descriptors; no byte deletion is implied.
    let refs: BTreeSet<_> = next
        .0
        .contributions
        .iter()
        .map(|c| c.asset_id.clone())
        .chain(next.0.selections.iter().filter_map(|s| s.asset_id.clone()))
        .chain(next.0.associated_asset_ids.iter().cloned())
        .collect();
    next.0.assets.retain(|asset| refs.contains(&asset.asset_id));
    next.validate(state.item_id())?;
    let resolved = resolve_role(&next, requested_role)?;
    Ok(SelectionPlan {
        expected_role_revision,
        next_role_revision: revision,
        state: next,
        resolved,
    })
}

fn identifier(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(ReferenceError::InvalidIdentifier);
    }
    Ok(())
}
fn role(value: &str) -> Result<()> {
    if !matches!(value, "poster" | "backdrop" | "thumbnail" | "banner") {
        return Err(ReferenceError::InvalidRole);
    }
    Ok(())
}
fn contribution_role(value: &str) -> Result<()> {
    role(value)?;
    if value == "banner" {
        return Err(ReferenceError::InvalidRole);
    }
    Ok(())
}
fn source(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAX_SOURCE_BYTES
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(ReferenceError::InvalidSource);
    }
    Ok(())
}
fn validate_asset(asset: &AssetReference) -> Result<()> {
    identifier(&asset.asset_id)?;
    if asset.owner != ASSET_OWNER {
        return Err(ReferenceError::InvalidProvenance);
    }
    if asset.content_sha256.len() != 64
        || !asset
            .content_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(ReferenceError::InvalidDigest);
    }
    if !matches!(
        asset.mime.as_str(),
        "image/png" | "image/jpeg" | "image/webp"
    ) || !(1..=MAX_DIMENSION).contains(&asset.width)
        || !(1..=MAX_DIMENSION).contains(&asset.height)
        || !(0..=MAX_ASSET_BYTES).contains(&asset.size_bytes)
    {
        return Err(ReferenceError::InvalidAsset);
    }
    Ok(())
}
fn syntax_budget(raw: &str) -> Result<()> {
    if raw.len() > MAX_REFERENCE_BYTES {
        return Err(ReferenceError::ByteBudget);
    }
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for byte in raw.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'[' | b'{' => {
                    depth += 1;
                    if depth > MAX_REFERENCE_DEPTH {
                        return Err(ReferenceError::DepthBudget);
                    }
                }
                b']' | b'}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    Ok(())
}
fn bounded_encode(value: &WireState) -> Result<String> {
    struct Writer {
        bytes: Vec<u8>,
        overflow: bool,
    }
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_REFERENCE_BYTES.saturating_sub(self.bytes.len()) {
                self.overflow = true;
                return Err(std::io::Error::other("artwork reference byte budget"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Writer {
        bytes: vec![],
        overflow: false,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| {
        if writer.overflow {
            ReferenceError::ByteBudget
        } else {
            ReferenceError::InvalidJson
        }
    })?;
    String::from_utf8(writer.bytes).map_err(|_| ReferenceError::InvalidJson)
}

#[cfg(test)]
mod tests;
