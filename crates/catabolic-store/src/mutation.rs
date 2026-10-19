// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Shared admission checks for native catalog mutations.
use crate::{Error, Result, Store};
use rusqlite::Connection;

pub fn profile(store: &Store, profile: &str) -> Result<()> {
    if !store.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM profiles WHERE id=?)",
        [profile],
        |r| r.get::<_, bool>(0),
    )? {
        return Err(Error(format!("unknown profile: {profile}")));
    }
    Ok(())
}

pub fn require_recovered(db: &Connection) -> Result<()> {
    if db.query_row("SELECT EXISTS(SELECT 1 FROM journal)", [], |r| {
        r.get::<_, bool>(0)
    })? {
        return Err(Error(
            "recover pending operations before changing desired catalog state".into(),
        ));
    }
    Ok(())
}

/// Match datetime.now(timezone.utc).isoformat(), including microsecond precision.
pub fn timestamp() -> Result<String> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| Error(e.to_string()))?;
    let seconds = libc::time_t::try_from(duration.as_secs())
        .map_err(|_| Error("time is outside the supported range".into()))?;
    let mut time = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: both pointers refer to valid storage; gmtime_r initializes the
    // output on success and retains neither pointer.
    if unsafe { libc::gmtime_r(&seconds, time.as_mut_ptr()) }.is_null() {
        return Err(Error("UTC time conversion failed".into()));
    }
    // SAFETY: gmtime_r returned non-null and initialized the output above.
    let time = unsafe { time.assume_init() };
    let fraction = if duration.subsec_micros() == 0 {
        String::new()
    } else {
        format!(".{:06}", duration.subsec_micros())
    };
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{fraction}+00:00",
        time.tm_year + 1900,
        time.tm_mon + 1,
        time.tm_mday,
        time.tm_hour,
        time.tm_min,
        time.tm_sec
    ))
}
