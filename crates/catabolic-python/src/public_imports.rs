//! Immutable Python import values; no database, runtime, or optional engine work.
use crate::public_import_values::{GRAPHQL_PUBLIC_SDL_BYTES, HTTP_CONTRACT_VERSION, TOPICS};
use pyo3::{prelude::*, types::PyTuple};

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add(
        "OCCURRENCES_SQL",
        catabolic_store::query::view_sql("catalog_component_occurrences"),
    )?;
    module.add("TOPICS", PyTuple::new(module.py(), TOPICS.iter().copied())?)?;
    // The native resolver owns LOCAL_SDL. Its frozen public prefix is SDL;
    // the generator proves both the prefix boundary and exact resource bytes.
    module.add(
        "SDL",
        &catabolic_store::graphql::schema_text()[..GRAPHQL_PUBLIC_SDL_BYTES],
    )?;
    module.add("HTTP_CONTRACT_VERSION", HTTP_CONTRACT_VERSION)?;
    Ok(())
}
