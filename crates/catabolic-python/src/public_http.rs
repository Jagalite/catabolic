// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! ASGI callback/bytes representation only. HTTP behavior belongs to catabolic-http.
use crate::embedding::CatabolicError;
use catabolic_http::{
    ServerConfig,
    asgi::{DisposableCatalog, Event, Exchange, FixtureApp},
};
use pyo3::{
    prelude::*,
    types::{PyBytes, PyDict, PyList},
};
use std::{
    sync::{Arc, Mutex},
    task::Poll,
};
fn error(error: catabolic_store::Error) -> PyErr {
    CatabolicError::new_err(error.to_string())
}

#[pyclass(module = "catabolic_native._native", frozen)]
struct DisposableHTTPFixture {
    catalog: Mutex<Option<Arc<DisposableCatalog>>>,
}
#[pymethods]
impl DisposableHTTPFixture {
    #[new]
    fn new(py: Python<'_>) -> PyResult<Self> {
        Ok(Self {
            catalog: Mutex::new(Some(py.detach(DisposableCatalog::new).map_err(error)?)),
        })
    }
    #[getter]
    fn path(&self) -> PyResult<String> {
        Ok(self.catalog()?.path().to_string_lossy().into_owned())
    }
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            let mut catalog = self.catalog.lock().unwrap_or_else(|e| e.into_inner());
            if catalog.as_ref().is_some_and(|v| Arc::strong_count(v) > 1) {
                return Err(CatabolicError::new_err(
                    "close all ASGI applications before closing their disposable fixture",
                ));
            }
            catalog.take();
            Ok(())
        })
    }
}
impl DisposableHTTPFixture {
    fn catalog(&self) -> PyResult<Arc<DisposableCatalog>> {
        self.catalog
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| CatabolicError::new_err("disposable HTTP fixture is closed"))
    }
}

#[pyclass(module = "catabolic_native._native", frozen)]
struct NativeASGI {
    app: FixtureApp,
}
#[pymethods]
impl NativeASGI {
    #[new]
    #[pyo3(signature=(fixture, read_only=true, allowed_hosts=None, origins=None, streams=8, per_principal=2, rate=120))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        py: Python<'_>,
        fixture: &DisposableHTTPFixture,
        read_only: bool,
        allowed_hosts: Option<Vec<String>>,
        origins: Option<Vec<String>>,
        streams: usize,
        per_principal: usize,
        rate: usize,
    ) -> PyResult<Self> {
        let fixture = fixture.catalog()?;
        let defaults = ServerConfig::default();
        let config = ServerConfig {
            read_only,
            allowed_hosts: allowed_hosts
                .filter(|v| !v.is_empty())
                .unwrap_or(defaults.allowed_hosts.clone()),
            origins: origins.unwrap_or_default(),
            streams,
            per_principal,
            rate,
            ..defaults
        };
        Ok(Self {
            app: py
                .detach(|| FixtureApp::new(fixture, config))
                .map_err(error)?,
        })
    }
    fn begin(
        &self,
        py: Python<'_>,
        peer: &str,
        method: &str,
        uri: &str,
        headers: Vec<(Vec<u8>, Vec<u8>)>,
    ) -> PyResult<NativeASGIExchange> {
        Ok(NativeASGIExchange {
            exchange: py
                .detach(|| self.app.begin(peer, method, uri, headers))
                .map_err(error)?,
        })
    }
    fn close(&self, py: Python<'_>) {
        py.detach(|| self.app.close());
    }
    #[getter]
    fn closed(&self) -> bool {
        self.app.is_closed()
    }
    #[getter]
    fn active(&self) -> usize {
        self.app.active()
    }
}

#[pyclass(module = "catabolic_native._native", frozen)]
struct NativeASGIExchange {
    exchange: Exchange,
}
#[pymethods]
impl NativeASGIExchange {
    fn try_feed(&self, body: &[u8], more: bool) -> PyResult<Option<bool>> {
        self.exchange.try_feed(body, more).map_err(error)
    }
    fn poll<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        let result = PyDict::new(py);
        let event = match self.exchange.poll() {
            Poll::Pending => return Ok(None),
            Poll::Ready(None) => {
                result.set_item("type", "native.closed")?;
                return Ok(Some(result));
            }
            Poll::Ready(Some(event)) => event,
        };
        match event {
            Event::Start(status, headers) => {
                result.set_item("type", "http.response.start")?;
                result.set_item("status", status)?;
                let items = PyList::empty(py);
                for (key, value) in headers {
                    items.append((PyBytes::new(py, &key), PyBytes::new(py, &value)))?;
                }
                result.set_item("headers", items)?;
            }
            Event::Body(body) => {
                result.set_item("type", "http.response.body")?;
                result.set_item("body", PyBytes::new(py, &body))?;
                result.set_item("more_body", true)?;
            }
            Event::End => {
                result.set_item("type", "http.response.body")?;
                result.set_item("body", PyBytes::new(py, b""))?;
                result.set_item("more_body", false)?;
            }
            Event::Error(message) => return Err(CatabolicError::new_err(message)),
        }
        Ok(Some(result))
    }
    fn set_notify(&self, callback: Py<PyAny>) {
        self.exchange.set_notify(Arc::new(move || {
            Python::attach(|py| {
                // The private callback only schedules asyncio readiness. A closed
                // loop cannot receive a wake; cancellation/drop still owns cleanup.
                let _ = callback.call0(py);
            })
        }));
    }
    #[getter]
    fn finished(&self) -> bool {
        self.exchange.finished()
    }
    fn cancel(&self) {
        self.exchange.cancel();
    }
}
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<DisposableHTTPFixture>()?;
    module.add_class::<NativeASGI>()?;
    module.add_class::<NativeASGIExchange>()?;
    Ok(())
}
