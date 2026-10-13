// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Disposable-fixture-only in-process ingress. This is not production admission.
//! All HTTP behavior remains in the same native request and streaming owners.
use crate::{ServerConfig, server};
use axum::{
    body::{Body, Bytes, HttpBody},
    extract::State,
    http::{HeaderName, HeaderValue, Request},
};
use catabolic_store::{Error, Result};
use http_body_util::BodyExt;
use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
};
use tokio::{
    runtime::Runtime,
    sync::{mpsc, watch},
};

pub const MAX_CHUNK: usize = 65536;

/// The only in-process admission capability. Its path cannot be supplied by a
/// caller: creation owns a new temporary directory and initializes a new catalog.
pub struct DisposableCatalog {
    _root: tempfile::TempDir,
    database: PathBuf,
    database_id: String,
}
impl DisposableCatalog {
    pub fn new() -> Result<Arc<Self>> {
        let root = tempfile::Builder::new()
            .prefix("catabolic-asgi-fixture-")
            .tempdir()?;
        let database = root.path().join("catalog.db");
        let initialized = catabolic_store::migration::initialize(&database)?;
        let database_id = initialized["database_id"].as_str().unwrap().to_owned();
        Ok(Arc::new(Self {
            _root: root,
            database,
            database_id,
        }))
    }
    fn validate(&self) -> Result<()> {
        let store = catabolic_store::Store::open(&self.database, false, false)?;
        if store.database_id != self.database_id {
            return Err(Error(
                "disposable HTTP fixture catalog identity changed".into(),
            ));
        }
        Ok(())
    }
    pub fn path(&self) -> &Path {
        &self.database
    }
}

pub enum Event {
    Start(u16, Vec<(Vec<u8>, Vec<u8>)>),
    Body(Vec<u8>),
    End,
    Error(String),
}

pub struct FixtureApp {
    app: Mutex<Option<Arc<server::App>>>,
    runtime: Mutex<Option<Runtime>>,
    stop: watch::Sender<bool>,
    active: Arc<AtomicUsize>,
}
type Notify = Arc<dyn Fn() + Send + Sync>;
#[derive(Default)]
struct Wake(Mutex<Option<Notify>>);
impl Wake {
    fn notify(&self) {
        let callback = self.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(callback) = callback {
            callback();
        }
    }
    fn install(&self, callback: Notify) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(callback);
        // Registration can race the first ready event. Wake once after storing
        // the callback so readiness that preceded registration is not lost.
        self.notify();
    }
}
struct Active {
    count: Arc<AtomicUsize>,
    finished: Arc<AtomicBool>,
    wake: Arc<Wake>,
}
impl Drop for Active {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::SeqCst);
        self.finished.store(true, Ordering::Release);
        self.wake.notify();
    }
}

impl FixtureApp {
    pub fn new(fixture: Arc<DisposableCatalog>, config: ServerConfig) -> Result<Self> {
        fixture.validate()?;
        let path = fixture.path().to_owned();
        let app = server::application(&path, config, Some(fixture))?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(32)
            .enable_all()
            .build()
            .map_err(|e| Error(e.to_string()))?;
        let (stop, _) = watch::channel(false);
        Ok(Self {
            app: Mutex::new(Some(app)),
            runtime: Mutex::new(Some(runtime)),
            stop,
            active: Arc::default(),
        })
    }

    /// Refuse untrusted/missing peers and proxy indications before creating a
    /// request, reading its body, consuming limits, or dispatching any owner.
    pub fn begin(
        &self,
        peer: &str,
        method: &str,
        uri: &str,
        headers: Vec<(Vec<u8>, Vec<u8>)>,
    ) -> Result<Exchange> {
        if !peer.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) {
            return Err(Error(
                "experimental ASGI requires an explicit loopback client peer".into(),
            ));
        }
        let mut request = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            let name = HeaderName::from_bytes(&name)
                .map_err(|_| Error("invalid ASGI header name".into()))?;
            if name.as_str() == "forwarded"
                || name.as_str().starts_with("x-forwarded-")
                || matches!(name.as_str(), "x-real-ip" | "via")
            {
                return Err(Error(
                    "experimental ASGI refuses proxy forwarding headers".into(),
                ));
            }
            let value = HeaderValue::from_bytes(&value)
                .map_err(|_| Error("invalid ASGI header value".into()))?;
            request = request.header(name, value);
        }
        let (upload, incoming) = mpsc::channel::<std::result::Result<Bytes, std::io::Error>>(1);
        let wake = Arc::new(Wake::default());
        let body = futures::stream::unfold(
            (incoming, wake.clone()),
            |(mut incoming, wake)| async move {
                let chunk = incoming.recv().await;
                // Releasing an ingress slot must wake the async producer without
                // requiring a Python executor thread that egress could occupy.
                wake.notify();
                chunk.map(|chunk| (chunk, (incoming, wake)))
            },
        );
        let request = request
            .body(Body::from_stream(body))
            .map_err(|_| Error("invalid ASGI request".into()))?;
        let head = method == "HEAD";
        let (output, outgoing) = mpsc::channel(1);
        let (cancel, mut cancelled) = watch::channel(false);
        let mut stopped = self.stop.subscribe();
        let state = self
            .app
            .lock()
            .map_err(|_| Error("ASGI app lock poisoned".into()))?
            .clone()
            .ok_or_else(|| Error("ASGI application is closed".into()))?;
        let runtime = self
            .runtime
            .lock()
            .map_err(|_| Error("ASGI app lock poisoned".into()))?;
        let runtime = runtime
            .as_ref()
            .ok_or_else(|| Error("ASGI application is closed".into()))?;
        if self.is_closed() {
            return Err(Error("ASGI application is closed".into()));
        }
        self.active.fetch_add(1, Ordering::SeqCst);
        let finished = Arc::new(AtomicBool::new(false));
        let active = Active {
            count: self.active.clone(),
            finished: finished.clone(),
            wake: wake.clone(),
        };
        let notify = wake.clone();
        let task = runtime.spawn(async move {
            let _active = active;
            tokio::select! {
                biased;
                _ = cancelled.changed() => {},
                _ = stopped.changed() => {},
                _ = async move {
                    let fixture = state._fixture.as_ref().unwrap().clone();
                    match tokio::task::spawn_blocking(move || fixture.validate()).await {
                        Ok(Ok(())) => {},
                        _ => {
                            let _ = output.send(Event::Error("disposable HTTP fixture catalog identity changed or unavailable".into())).await;
                            return;
                        },
                    }
                    let response = server::handle(State(state), request).await;
                    let (mut parts, mut body) = response.into_parts();
                    // Hyper supplies this for exact-size bodies on the socket
                    // transport. In ASGI that representation is our job.
                    if !head && !parts.status.is_informational()
                        && !matches!(parts.status.as_u16(), 204 | 304)
                        && !parts.headers.contains_key("content-length")
                        && let Some(length) = body.size_hint().exact() {
                        parts.headers.insert("content-length", HeaderValue::from_str(&length.to_string()).unwrap());
                    }
                    let headers = parts.headers.iter().map(|(k,v)| (k.as_str().as_bytes().to_vec(), v.as_bytes().to_vec())).collect();
                    if output.send(Event::Start(parts.status.as_u16(), headers)).await.is_err() { return; }
                    notify.notify();
                    loop {
                        // Reserve before polling the owner's next chunk: the
                        // bridge adds only one bounded delivery slot.
                        let Ok(permit) = output.reserve().await else { return; };
                        match body.frame().await {
                            Some(Ok(frame)) => if let Ok(data) = frame.into_data() {
                                // The owners use <=64 KiB stream chunks. Ordinary
                                // JSON may be larger, so split that representation.
                                let mut chunks = data.chunks(MAX_CHUNK);
                                if let Some(chunk) = chunks.next() { permit.send(Event::Body(chunk.to_vec())); notify.notify(); }
                                for chunk in chunks {
                                    if output.send(Event::Body(chunk.to_vec())).await.is_err() { return; }
                                    notify.notify();
                                }
                            },
                            Some(Err(error)) => { permit.send(Event::Error(error.to_string())); return; },
                            None => { permit.send(Event::End); return; },
                        }
                    }
                } => {},
            }
        });
        Ok(Exchange {
            upload: Mutex::new(Some(upload)),
            outgoing: Mutex::new(outgoing),
            cancel,
            task: task.abort_handle(),
            done: AtomicBool::new(false),
            finished,
            wake,
        })
    }
    pub fn active(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }
    pub fn is_closed(&self) -> bool {
        *self.stop.borrow()
    }
    pub fn close(&self) {
        self.stop.send_replace(true);
        let runtime = self
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(runtime) = runtime {
            runtime.shutdown_timeout(std::time::Duration::from_secs(5));
        }
        self.app.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}
impl Drop for FixtureApp {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        if let Some(runtime) = self
            .runtime
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            runtime.shutdown_background();
        }
    }
}

pub struct Exchange {
    upload: Mutex<Option<mpsc::Sender<std::result::Result<Bytes, std::io::Error>>>>,
    outgoing: Mutex<mpsc::Receiver<Event>>,
    cancel: watch::Sender<bool>,
    task: tokio::task::AbortHandle,
    done: AtomicBool,
    finished: Arc<AtomicBool>,
    wake: Arc<Wake>,
}
impl Exchange {
    /// None means backpressure; no operation here waits for channel capacity.
    pub fn try_feed(&self, chunk: &[u8], more: bool) -> Result<Option<bool>> {
        if chunk.len() > MAX_CHUNK {
            return Err(Error("ASGI ingress chunk exceeds 64 KiB".into()));
        }
        let sender = self
            .upload
            .lock()
            .map_err(|_| Error("ASGI upload lock poisoned".into()))?
            .clone();
        let Some(sender) = sender else {
            return Ok(Some(false));
        };
        let accepted = if chunk.is_empty() {
            true
        } else {
            match sender.try_send(Ok(Bytes::copy_from_slice(chunk))) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => return Ok(None),
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            }
        };
        if !more || !accepted {
            self.upload.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
        Ok(Some(accepted))
    }
    pub fn poll(&self) -> Poll<Option<Event>> {
        match self
            .outgoing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_recv()
        {
            Ok(event) => Poll::Ready(Some(event)),
            Err(mpsc::error::TryRecvError::Empty) => Poll::Pending,
            Err(mpsc::error::TryRecvError::Disconnected) => Poll::Ready(None),
        }
    }
    pub fn set_notify(&self, notify: Arc<dyn Fn() + Send + Sync>) {
        self.wake.install(notify);
    }
    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
    pub fn cancel(&self) {
        if !self.done.swap(true, Ordering::SeqCst) {
            self.cancel.send_replace(true);
            self.task.abort();
            self.upload.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
    }
}
impl Drop for Exchange {
    fn drop(&mut self) {
        self.cancel();
    }
}
