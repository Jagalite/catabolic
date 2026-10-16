// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Read-only trusted-operator inspection, not scoped application admission.
//! Existing native migration owners produce every field. Planning neither
//! establishes quiescence nor executes an upgrade or changes schema defaults.
use pyo3::prelude::*;
use std::path::Path;

#[pyfunction(name = "native_catalog_inspect")]
#[pyo3(signature=(path, full=false))]
fn inspect(py: Python<'_>, path: &str, full: bool) -> PyResult<String> {
    super::result(py.detach(|| catabolic_store::migration::inspect_native(Path::new(path), full)))
}

#[pyfunction(name = "native_catalog_plan_upgrade")]
fn plan_upgrade(py: Python<'_>, path: &str) -> PyResult<String> {
    super::result(py.detach(|| catabolic_store::migration::plan_native(Path::new(path))))
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(inspect, module)?)?;
    module.add_function(wrap_pyfunction!(plan_upgrade, module)?)?;
    Ok(())
}
