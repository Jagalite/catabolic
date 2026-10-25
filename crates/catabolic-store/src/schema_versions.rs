//! Version boundaries shared by resource generation and runtime admission.
/// The frozen legacy target. Existing initialization and upgrade callers retain schema 32.
pub const SCHEMA_VERSION: u32 = 32;
/// The first native schema. This boundary must not move when native versions advance.
pub const FIRST_NATIVE_SCHEMA_VERSION: u32 = 33;
/// The latest explicit opt-in native target; never selected by ordinary Store::open.
pub const NATIVE_SCHEMA_VERSION: u32 = 40;

const _: () = {
    assert!(FIRST_NATIVE_SCHEMA_VERSION == SCHEMA_VERSION + 1);
    assert!(NATIVE_SCHEMA_VERSION >= FIRST_NATIVE_SCHEMA_VERSION);
};
