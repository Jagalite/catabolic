// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Plex PIN authorization. No catalog is opened and no token is returned by
//! the login operation boundary. Test endpoints require an explicit opt-in and
//! a numeric loopback address; ordinary callers always use Plex's fixed origin.
use catabolic_store::{Result as StoreResult, consumers::Failure};
use serde_json::{Value, json};
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

type Result<T> = std::result::Result<T, Failure>;
const MAXIMUM: usize = 65536;
pub const INTERRUPTED: &str = "Plex login interrupted";

fn checkpoint(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(error("interrupted"))
    } else {
        Ok(())
    }
}
struct Control<'a> {
    cancel: &'a AtomicBool,
    poll: &'a mut dyn FnMut(),
}
impl Control<'_> {
    fn checkpoint(&mut self) -> Result<()> {
        (self.poll)();
        checkpoint(self.cancel)
    }
}
fn uncontrolled<T>(operation: impl FnOnce(&mut Control<'_>) -> Result<T>) -> Result<T> {
    operation(&mut Control {
        cancel: &AtomicBool::new(false),
        poll: &mut || {},
    })
}
fn sync_private(file: &File, control: &mut Control<'_>) -> Result<()> {
    let result = file.sync_all();
    // Poll on the actual operation owner, rather than letting a publication
    // thread race a delayed Python signal observer before the commit point.
    control.checkpoint()?;
    result.map_err(|e| {
        if e.kind() == std::io::ErrorKind::Interrupted {
            error("interrupted")
        } else {
            storage_error(e)
        }
    })
}

fn error(code: &str) -> Failure {
    Failure::new(code)
}
fn storage_error(_: std::io::Error) -> Failure {
    error("credential_storage_unavailable")
}
fn text(value: &Value, maximum: usize) -> Result<&str> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.chars().count() <= maximum && !s.chars().any(|c| c < ' '))
        .ok_or_else(|| error("invalid_configuration"))
}
fn owner() -> u32 {
    // SAFETY: getuid takes no arguments and cannot fail.
    unsafe { libc::getuid() }
}

/// Resolve and create the private directory using the frozen final-directory
/// ownership and mode checks. The directory itself may not be a symlink.
pub fn directory(value: impl AsRef<Path>) -> Result<PathBuf> {
    let value = value.as_ref();
    let expanded = match value.to_str() {
        Some("~") => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| value.into()),
        Some(s) if s.starts_with("~/") => std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(&s[2..]))
            .unwrap_or_else(|| value.into()),
        _ => value.to_path_buf(),
    };
    let folder = std::path::absolute(expanded).map_err(storage_error)?;
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&folder)
        .map_err(storage_error)?;
    let info = fs::symlink_metadata(&folder).map_err(storage_error)?;
    if !info.is_dir() || info.uid() != owner() || info.mode() & 0o077 != 0 {
        return Err(error("unsafe_credential_storage"));
    }
    Ok(folder)
}

pub fn read_private(filename: impl AsRef<Path>) -> Result<Value> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(filename)
        .map_err(storage_error)?;
    let info = file.metadata().map_err(storage_error)?;
    if !info.is_file() || info.uid() != owner() || info.mode() & 0o077 != 0 || info.nlink() != 1 {
        return Err(error("unsafe_credential_storage"));
    }
    // Python's bound is Unicode characters, not UTF-8 bytes. Four bytes per
    // scalar bounds allocation while preserving its 16,384-character contract.
    let mut bytes = Vec::new();
    file.take(4 * 16385)
        .read_to_end(&mut bytes)
        .map_err(storage_error)?;
    let raw = std::str::from_utf8(&bytes).map_err(|_| error("credential_storage_unavailable"))?;
    if raw.chars().count() > 16384 {
        return Err(error("credential_storage_unavailable"));
    }
    let value =
        catabolic_core::decode_json(raw).map_err(|_| error("credential_storage_unavailable"))?;
    if !value.is_object() {
        return Err(error("credential_storage_unavailable"));
    }
    Ok(value)
}

/// Flush a private temporary file, atomically replace the destination and sync
/// its directory. Tempfile's RAII cleanup also covers serialization/rename errors.
pub fn write_private(filename: impl AsRef<Path>, value: &Value) -> Result<()> {
    uncontrolled(|control| write_private_with_control(filename.as_ref(), value, control))
}

fn write_private_with_control(
    filename: &Path,
    value: &Value,
    control: &mut Control<'_>,
) -> Result<()> {
    control.checkpoint()?;
    let folder = filename
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::Builder::new()
        .prefix(".plex-")
        .tempfile_in(folder)
        .map_err(storage_error)?;
    let raw = serde_json::to_vec(value).map_err(|_| error("credential_storage_unavailable"))?;
    temporary.write_all(&raw).map_err(storage_error)?;
    temporary.flush().map_err(storage_error)?;
    control.checkpoint()?;
    sync_private(temporary.as_file(), control)?;
    control.checkpoint()?;
    temporary
        .persist(filename)
        .map_err(|_| error("credential_storage_unavailable"))?;
    // The rename is the commit point. Always attempt the directory sync once
    // committed, even if cancellation arrives now; never roll back a valid
    // credential to stale PIN state. The final checkpoint reports interruption.
    sync_private(&File::open(folder).map_err(storage_error)?, control)?;
    Ok(())
}

pub fn identity(folder: impl AsRef<Path>) -> Result<String> {
    uncontrolled(|control| identity_with_control(folder.as_ref(), control))
}

fn identity_with_control(folder: &Path, control: &mut Control<'_>) -> Result<String> {
    control.checkpoint()?;
    let lock = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(folder)
        .map_err(storage_error)?;
    loop {
        control.checkpoint()?;
        // SAFETY: the descriptor belongs to `lock`, stays live for the syscall
        // and is closed on every return, releasing the advisory directory lock.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break;
        }
        let failure = std::io::Error::last_os_error();
        if !matches!(
            failure.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
        ) {
            return Err(storage_error(failure));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let filename = folder.join("client.json");
    if filename.exists() {
        let value = read_private(filename)?;
        let client = value
            .get("client_id")
            .ok_or_else(|| error("invalid_login_state"))?;
        return text(client, 128).map(str::to_owned);
    }
    let client = uuid::Uuid::new_v4().to_string();
    write_private_with_control(&filename, &json!({"client_id":client}), control)?;
    Ok(client)
}

fn network_error(failure: ureq::Error) -> Failure {
    if matches!(failure, ureq::Error::Timeout(ureq::Timeout::Global)) {
        error("plex_login_failed")
    } else {
        error("temporarily_failed")
    }
}

fn form(value: &str) -> String {
    crate::consumers::quote(value).replace("%20", "+")
}
fn positive_integer(value: &Value) -> bool {
    value.is_number() && {
        let s = value.to_string();
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && s.bytes().any(|b| b != b'0')
    }
}
// Preserve the PIN endpoint error contract around the shared wire decoder.
fn decode_response(bytes: &[u8]) -> Result<Value> {
    crate::consumers::json_wire::decode(bytes).map_err(|_| error("plex_login_failed"))
}

fn same_pin_id(returned: &Value, pin: &Value) -> bool {
    if returned == pin {
        return true;
    }
    let raw = pin.to_string();
    let Some(pin) = num_bigint::BigInt::parse_bytes(raw.as_bytes(), 10) else {
        return false;
    };
    if let Some(boolean) = returned.as_bool() {
        return pin == num_bigint::BigInt::from(u8::from(boolean));
    }
    // Unequal integer JSON values must never round through f64: Python
    // compares arbitrary integers exactly, even above 2**53.
    if !returned.is_number() || !returned.to_string().contains(['.', 'e', 'E']) {
        return false;
    }
    let Some(number) = returned.as_f64() else {
        return false;
    };
    if !number.is_finite() || number.fract() != 0.0 {
        return false;
    }
    num_bigint::BigInt::parse_bytes(format!("{number:.0}").as_bytes(), 10)
        .is_some_and(|value| value == pin)
}

fn retry_after(raw: Option<&str>) -> f64 {
    let Some(raw) = raw else { return 0.0 };
    let raw = raw.trim();
    if raw.is_empty() {
        return 0.0;
    }
    if let Ok(n) = raw.parse::<i64>() {
        return n.clamp(0, 86400) as f64;
    }
    let digits = raw.strip_prefix(['+', '-']).unwrap_or(raw);
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        return if raw.starts_with('-') { 0.0 } else { 86400.0 };
    }
    chrono::DateTime::parse_from_rfc2822(raw)
        .map(|d| (d.timestamp() as f64 - catabolic_store::notifications::now()).clamp(0.0, 86400.0))
        .unwrap_or(60.0)
}

struct Service {
    base: String,
    agent: ureq::Agent,
}
impl Service {
    fn new(base: String) -> Self {
        Self {
            base,
            agent: ureq::Agent::config_builder()
                .proxy(None)
                .max_idle_connections(0)
                .max_redirects(0)
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(15)))
                .timeout_connect(Some(Duration::from_secs(10)))
                .build()
                .into(),
        }
    }
    fn production() -> Self {
        Self::new("https://plex.tv/api/v2/".into())
    }
    fn for_request(request: &Value) -> Result<Self> {
        let Some(endpoint) = request.get("fixture_endpoint") else {
            return Ok(Self::production());
        };
        if std::env::var("CATABOLIC_PLEX_LOGIN_FIXTURE").as_deref() != Ok("1") {
            return Err(error("invalid_configuration"));
        }
        let endpoint = endpoint
            .as_str()
            .ok_or_else(|| error("invalid_configuration"))?;
        let authority = endpoint
            .strip_prefix("http://")
            .and_then(|v| v.strip_suffix('/').or(Some(v)))
            .ok_or_else(|| error("invalid_configuration"))?;
        let socket: std::net::SocketAddr = authority
            .parse()
            .map_err(|_| error("invalid_configuration"))?;
        if !socket.ip().is_loopback() || socket.port() == 0 {
            return Err(error("invalid_configuration"));
        }
        Ok(Self::new(format!("http://{socket}/api/v2/")))
    }
    fn call(
        &self,
        route: &str,
        client_id: &str,
        method: &str,
        params: &[(&str, &str)],
        secret: Option<&str>,
    ) -> Result<Value> {
        let query = params
            .iter()
            .map(|(key, value)| format!("{}={}", form(key), form(value)))
            .collect::<Vec<_>>()
            .join("&");
        let url = format!(
            "{}{route}{}",
            self.base,
            if query.is_empty() {
                String::new()
            } else {
                format!("?{query}")
            }
        );
        let mut builder = ureq::http::Request::builder()
            .method(method)
            .uri(&url)
            .header("Connection", "close")
            .header("Accept", "application/json")
            .header("X-Plex-Product", "Catabolic")
            .header("X-Plex-Client-Identifier", client_id);
        if method == "POST" {
            // PIN creation has no body. Without explicit framing, ureq sends
            // an empty chunked body that HTTP/1.0 peers may leave unread,
            // resetting the connection before the response is consumed.
            builder = builder.header("Content-Length", "0");
        }
        if let Some(secret) = secret {
            builder = builder.header("X-Plex-Token", secret);
        }
        let request = builder.body(()).map_err(|_| error("temporarily_failed"))?;
        let response = self.agent.run(request);
        let mut response = response.map_err(network_error)?;
        if !response.status().is_success() {
            let code = match response.status().as_u16() {
                401 | 403 => "unauthorized",
                429 => "rate_limited",
                404 => "unavailable",
                405 | 501 => "unsupported",
                400 | 412 | 422 => "invalid_configuration",
                _ => "temporarily_failed",
            };
            let mut failure = error(code);
            failure.retry_after = retry_after(
                response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok()),
            );
            return Err(failure);
        }
        if response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .is_some_and(|v| v > MAXIMUM as u64)
        {
            return Err(error("temporarily_failed"));
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take((MAXIMUM + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|failure| network_error(failure.into()))?;
        if bytes.len() > MAXIMUM {
            return Err(error("temporarily_failed"));
        }
        let value = decode_response(&bytes)?;
        if !value.is_object() {
            return Err(error("plex_login_failed"));
        }
        Ok(value)
    }
    fn call_with_control(
        &self,
        route: &str,
        client_id: &str,
        method: &str,
        params: &[(&str, &str)],
        secret: Option<&str>,
        control: &mut Control<'_>,
    ) -> Result<Value> {
        control.checkpoint()?;
        std::thread::scope(|scope| {
            let (send, receive) = mpsc::sync_channel(1);
            scope.spawn(move || {
                let _ = send.send(self.call(route, client_id, method, params, secret));
            });
            loop {
                // Keep joining the one bounded network request after a signal.
                // It has no access to private publication or subsequent calls.
                (control.poll)();
                match receive.recv_timeout(Duration::from_millis(10)) {
                    Ok(result) => {
                        control.checkpoint()?;
                        return result;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(error("plex_login_failed"));
                    }
                }
            }
        })
    }
    fn start(&self, storage: &Path, control: &mut Control<'_>) -> Result<Value> {
        control.checkpoint()?;
        let folder = directory(storage)?;
        let client_id = identity_with_control(&folder, control)?;
        control.checkpoint()?;
        let result = self.call_with_control(
            "pins",
            &client_id,
            "POST",
            &[("strong", "true")],
            None,
            control,
        )?;
        control.checkpoint()?;
        let ttl = result["expiresIn"]
            .as_u64()
            .filter(|n| (1..=3600).contains(n));
        if !positive_integer(&result["id"]) || ttl.is_none() {
            return Err(error("invalid_response"));
        }
        let ttl = ttl.expect("checked ttl");
        let code = text(&result["code"], 512)?;
        let identifier = uuid::Uuid::new_v4().to_string();
        write_private_with_control(
            &folder.join(format!("{identifier}.json")),
            &json!({"client_id":client_id,"pin_id":result["id"],"code":code,"expires_at":catabolic_store::notifications::now()+ttl as f64}),
            control,
        )?;
        Ok(
            json!({"login_id":identifier,"authorization_url":format!("https://app.plex.tv/auth#?clientID={}&code={}&context%5Bdevice%5D%5Bproduct%5D=Catabolic",form(&client_id),form(code)),"expires_in":ttl,"state":"awaiting_authorization","complete":false}),
        )
    }
    fn complete(
        &self,
        storage: &Path,
        identifier: &str,
        control: &mut Control<'_>,
    ) -> Result<Value> {
        control.checkpoint()?;
        if uuid::Uuid::parse_str(identifier)
            .ok()
            .is_none_or(|id| id.to_string() != identifier)
        {
            return Err(error("invalid_login_id"));
        }
        let folder = directory(storage)?;
        let filename = folder.join(format!("{identifier}.json"));
        let session = read_private(&filename)?;
        let required = |key: &str| session.get(key).ok_or_else(|| error("invalid_login_state"));
        let client_id = text(required("client_id")?, 128)?;
        if let Some(token) = session.get("token") {
            text(token, 4096)?;
        } else {
            let pin = required("pin_id")?;
            let expires = required("expires_at")?
                .as_f64()
                .ok_or_else(|| error("invalid_login_state"))?;
            if !positive_integer(pin) {
                return Err(error("invalid_login_state"));
            }
            let code = text(required("code")?, 512)?;
            if catabolic_store::notifications::now() >= expires {
                return Ok(json!({"state":"expired","complete":false,"action":"start_new_login"}));
            }
            control.checkpoint()?;
            let result = self.call_with_control(
                &format!("pins/{pin}"),
                client_id,
                "GET",
                &[("code", code)],
                None,
                control,
            )?;
            control.checkpoint()?;
            if !same_pin_id(&result["id"], pin) || result["code"] != code {
                return Err(error("invalid_response"));
            }
            if result["authToken"].is_null() {
                return Ok(
                    json!({"state":"awaiting_authorization","complete":false,"retry_after":1}),
                );
            }
            let secret = text(&result["authToken"], 4096)?;
            control.checkpoint()?;
            self.call_with_control("user", client_id, "GET", &[], Some(secret), control)?;
            control.checkpoint()?;
            write_private_with_control(
                &filename,
                &json!({"client_id":client_id,"token":secret}),
                control,
            )?;
        }
        Ok(json!({"state":"authorized","credential_file":filename,"complete":true}))
    }
}

/// Fixed-origin HTTP helper matching the frozen route/method/query boundary.
/// Login operations never expose this helper's private response to the CLI.
pub fn call(
    route: &str,
    client_id: &str,
    method: &str,
    params: &[(&str, &str)],
    secret: Option<&str>,
) -> Result<Value> {
    Service::production().call(route, client_id, method, params, secret)
}

pub fn start(storage: impl AsRef<Path>) -> Result<Value> {
    uncontrolled(|control| Service::production().start(storage.as_ref(), control))
}
pub fn complete(storage: impl AsRef<Path>, identifier: &str) -> Result<Value> {
    uncontrolled(|control| Service::production().complete(storage.as_ref(), identifier, control))
}

/// Private credential consumption, for trusted adapter callers only. Never
/// expose this pair through the CLI operation or a catalog response.
pub fn credential(reference: &str) -> Result<(String, String)> {
    let value = read_private(
        reference
            .get(5..)
            .ok_or_else(|| error("credential_storage_unavailable"))?,
    )?;
    let token = value
        .get("token")
        .ok_or_else(|| error("invalid_login_state"))?;
    let client = value
        .get("client_id")
        .ok_or_else(|| error("invalid_login_state"))?;
    Ok((text(token, 4096)?.into(), text(client, 128)?.into()))
}

/// Catalog-independent opt-in native operation boundary. Only public login
/// outcomes are routable; private state and token-consuming helpers are not.
pub fn execute(operation: &str, request: &Value) -> StoreResult<Value> {
    execute_with_poll(operation, request, &AtomicBool::new(false), &mut || {})
}

// Process-global signal ownership is restricted to the executable entrypoint.
// Embedders retain their own handlers and use execute_with_poll instead.
static CLI_SIGNAL_OWNER: std::sync::Mutex<()> = std::sync::Mutex::new(());
static CLI_INTERRUPTED: AtomicBool = AtomicBool::new(false);
static CLI_TERMINATED: AtomicBool = AtomicBool::new(false);
extern "C" fn cli_interrupted(signal: libc::c_int) {
    // Lock-free atomics are the only operations in this signal handler. TERM
    // is retained only when it is the first signal, matching the supervisor.
    if signal == libc::SIGTERM && !CLI_INTERRUPTED.load(Ordering::Acquire) {
        CLI_TERMINATED.store(true, Ordering::Release);
    }
    CLI_INTERRUPTED.store(true, Ordering::Release);
}
/// Whether the executable signal guard's first signal was TERM. The legacy
/// CLI uses this to exit by TERM after the Plex owner has finished cleanup.
pub fn cli_terminated() -> bool {
    CLI_TERMINATED.load(Ordering::Acquire)
}
struct CliSignalGuard {
    previous: Vec<(libc::c_int, libc::sigaction)>,
}
impl CliSignalGuard {
    fn install() -> StoreResult<Self> {
        let mut guard = Self {
            previous: Vec::new(),
        };
        for signal in [libc::SIGINT, libc::SIGTERM] {
            // SAFETY: sigaction is a C POD struct initialized before use; the
            // signal set is populated by sigemptyset and the handler has the
            // required C ABI and signal-safe atomic-only body.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = cli_interrupted as *const () as usize;
            // SAFETY: sa_mask is a live, writable sigset_t in `action`.
            unsafe { libc::sigemptyset(&mut action.sa_mask) };
            let mut previous = std::mem::MaybeUninit::<libc::sigaction>::uninit();
            // SAFETY: both pointers refer to valid sigaction storage and the
            // signal constants are supported catchable Unix signals.
            if unsafe { libc::sigaction(signal, &action, previous.as_mut_ptr()) } != 0 {
                return Err(catabolic_store::Error(
                    "Plex login signal handler unavailable".into(),
                ));
            }
            // SAFETY: successful sigaction initialized `previous` completely.
            guard
                .previous
                .push((signal, unsafe { previous.assume_init() }));
        }
        Ok(guard)
    }
}
impl Drop for CliSignalGuard {
    fn drop(&mut self) {
        for (signal, previous) in self.previous.iter().rev() {
            // SAFETY: these are the live saved actions returned by successful
            // sigaction calls; a null output pointer requests no old-action copy.
            unsafe { libc::sigaction(*signal, previous, std::ptr::null_mut()) };
        }
    }
}

/// Executable-only scoped signal bridge. Observe a signal synchronously in the
/// native publication checkpoints, without an asynchronous listener race.
/// Python and other hosts must use execute_with_poll to preserve their handlers.
pub fn execute_cli(operation: &str, request: &Value) -> StoreResult<Value> {
    with_cli_signals(INTERRUPTED, |cancel| {
        crate::api::execute_plex_login_with_poll(operation, request, cancel, &mut || {})
    })
}
/// Shared only by executable Plex owners. Embedded calls must retain their
/// host's signal handlers and pass per-call cancellation/polling instead.
pub(crate) fn with_cli_signals(
    interrupted: &str,
    operation: impl FnOnce(&AtomicBool) -> StoreResult<Value>,
) -> StoreResult<Value> {
    let _owner = CLI_SIGNAL_OWNER
        .lock()
        .map_err(|_| catabolic_store::Error("Plex login signal owner unavailable".into()))?;
    CLI_INTERRUPTED.store(false, Ordering::Release);
    CLI_TERMINATED.store(false, Ordering::Release);
    let _signals = CliSignalGuard::install()?;
    let result = operation(&CLI_INTERRUPTED);
    checkpoint(&CLI_INTERRUPTED).map_err(|_| catabolic_store::Error(interrupted.into()))?;
    result
}

/// Poll signals on the operation owner, including immediately before private
/// publication. Only HTTP uses a joined, bounded worker; it cannot write files
/// or issue the next request after the owner observes cancellation.
pub fn execute_with_poll(
    operation: &str,
    request: &Value,
    cancel: &AtomicBool,
    poll: &mut dyn FnMut(),
) -> StoreResult<Value> {
    let mut control = Control { cancel, poll };
    execute_inner(operation, request, &mut control).map_err(|failure| {
        if failure.code == "interrupted" {
            catabolic_store::Error(INTERRUPTED.into())
        } else {
            failure.error()
        }
    })
}

fn execute_inner(operation: &str, request: &Value, control: &mut Control<'_>) -> Result<Value> {
    control.checkpoint()?;
    let service = Service::for_request(request)?;
    let storage = request
        .get("credential_dir")
        .or_else(|| request.get("storage"))
        .map(|v| text(v, usize::MAX))
        .transpose()?
        .unwrap_or("~/.config/catabolic/credentials");
    match operation {
        "start" => service.start(Path::new(storage), control),
        "complete" => service.complete(
            Path::new(storage),
            request["identifier"]
                .as_str()
                .or_else(|| request["login_id"].as_str())
                .ok_or_else(|| error("invalid_login_id"))?,
            control,
        ),
        _ => Err(error("invalid_configuration")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn success<T>(result: Result<T>) -> T {
        result.unwrap_or_else(|error| panic!("{}", error.code))
    }

    #[test]
    fn private_helpers_and_reference_contract() {
        let temp = tempfile::tempdir().unwrap();
        let folder = success(directory(temp.path().join("nested/credentials")));
        let client = success(identity(&folder));
        assert_eq!(client, success(identity(&folder)));
        let filename = folder.join("credential.json");
        success(write_private(
            &filename,
            &json!({"client_id":client,"token":"SYNTHETIC_ONLY"}),
        ));
        assert_eq!(
            success(credential(&format!("file:{}", filename.display()))),
            ("SYNTHETIC_ONLY".into(), client)
        );
        assert_eq!(
            fs::metadata(&filename).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(folder).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn private_character_bound_and_missing_keys() {
        let temp = tempfile::tempdir().unwrap();
        let filename = temp.path().join("value.json");
        let value = json!({"value":"é".repeat(9000)});
        success(write_private(&filename, &value));
        assert_eq!(success(read_private(&filename)), value);
        assert_eq!(
            credential(&format!("file:{}", filename.display()))
                .err()
                .unwrap()
                .code,
            "invalid_login_state"
        );
        success(write_private(
            &filename,
            &json!({"value":"é".repeat(16384)}),
        ));
        assert_eq!(
            read_private(&filename).err().unwrap().code,
            "credential_storage_unavailable"
        );
    }

    #[test]
    fn private_failed_rename_cleans_temporary_file() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("blocked");
        fs::create_dir(&destination).unwrap();
        assert_eq!(
            write_private(&destination, &json!({"token":"SYNTHETIC_ONLY"}))
                .err()
                .unwrap()
                .code,
            "credential_storage_unavailable"
        );
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn retry_after_and_arbitrary_pin_bounds() {
        assert_eq!(retry_after(Some("120")), 120.0);
        assert_eq!(retry_after(Some("invalid")), 60.0);
        assert_eq!(retry_after(Some("-42")), 0.0);
        assert_eq!(retry_after(Some("999999999999999999999999999")), 86400.0);
        assert!(positive_integer(
            &serde_json::from_str("123456789012345678901234567890").unwrap()
        ));
        for value in [json!(true), json!(1.0), json!(0), json!(-1), Value::Null] {
            assert!(!positive_integer(&value));
        }
        assert_eq!(form("a b+/é"), "a+b%2B%2F%C3%A9");
    }

    #[test]
    fn executable_signal_guard_restores_prior_handlers() {
        fn current(signal: libc::c_int) -> libc::sigaction {
            let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
            // SAFETY: a null new-action pointer queries the valid catchable
            // signal into live writable sigaction storage.
            let result = unsafe { libc::sigaction(signal, std::ptr::null(), action.as_mut_ptr()) };
            assert_eq!(result, 0);
            // SAFETY: the successful query initialized the output structure.
            unsafe { action.assume_init() }
        }
        let _owner = CLI_SIGNAL_OWNER.lock().unwrap();
        let before = [current(libc::SIGINT), current(libc::SIGTERM)];
        let guard = CliSignalGuard::install().unwrap();
        for signal in [libc::SIGINT, libc::SIGTERM] {
            assert_eq!(
                current(signal).sa_sigaction,
                cli_interrupted as *const () as usize
            );
        }
        drop(guard);
        for (index, signal) in [libc::SIGINT, libc::SIGTERM].into_iter().enumerate() {
            let after = current(signal);
            assert_eq!(after.sa_sigaction, before[index].sa_sigaction);
            // libc may add kernel trampoline housekeeping (SA_RESTORER) to
            // an otherwise identical saved action. Compare public semantics.
            let meaningful = libc::SA_ONSTACK
                | libc::SA_RESTART
                | libc::SA_SIGINFO
                | libc::SA_NODEFER
                | libc::SA_RESETHAND;
            assert_eq!(
                after.sa_flags & meaningful,
                before[index].sa_flags & meaningful
            );
            for member in 1..=64 {
                // SAFETY: both masks came from successful sigaction queries;
                // sigismember handles out-of-platform-range values with -1.
                let actual = unsafe { libc::sigismember(&after.sa_mask, member) };
                // SAFETY: the saved mask is initialized sigaction output.
                let expected = unsafe { libc::sigismember(&before[index].sa_mask, member) };
                assert_eq!(actual, expected);
            }
        }
    }

    #[test]
    fn response_numeric_equality_never_rounds_distinct_integers() {
        assert!(same_pin_id(&json!(123.0), &json!(123)));
        assert!(same_pin_id(&json!(true), &json!(1)));
        assert!(!same_pin_id(&json!(true), &json!(123)));
        assert!(!same_pin_id(
            &json!(9007199254740993u64),
            &json!(9007199254740992u64)
        ));
        assert!(!same_pin_id(
            &json!(9007199254740992.0),
            &json!(9007199254740993u64)
        ));
        assert!(same_pin_id(
            &json!(9007199254740992.0),
            &json!(9007199254740992u64)
        ));
    }

    #[test]
    fn pre_cancelled_login_has_no_files_or_requests() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("credentials");
        let cancelled = AtomicBool::new(false);
        let result = execute_with_poll(
            "start",
            &json!({"credential_dir":folder}),
            &cancelled,
            &mut || {
                cancelled.store(true, Ordering::Release);
            },
        );
        assert_eq!(result.unwrap_err().0, INTERRUPTED);
        assert!(!folder.exists());
    }

    #[test]
    fn network_timeout_category_matches_frozen_login() {
        let failure = network_error(ureq::Error::Timeout(ureq::Timeout::Global));
        assert_eq!(failure.code, "plex_login_failed");
        assert!(!failure.retryable());
    }

    #[test]
    fn file_descriptor_lifetime_on_repeated_storage_errors() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state");
        fs::write(&path, b"invalid").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        #[cfg(target_os = "linux")]
        let before = fs::read_dir("/proc/self/fd").unwrap().count();
        for _ in 0..256 {
            assert!(read_private(&path).is_err());
        }
        #[cfg(target_os = "linux")]
        assert!(fs::read_dir("/proc/self/fd").unwrap().count() <= before + 3);
    }
}
