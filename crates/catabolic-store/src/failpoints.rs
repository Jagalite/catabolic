// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Test-only observation hooks for lifecycle operations (native restore,
//! native backup and journal conversion).
//!
//! The hooks exist only with the off-by-default `failpoints` feature, which
//! the integration tests enable and catabolic-cli and the wheel never do.
//! Without it the observer is a zero-sized value and every hook call is an
//! inlined `Ok(())`, so release builds can neither run nor fail callbacks at
//! these points.
use crate::Result;

/// True only in builds compiled with the test-only `failpoints` feature.
/// Shipped binaries assert that it is false.
pub const FAILPOINTS_ENABLED: bool = cfg!(feature = "failpoints");

#[cfg(feature = "failpoints")]
pub(crate) type Observer<'a> = &'a dyn Fn(&str) -> Result<()>;

#[cfg(not(feature = "failpoints"))]
#[derive(Clone, Copy)]
pub(crate) struct Observer<'a>(std::marker::PhantomData<&'a ()>);

#[cfg(feature = "failpoints")]
fn ignore(_: &str) -> Result<()> {
    Ok(())
}

/// The observer used by every production entry point.
pub(crate) fn none() -> Observer<'static> {
    #[cfg(feature = "failpoints")]
    {
        &ignore
    }
    #[cfg(not(feature = "failpoints"))]
    {
        Observer(std::marker::PhantomData)
    }
}

/// Call the observer at a named point. Without the feature this is `Ok(())`.
#[cfg(feature = "failpoints")]
pub(crate) fn hit(observer: Observer<'_>, name: &str) -> Result<()> {
    observer(name)
}

#[cfg(not(feature = "failpoints"))]
#[inline(always)]
pub(crate) fn hit(_: Observer<'_>, _: &str) -> Result<()> {
    Ok(())
}

/// A hook name that is public only in `failpoints` builds.
macro_rules! failpoint_name {
    ($(#[$meta:meta])* $name:ident = $value:literal) => {
        $(#[$meta])*
        #[cfg(feature = "failpoints")]
        pub const $name: &str = $value;
        #[cfg(not(feature = "failpoints"))]
        const $name: &str = $value;
    };
}
pub(crate) use failpoint_name;
