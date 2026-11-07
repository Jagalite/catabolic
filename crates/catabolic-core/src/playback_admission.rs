// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Playback admission policy over counts and capacity observed by the store.
//! The store must revalidate the facts in its owning transaction.
pub fn session_allowed(global_active: i64, principal_active: i64) -> bool {
    global_active < 100 && principal_active < 10
}
pub fn preset_supported(preset: &str) -> bool {
    matches!(preset, "h264-720p" | "h264-1080p")
}
pub fn reservation_available(
    reserved: i64,
    available: u128,
    output_max: u64,
    reserve_floor: u64,
) -> bool {
    reserved >= 0
        && available >= reserved as u128 + u128::from(output_max) + u128::from(reserve_floor)
}
/// A cache can retire only after all live session demand has ended. The
/// filesystem adapter rechecks these facts before deleting cache contents.
pub fn cache_retirement_allowed(
    state: &str,
    directory_present: bool,
    active_session: bool,
    updated_at: f64,
    now: f64,
) -> bool {
    state != "running"
        && (state != "stale" || directory_present)
        && !active_session
        && (state != "ready" || updated_at < now - 3600.0)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_retirement_requires_no_live_demand_and_an_old_ready_cache() {
        assert!(!cache_retirement_allowed("ready", true, true, 0.0, 4000.0));
        assert!(!cache_retirement_allowed(
            "ready", true, false, 400.0, 4000.0
        ));
        assert!(cache_retirement_allowed(
            "ready", true, false, 399.0, 4000.0
        ));
        assert!(!cache_retirement_allowed(
            "running", true, false, 0.0, 4000.0
        ));
        assert!(!cache_retirement_allowed(
            "stale", false, false, 0.0, 4000.0
        ));
        assert!(cache_retirement_allowed("stale", true, false, 0.0, 4000.0));
    }
    #[test]
    fn limits_and_reservations_do_not_overcommit() {
        assert!(session_allowed(99, 9));
        assert!(!session_allowed(100, 0));
        assert!(!session_allowed(0, 10));
        assert!(preset_supported("h264-720p"));
        assert!(!preset_supported("h265"));
        assert!(reservation_available(5, 15, 7, 3));
        assert!(!reservation_available(5, 14, 7, 3));
        assert!(!reservation_available(-1, u128::MAX, 0, 0));
    }
}
