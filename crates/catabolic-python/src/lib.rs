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
fn encode_json(raw: &str) -> PyResult<String> {
    let value = serde_json::from_str(raw).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    Ok(catabolic_core::encode(&value))
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
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add_function(wrap_pyfunction!(encode_json, m)?)?;
    m.add_function(wrap_pyfunction!(initialize_database, m)?)?;
    m.add_function(wrap_pyfunction!(inspect_database, m)?)?;
    m.add_function(wrap_pyfunction!(upgrade_database, m)?)?;
    m.add_function(wrap_pyfunction!(execute_sql, m)?)?;
    m.add_function(wrap_pyfunction!(catalog_query, m)?)?;
    m.add_function(wrap_pyfunction!(execute_graphql, m)?)?;
    m.add_function(wrap_pyfunction!(evaluate_read, m)?)?;
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
        serde_json::from_str(options).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
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
    let variables =
        serde_json::from_str(variables).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
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
        serde_json::from_str(request).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    result(py.detach(|| {
        let store = catabolic_store::Store::open(Path::new(path), false, false)?;
        let mut evaluation=catabolic_store::selection::Evaluation::new(store,profile,60000,100000,false)?;
        match operation {
            "select"=>{let selected=evaluation.select(&request)?;Ok(serde_json::json!({"entity":selected.entity,"ids":selected.ids,"report":selected.report}))},
            "query"=>evaluation.run(request["id"].as_str().ok_or_else(||catabolic_store::Error("query requires id".into()))?,request["limit"].as_u64().unwrap_or(1000) as usize),
            "layout"=>catabolic_store::layout::plan(&mut evaluation,request["id"].as_str().ok_or_else(||catabolic_store::Error("layout requires id".into()))?,request["catalog"].as_str().unwrap_or("global"),request["replace_layout"]==true,request["allow_empty"]==true,request["limit"].as_u64().unwrap_or(100) as usize),
            _=>Err(catabolic_store::Error("unknown read operation".into())),
        }
    }))
}
