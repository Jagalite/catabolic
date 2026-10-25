//! Explicit pins survive provider refresh; local art wins over imported conflicts.
pub mod native;

use std::collections::BTreeSet;
pub fn resolve(
    pinned: Option<String>,
    local: Option<String>,
    candidates: &BTreeSet<String>,
) -> (Option<String>, bool) {
    let asset = pinned.or(local).or_else(|| {
        (candidates.len() == 1)
            .then(|| candidates.first().cloned())
            .flatten()
    });
    let conflict = asset.is_none() && candidates.len() > 1;
    (asset, conflict)
}
pub fn select(
    current_revision: i64,
    expected_revision: i64,
    requested: Option<&str>,
    contributions: &[String],
) -> Result<i64, &'static str> {
    let revision = if current_revision < 0 || current_revision != expected_revision {
        return Err("artwork_selection_conflict");
    } else {
        current_revision
            .checked_add(1)
            .ok_or("artwork_selection_conflict")?
    };
    if requested.is_some_and(|asset| !contributions.iter().any(|c| c == asset)) {
        return Err("invalid_artwork_selection");
    }
    Ok(revision)
}
