mod artifacts_processing;
mod catalog_refresh;
mod embedding;
mod public_access;
mod public_fallback;
mod public_http;
mod public_import_values;
mod public_imports;
mod public_layouts;
mod public_publisher;
mod public_secret;
mod public_watchers;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::path::Path;

fn result(value: catabolic_store::Result<serde_json::Value>) -> PyResult<String> {
    value
        .map(|v| v.to_string())
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))
}
#[pyfunction]
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
#[pyfunction]
fn register_native_helper(py: Python<'_>, path: &str, sha256: &str) -> PyResult<()> {
    py.detach(|| catabolic_runtime::helper::register(Path::new(path), Some(sha256)))
        .map_err(|error| PyRuntimeError::new_err(error.to_string()))
}
#[pyfunction]
fn native_helper(py: Python<'_>) -> PyResult<String> {
    py.detach(catabolic_runtime::helper::executable)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| PyRuntimeError::new_err(error.to_string()))
}
#[pyfunction]
fn encode_json(raw: &str) -> PyResult<String> {
    let value =
        catabolic_core::decode_json(raw).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    Ok(catabolic_core::encode(&value))
}
#[pyfunction]
fn layout_presets() -> &'static str {
    include_str!("../../catabolic-store/resources/layouts.json")
}
#[pyfunction]
fn component_revision(raw: &str) -> PyResult<String> {
    let value = catabolic_core::decode_json(raw)
        .map_err(|error| embedding::CatabolicError::new_err(error.to_string()))?;
    catabolic_store::public_components::revision(&value)
        .map_err(|error| embedding::CatabolicError::new_err(error.to_string()))
}
#[pyfunction]
fn initialize_database(py: Python<'_>, path: &str) -> PyResult<String> {
    result(py.detach(|| catabolic_store::migration::initialize(Path::new(path))))
}
#[pyfunction]
#[pyo3(signature=(path, full=false))]
fn inspect_database(py: Python<'_>, path: &str, full: bool) -> PyResult<String> {
    result(py.detach(|| catabolic_store::migration::inspect(Path::new(path), full)))
}
#[pyfunction]
#[pyo3(signature=(path, dry_run=false, backup_dir=None))]
fn upgrade_database(
    py: Python<'_>,
    path: &str,
    dry_run: bool,
    backup_dir: Option<&str>,
) -> PyResult<String> {
    result(py.detach(|| {
        catabolic_store::migration::upgrade(
            Path::new(path),
            dry_run,
            backup_dir.map(Path::new),
            &|_, _, _| Ok(()),
        )
    }))
}
#[pyfunction]
#[pyo3(signature=(path, sql=None, profile="default", params=None, describe=false, max_rows=1000, timeout_ms=5000, stable=false, http=false))]
#[allow(clippy::too_many_arguments)]
fn execute_sql(
    py: Python<'_>,
    path: &str,
    sql: Option<&str>,
    profile: &str,
    params: Option<&str>,
    describe: bool,
    max_rows: usize,
    timeout_ms: u64,
    stable: bool,
    http: bool,
) -> PyResult<String> {
    result(py.detach(|| {
        catabolic_store::sql::execute(
            Path::new(path),
            sql,
            &catabolic_store::sql::Options {
                profile,
                params,
                describe,
                max_rows,
                timeout_ms,
                stable,
                http,
            },
        )
    }))
}
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    embedding::register(m)?;
    public_fallback::register(m)?;
    public_imports::register(m)?;
    public_http::register(m)?;
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add_function(wrap_pyfunction!(register_native_helper, m)?)?;
    m.add_function(wrap_pyfunction!(native_helper, m)?)?;
    m.add_function(wrap_pyfunction!(encode_json, m)?)?;
    m.add_function(wrap_pyfunction!(component_revision, m)?)?;
    m.add_function(wrap_pyfunction!(layout_presets, m)?)?;
    m.add_function(wrap_pyfunction!(initialize_database, m)?)?;
    m.add_function(wrap_pyfunction!(inspect_database, m)?)?;
    m.add_function(wrap_pyfunction!(upgrade_database, m)?)?;
    m.add_function(wrap_pyfunction!(execute_sql, m)?)?;
    m.add_function(wrap_pyfunction!(catalog_query, m)?)?;
    m.add_function(wrap_pyfunction!(execute_graphql, m)?)?;
    m.add_function(wrap_pyfunction!(evaluate_read, m)?)?;
    m.add_function(wrap_pyfunction!(tag_mutation, m)?)?;
    m.add_function(wrap_pyfunction!(execute_operation, m)?)?;
    m.add_function(wrap_pyfunction!(interchange, m)?)?;
    Ok(())
}
#[pyfunction]
#[pyo3(signature=(path, entity, options="{}", profile="default"))]
fn catalog_query(
    py: Python<'_>,
    path: &str,
    entity: &str,
    options: &str,
    profile: &str,
) -> PyResult<String> {
    let request =
        catabolic_core::decode_json(options).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    result(py.detach(|| {
        let store = catabolic_store::Store::open(Path::new(path), false, false)?;
        catabolic_store::query::CatalogQuery::new(&store, profile)?.execute(entity, &request)
    }))
}
#[pyfunction]
#[pyo3(signature=(path,document,profile="default",variables="{}",operation_name=None,timeout_ms=5000))]
fn execute_graphql(
    py: Python<'_>,
    path: &str,
    document: &str,
    profile: &str,
    variables: &str,
    operation_name: Option<&str>,
    timeout_ms: u64,
) -> PyResult<String> {
    let variables = catabolic_store::graphql::InputVariables::from_json(variables)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    result(py.detach(|| {
        catabolic_store::graphql::execute(
            Path::new(path),
            document,
            profile,
            variables,
            operation_name,
            timeout_ms,
        )
    }))
}

#[pyfunction]
#[pyo3(signature=(path, operation, request="{}", profile="default"))]
fn evaluate_read(
    py: Python<'_>,
    path: &str,
    operation: &str,
    request: &str,
    profile: &str,
) -> PyResult<String> {
    let request: serde_json::Value =
        catabolic_core::decode_json(request).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    result(py.detach(|| {
        let store = catabolic_store::Store::open(Path::new(path), false, false)?;
        let mut evaluation=catabolic_store::selection::Evaluation::new(store,profile,60000,100000,false)?;
        match operation {
            "manifest"=>catabolic_store::manifest::build(&mut evaluation,request["catalog"].as_str().unwrap_or("global"),request.get("extra"),request.get("selection")),
            "select"=>{let selected=evaluation.select(&request)?;Ok(serde_json::json!({"entity":selected.entity,"ids":selected.ids,"report":selected.report}))},
            "query"=>evaluation.run(request["id"].as_str().ok_or_else(||catabolic_store::Error("query requires id".into()))?,request["limit"].as_u64().unwrap_or(1000) as usize),
            "layout"=>catabolic_store::layout::plan(&mut evaluation,request["id"].as_str().ok_or_else(||catabolic_store::Error("layout requires id".into()))?,request["catalog"].as_str().unwrap_or("global"),request["replace_layout"]==true,request["allow_empty"]==true,request["limit"].as_u64().unwrap_or(100) as usize),
            _=>Err(catabolic_store::Error("unknown read operation".into())),
        }
    }))
}

#[pyfunction]
#[pyo3(signature=(path, operation, request="{}", profile="default"))]
fn tag_mutation(
    py: Python<'_>,
    path: &str,
    operation: &str,
    request: &str,
    profile: &str,
) -> PyResult<String> {
    let request: serde_json::Value =
        catabolic_core::decode_json(request).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    result(py.detach(|| {
        let store = catabolic_store::Store::open(Path::new(path), true, false)?;
        catabolic_store::tagging::dispatch(&store, profile, operation, &request)
    }))
}

#[pyfunction]
#[pyo3(signature=(path, surface, operation, request="{}", profile="default"))]
fn execute_operation(
    py: Python<'_>,
    path: &str,
    surface: &str,
    operation: &str,
    request: &str,
    profile: &str,
) -> PyResult<String> {
    let request: serde_json::Value =
        catabolic_core::decode_json(request).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    if (matches!(surface, "plex-import" | "plex-match" | "plex-metadata") && operation == "run")
        || surface == "plex-login"
    {
        py.check_signals()?;
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let interrupted = std::sync::Mutex::new(None);
        let outcome = py.detach(|| {
            let mut poll = || {
                if let Err(error) = Python::attach(|py| py.check_signals()) {
                    let mut first = interrupted.lock().unwrap();
                    if first.is_none() {
                        *first = Some(error);
                    }
                    cancelled.store(true, std::sync::atomic::Ordering::Release);
                }
            };
            if surface == "plex-login" {
                catabolic_runtime::api::execute_plex_login_with_poll(
                    operation, &request, &cancelled, &mut poll,
                )
            } else if surface == "plex-metadata" {
                catabolic_runtime::api::execute_plex_metadata_with_poll(
                    Path::new(path),
                    profile,
                    &request,
                    &cancelled,
                    &mut poll,
                )
            } else if surface == "plex-match" {
                catabolic_runtime::api::execute_plex_match_with_poll(
                    Path::new(path),
                    profile,
                    &request,
                    &cancelled,
                    &mut poll,
                )
            } else {
                catabolic_runtime::api::execute_plex_import_with_poll(
                    Path::new(path),
                    profile,
                    &request,
                    &cancelled,
                    &mut poll,
                )
            }
        });
        // The runtime has joined owned work and completed cancellation cleanup
        // before the original Python signal exception is propagated.
        if let Some(error) = interrupted.into_inner().unwrap() {
            return Err(error);
        }
        return result(outcome);
    }
    result(py.detach(|| {
        if surface == "import" && operation == "catalog" {
            let cancelled = std::sync::atomic::AtomicBool::new(false);
            let mut poll = || {
                if Python::attach(|py| py.check_signals()).is_err() {
                    cancelled.store(true, std::sync::atomic::Ordering::Release);
                }
            };
            return catabolic_runtime::api::execute_import_with_poll(
                Path::new(path),
                profile,
                &request,
                &cancelled,
                &mut poll,
            );
        }
        catabolic_runtime::api::execute(Path::new(path), profile, surface, operation, &request)
    }))
}

#[pyfunction]
#[pyo3(signature=(operation, request="{}"))]
fn interchange(py: Python<'_>, operation: &str, request: &str) -> PyResult<String> {
    let request: serde_json::Value =
        catabolic_core::decode_json(request).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    result(py.detach(|| catabolic_store::interchange::dispatch(operation, &request)))
}
