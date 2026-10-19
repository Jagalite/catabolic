// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Bounded external receipt-v1 dispatch. HTTP runs outside the catalog writer
//! lock and every returned result is fenced again before durable acceptance.
use catabolic_store::{Error, Result, Store, processor_mutation as storage};
use serde_json::{Value, json};
use std::{io::Read, time::Duration};
const MAX_RESPONSE: usize = 1024 * 1024;
pub const WORKER_ARGUMENT: &str = "__processor-http-worker";
/// The frozen worker's diagnostic for every urllib transport, protocol,
/// header-encoding and body-read failure.
const NETWORK_ERROR: &str = "HTTP request failed; check endpoint, credentials and network";

/// Frozen `token()`: resolved in the dispatching process before the request.
fn credential(request: &Value) -> Result<Option<String>> {
    let Some(environment) = request["credential_env"].as_str() else {
        return Ok(None);
    };
    catabolic_store::notifications::credential_reference(environment)?;
    let secret = std::env::var(environment)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error(format!("set credential environment variable {environment}")))?;
    if secret.contains(['\n', '\r']) {
        return Err(Error("invalid credential value".into()));
    }
    Ok(Some(secret))
}

/// The frozen worker envelope's exact canonical UTF-8 size bounds the request,
/// including the base64 body and the resolved Authorization header.
fn request_budget(request: &Value, secret: Option<&str>) -> Result<()> {
    use base64::Engine;
    let mut headers = json!({"Content-Type": "application/json"});
    if let Some(secret) = secret {
        headers["Authorization"] = json!(format!("Bearer {secret}"));
    }
    let body = request
        .get("body")
        .filter(|v| !v.is_null())
        .map(|body| base64::engine::general_purpose::STANDARD.encode(catabolic_core::encode(body)));
    let envelope = json!({
        "url": request["url"], "headers": headers, "method": request["method"],
        "maximum": MAX_RESPONSE, "timeout": request["timeout"], "body": body,
        "structured_errors": false,
    });
    if catabolic_core::encode(&envelope).len() > 256 * 1024 {
        return Err(Error("HTTP request exceeds input limit".into()));
    }
    Ok(())
}

/// Private native transport helper. The existing process supervisor bounds DNS,
/// TLS, body reads and cancellation, and reaps this worker before returning.
pub fn worker_main() {
    use base64::Engine;
    use std::io::Write;
    let result = (|| -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        std::io::stdin().take(262145).read_to_end(&mut bytes)?;
        if bytes.len() > 262144 {
            return Err(Error("HTTP request exceeds input limit".into()));
        }
        HttpTransport.request(&catabolic_store::filesystem_mutation::admitted_json_bytes(
            &bytes,
        )?)
    })();
    let result = match result {
        Ok(bytes) => json!({"bytes":base64::engine::general_purpose::STANDARD.encode(bytes)}),
        Err(error) => json!({"error":error.0}),
    };
    let _ = writeln!(std::io::stdout(), "{}", catabolic_core::encode(&result));
}

pub struct SupervisedTransport<'a> {
    pub executable: &'a std::path::Path,
    pub cancel: &'a std::sync::atomic::AtomicBool,
}
impl Transport for SupervisedTransport<'_> {
    fn request(&mut self, request: &Value) -> Result<Vec<u8>> {
        use base64::Engine;
        use std::{ffi::OsStr, sync::atomic::Ordering};
        // The frozen dispatcher resolves the credential and bounds the exact
        // worker envelope before starting its helper; the native helper
        // repeats both checks and still receives only the variable name.
        let secret = credential(request)?;
        request_budget(request, secret.as_deref())?;
        drop(secret);
        let input = catabolic_core::encode(request);
        if input.len() > 262144 {
            return Err(Error("HTTP request exceeds input limit".into()));
        }
        let timeout = request["timeout"]
            .as_u64()
            .filter(|v| (1..=60).contains(v))
            .ok_or_else(|| Error("HTTP timeout must be 1..60 seconds".into()))?;
        let result = crate::process::command_output(
            &[self.executable.as_os_str(), OsStr::new(WORKER_ARGUMENT)],
            crate::process::CommandOptions {
                input_bytes: Some(input.as_bytes()),
                cancel: Some(self.cancel),
                timeout: Duration::from_secs(timeout),
                maximum: 2 * MAX_RESPONSE,
                ..Default::default()
            },
        );
        if self.cancel.load(Ordering::Acquire)
            || result.as_ref().is_err_and(|e| e.state == "cancelled")
        {
            return Err(Error("native legacy CLI interrupted".into()));
        }
        let failed = || Error("HTTP worker failed; check endpoint and network".into());
        if result.as_ref().is_err_and(|error| error.state == "timeout") {
            return Err(Error("HTTP request exceeded the elapsed-time limit".into()));
        }
        let result = result.map_err(|_| failed())?;
        if result.returncode != 0 || !result.stderr.is_empty() {
            return Err(failed());
        }
        let result = catabolic_core::decode_json_bytes(&result.stdout).map_err(|_| failed())?;
        if let Some(error) = result["error"].as_str() {
            return Err(Error(error.to_owned()));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(result["bytes"].as_str().ok_or_else(failed)?)
            .map_err(|_| failed())?;
        if bytes.len() > MAX_RESPONSE {
            return Err(Error("HTTP response exceeds byte limit".into()));
        }
        Ok(bytes)
    }
}

pub trait Transport {
    fn request(&mut self, request: &Value) -> Result<Vec<u8>>;
}
#[derive(Default)]
pub struct HttpTransport;
impl Transport for HttpTransport {
    fn request(&mut self, request: &Value) -> Result<Vec<u8>> {
        let timeout = request["timeout"]
            .as_u64()
            .filter(|n| (1..=60).contains(n))
            .ok_or_else(|| Error("HTTP timeout must be 1..60 seconds".into()))?;
        let url = request["url"]
            .as_str()
            .ok_or_else(|| Error("invalid processor endpoint".into()))?;
        catabolic_store::notifications::callback_origin(url)
            .map_err(|_| Error("invalid processor endpoint".into()))?;
        let secret = credential(request)?;
        request_budget(request, secret.as_deref())?;
        let network = || Error(NETWORK_ERROR.into());
        let mut headers = vec![("Content-Type", b"application/json".to_vec())];
        if let Some(secret) = secret {
            // http.client encodes header values as Latin-1; a wider character
            // fails before any connection, like its UnicodeEncodeError. The
            // native HTTP client accepts only visible ASCII in Authorization,
            // so Latin-1 bytes above 0x7f still fail with the same diagnostic
            // (a recorded difference: frozen Python sends them).
            let mut value = b"Bearer ".to_vec();
            for character in secret.chars() {
                value.push(u8::try_from(u32::from(character)).map_err(|_| network())?);
            }
            headers.push(("Authorization", value));
        }
        let body = request
            .get("body")
            .filter(|v| !v.is_null())
            .map(catabolic_core::encode);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(timeout)))
            .build()
            .into();
        let mut response = match request["method"].as_str() {
            Some("PUT") => {
                let mut outgoing = agent.put(url);
                for (key, value) in &headers {
                    outgoing = outgoing.header(*key, value.as_slice());
                }
                outgoing
                    .send(body.as_deref().unwrap_or("").as_bytes())
                    .map_err(|_| network())?
            }
            Some("GET") => {
                let mut outgoing = agent.get(url);
                for (key, value) in &headers {
                    outgoing = outgoing.header(*key, value.as_slice());
                }
                outgoing.call().map_err(|_| network())?
            }
            _ => return Err(Error("invalid processor HTTP method".into())),
        };
        // The transport still refuses every redirect. Match the frozen worker's
        // presentation only when its redirect handler would receive this normal
        // Location/URI reference; other HTTP statuses keep their status error.
        if [301, 302, 303, 307, 308].contains(&response.status().as_u16())
            && response
                .headers()
                .get("Location")
                .or_else(|| response.headers().get("URI"))
                .and_then(|value| value.to_str().ok())
                .is_some_and(redirect_handler_reference)
        {
            return Err(Error(
                "redirect refused; configure the final endpoint explicitly".into(),
            ));
        }
        if !response.status().is_success() {
            return Err(Error(format!(
                "HTTP request failed with status {}",
                response.status().as_u16()
            )));
        }
        // Like the frozen `read(maximum + 1)`, an advertised length alone
        // decides nothing: at most one byte past the limit is read. http.client
        // returns a length-delimited body cut short by EOF as-is; only chunked
        // framing raises IncompleteRead, an ordinary transport failure.
        let chunked = response.headers().contains_key("Transfer-Encoding");
        let mut bytes = Vec::new();
        match response
            .body_mut()
            .as_reader()
            .take(MAX_RESPONSE as u64 + 1)
            .read_to_end(&mut bytes)
        {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof && !chunked => (),
            Err(_) => return Err(network()),
        }
        if bytes.len() > MAX_RESPONSE {
            return Err(Error("HTTP response exceeds byte limit".into()));
        }
        Ok(bytes)
    }
}
// This chooses a diagnostic, never an endpoint to contact. urllib's frozen
// redirect handler accepts relative references and HTTP(S)/FTP schemes, and
// reports other schemes as ordinary HTTP status failures before NoRedirect.
fn redirect_handler_reference(location: &str) -> bool {
    let location = location.trim_start_matches(|c: char| c <= ' ');
    let Some((scheme, _)) = location.split_once(':') else {
        return true;
    };
    let has_scheme = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'));
    !has_scheme
        || ["http", "https", "ftp"]
            .iter()
            .any(|expected| scheme.eq_ignore_ascii_case(expected))
}

pub fn dispatch_with_transport(
    store: &mut Store,
    profile: &str,
    id: &str,
    token: &str,
    transport: &mut impl Transport,
) -> Result<Value> {
    let row = storage::fence(store, profile, id, token)?;
    let config = storage::get(store, profile, row["processor"].as_str().unwrap())?;
    let payload = &row["request"];
    if row["state"] == "leased" {
        let captured = crate::receipts::source_receipt(
            store,
            profile,
            payload["source"]["file_id"]
                .as_str()
                .ok_or_else(|| Error("invalid processor source".into()))?,
            payload["source"]["item_id"]
                .as_str()
                .ok_or_else(|| Error("invalid processor source".into()))?,
        )?;
        if captured["source"] != payload["source"] {
            return Err(Error(
                "processor source changed since enqueue; enqueue a new revision".into(),
            ));
        }
    }
    let mut body = payload.clone();
    for (key, value) in [
        ("job_id", json!(id)),
        (
            "attempt",
            json!(row["generation"].as_i64().unwrap().to_string()),
        ),
        ("producer", row["processor"].clone()),
        ("instance", row["worker"].clone()),
    ] {
        body[key] = value;
    }
    let request = json!({"url":format!("{}/jobs/{}/{}",config["endpoint"].as_str().unwrap(),id,row["generation"]),"method":if row["state"]=="leased"{"PUT"}else{"GET"},"body":if row["state"]=="leased"{body}else{Value::Null},"timeout":config["timeout"],"credential_env":config["credential_env"]});
    let raw = store.detached(|| transport.request(&request))?;
    if raw.len() > MAX_RESPONSE {
        return Err(Error("HTTP response exceeds byte limit".into()));
    }
    let response = catabolic_core::decode_json_bytes(&raw)
        .map_err(|_| Error("processor returned invalid JSON".into()))?;
    if !response.is_object()
        || !matches!(
            response["state"].as_str(),
            Some("queued" | "running" | "complete" | "failed")
        )
    {
        return Err(Error("processor returned an invalid job state".into()));
    }
    if response["state"] == "complete" {
        storage::complete(store, profile, id, token, &response["receipt"])
    } else {
        storage::transition(
            store,
            profile,
            id,
            token,
            if response["state"] == "failed" {
                "failed"
            } else {
                "submitted"
            },
        )
    }
}
pub fn dispatch(store: &mut Store, profile: &str, id: &str, token: &str) -> Result<Value> {
    dispatch_with_transport(store, profile, id, token, &mut HttpTransport)
}
pub fn tick(
    store: &mut Store,
    profile: &str,
    processor: &str,
    worker: &str,
    lease_seconds: i64,
    job_ids: Option<&[String]>,
) -> Result<Value> {
    let claim = storage::claim(
        store,
        profile,
        processor,
        worker,
        lease_seconds,
        true,
        job_ids,
    )?;
    if claim["claimed"] != true {
        return Ok(claim);
    }
    dispatch(
        store,
        profile,
        claim["id"].as_str().unwrap(),
        claim["lease_token"].as_str().unwrap(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        os::unix::fs::MetadataExt,
    };
    #[test]
    fn supervised_cancellation_reaps_helper_before_returning() {
        use std::{
            os::unix::fs::PermissionsExt,
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
        };
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("helper");
        let pid_file = root.path().join("pid");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nsleep 60\n",
                pid_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let child = std::thread::spawn(move || {
            SupervisedTransport {
                executable: &executable,
                cancel: &token,
            }
            .request(&json!({"timeout":60}))
        });
        // The shell creates the file before printf writes it; wait for the PID.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let pid: libc::pid_t = loop {
            let written = std::fs::read_to_string(&pid_file).unwrap_or_default();
            if let Ok(pid) = written.parse() {
                break pid;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "helper did not record its PID"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        cancel.store(true, Ordering::Release);
        assert_eq!(
            child.join().unwrap().unwrap_err().0,
            "native legacy CLI interrupted"
        );
        let mut status = 0;
        // SAFETY: pid is our disposable direct child; status is writable and
        // WNOHANG never waits on an unrelated process.
        let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        assert_eq!(waited, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
    struct Fixture {
        dir: tempfile::TempDir,
        store: Store,
        lease: Value,
    }
    impl Fixture {
        fn new(endpoint: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("catalog.db");
            catabolic_store::migration::initialize(&path).unwrap();
            let store = Store::open(&path, true, false).unwrap();
            let root = dir.path().join("media");
            std::fs::create_dir(&root).unwrap();
            for name in ["source.mkv", "external.mkv"] {
                std::fs::write(root.join(name), name.as_bytes()).unwrap();
            }
            let st = root.metadata().unwrap();
            store.transaction(|db|{
                db.execute("INSERT INTO locations VALUES ('media')",[])?;db.execute("INSERT INTO bindings VALUES ('default','source','media',?,?,?)",params![root.to_str(),st.dev()as i64,st.ino()as i64])?;db.execute("INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES ('scan','default','media',1,2,'[]')",[])?;
                for name in ["source.mkv","external.mkv"]{let st=root.join(name).metadata()?;db.execute("INSERT INTO files VALUES (?,'media',?)",params![name,name])?;db.execute("INSERT INTO observations VALUES ('default',?,?,?,?,?,'present','scan')",params![name,st.len()as i64,st.mtime()*1_000_000_000+st.mtime_nsec(),st.dev()as i64,st.ino()as i64])?;}
                db.execute("INSERT INTO items VALUES ('item','movie','{}')",[])?;db.execute("INSERT INTO item_files(id,file_id,item_id,role,origin) VALUES ('association','source.mkv','item','primary','explicit')",[])?;Ok(())}).unwrap();
            let definition = catabolic_store::output_mutation::define(
                &store,
                "external",
                &json!({"purpose":"custom"}),
            )
            .unwrap();
            storage::put(&store, "default", "fixture", &json!({"endpoint":endpoint})).unwrap();
            storage::enqueue(
                &store,
                "default",
                "fixture",
                "source.mkv",
                "item",
                definition["id"].as_str().unwrap(),
                &json!({}),
                None,
            )
            .unwrap();
            let lease =
                storage::claim(&store, "default", "fixture", "worker", 300, false, None).unwrap();
            Self { dir, store, lease }
        }
        fn receipt(&self) -> Value {
            json!({"version":1,"database_id":self.lease["request"]["database_id"],"profile":"default","producer":"fixture","instance":"worker","job_id":self.lease["id"],"attempt":"1","configuration_digest":self.lease["request"]["configuration_digest"],"tools":{"fixture":"1"},"outcome":"complete","source":self.lease["request"]["source"],"outputs":[{"location":"media","path":"external.mkv","definition_id":self.lease["request"]["definition_id"],"size":12}]})
        }
        fn run(&mut self, t: &mut impl Transport) -> Result<Value> {
            dispatch_with_transport(
                &mut self.store,
                "default",
                self.lease["id"].as_str().unwrap(),
                self.lease["lease_token"].as_str().unwrap(),
                t,
            )
        }
    }
    struct Reply {
        raw: Vec<u8>,
        path: Option<std::path::PathBuf>,
        cancel: bool,
        calls: usize,
    }
    impl Transport for Reply {
        fn request(&mut self, _: &Value) -> Result<Vec<u8>> {
            self.calls += 1;
            if let Some(path) = &self.path {
                let store = Store::open(path, true, false)?;
                if self.cancel {
                    store.db.execute("UPDATE processor_jobs SET state='cancelled',lease_token=NULL,lease_until=NULL",[])?;
                }
            }
            Ok(self.raw.clone())
        }
    }
    #[test]
    fn detached_network_response_is_fenced_before_any_output_write() {
        let mut f = Fixture::new("http://127.0.0.1:9");
        let raw =
            catabolic_core::encode(&json!({"state":"complete","receipt":f.receipt()})).into_bytes();
        let mut reply = Reply {
            raw,
            path: Some(f.store.path.clone()),
            cancel: true,
            calls: 0,
        };
        assert!(f.run(&mut reply).unwrap_err().0.contains("lease"));
        assert_eq!(reply.calls, 1);
        assert_eq!(
            f.store
                .db
                .query_row("SELECT count(*) FROM external_receipts", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn changed_source_prevents_dispatch_and_bad_responses_leave_lease_retryable() {
        let mut f = Fixture::new("http://127.0.0.1:9");
        for (raw, message) in [
            (b"NaN".to_vec(), "processor returned invalid JSON"),
            (
                b"{\"state\":\"unexpected\"}".to_vec(),
                "processor returned an invalid job state",
            ),
            (
                vec![b' '; MAX_RESPONSE + 1],
                "HTTP response exceeds byte limit",
            ),
        ] {
            let mut reply = Reply {
                raw,
                path: Some(f.store.path.clone()),
                cancel: false,
                calls: 0,
            };
            assert_eq!(f.run(&mut reply).unwrap_err().0, message);
            assert_eq!(
                storage::job(&f.store, "default", f.lease["id"].as_str().unwrap()).unwrap()["state"],
                "leased"
            );
        }
        std::fs::write(f.dir.path().join("media/source.mkv"), b"changed source").unwrap();
        let mut reply = Reply {
            raw: vec![],
            path: None,
            cancel: false,
            calls: 0,
        };
        assert!(f.run(&mut reply).is_err());
        assert_eq!(reply.calls, 0);
    }
    fn incoming(mut socket: std::net::TcpStream) -> (std::net::TcpStream, String, Vec<u8>) {
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut reader = BufReader::new(&mut socket);
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        let mut length = 0usize;
        let mut total = first.len();
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            total += line.len();
            assert!(total < 65536);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse().unwrap();
            }
        }
        assert!(length < 256 * 1024);
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        drop(reader);
        (socket, first, body)
    }
    #[test]
    fn real_http_put_then_poll_accepts_receipt_with_writer_lock_released() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let mut f = Fixture::new(&endpoint);
        let path = f.store.path.clone();
        let receipt = f.receipt();
        let job = f.lease["id"].as_str().unwrap().to_owned();
        let server = std::thread::spawn(move || {
            let mut methods = vec![];
            for step in 0..2 {
                let (socket, _) = listener.accept().unwrap();
                let (mut socket, line, body) = incoming(socket);
                methods.push(line.clone());
                let store =
                    Store::open(&path, true, false).expect("network I/O must not hold writer lock");
                drop(store);
                if step == 0 {
                    let request = catabolic_core::decode_json_bytes(&body).unwrap();
                    assert_eq!(request["job_id"], job);
                    assert_eq!(request["instance"], "worker");
                    assert_eq!(request["attempt"], "1");
                    assert!(request.get("lease_token").is_none());
                } else {
                    assert!(body.is_empty());
                }
                let reply = catabolic_core::encode(&if step == 0 {
                    json!({"state":"running"})
                } else {
                    json!({"state":"complete","receipt":receipt})
                });
                write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",reply.len(),reply).unwrap();
            }
            methods
        });
        let first = f.run(&mut HttpTransport).unwrap();
        assert_eq!(first["state"], "submitted");
        assert!(first.get("lease_token").is_none());
        let result = f.run(&mut HttpTransport).unwrap();
        assert_eq!(result["reused"], false);
        let methods = server.join().unwrap();
        assert!(methods[0].starts_with("PUT /jobs/"));
        assert!(methods[1].starts_with("GET /jobs/"));
        assert_eq!(
            storage::job(&f.store, "default", f.lease["id"].as_str().unwrap()).unwrap()["state"],
            "complete"
        );
    }
    #[test]
    fn real_http_does_not_follow_redirects_or_accept_oversize_bodies() {
        for (response, expected) in [
            (
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
                "redirect refused; configure the final endpoint explicitly",
            ),
            (
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    MAX_RESPONSE + 1,
                    " ".repeat(MAX_RESPONSE + 1)
                ),
                "HTTP response exceeds byte limit",
            ),
            ("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\n{\"st".to_owned(), NETWORK_ERROR),
            ("garbage\r\n\r\n".to_owned(), NETWORK_ERROR),
            (String::new(), NETWORK_ERROR),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (socket, _) = listener.accept().unwrap();
                let (mut socket, _, _) = incoming(socket);
                socket.write_all(response.as_bytes()).unwrap();
            });
            let request = json!({"url":endpoint,"method":"GET","timeout":2});
            assert_eq!(HttpTransport.request(&request).unwrap_err().0, expected);
            server.join().unwrap();
        }
        // Frozen read(maximum + 1) returns a length-delimited body truncated by
        // EOF; an advertised oversized length alone is not decisive.
        for (response, expected) in [
            (
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    MAX_RESPONSE + 1
                ),
                &b""[..],
            ),
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n{}".to_owned(),
                &b"{}"[..],
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (socket, _) = listener.accept().unwrap();
                let (mut socket, _, _) = incoming(socket);
                socket.write_all(response.as_bytes()).unwrap();
            });
            let request = json!({"url":endpoint,"method":"GET","timeout":2});
            assert_eq!(HttpTransport.request(&request).unwrap(), expected);
            server.join().unwrap();
        }
    }
    #[test]
    fn refused_connection_uses_frozen_worker_diagnostic() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let request = json!({"url":endpoint,"method":"GET","timeout":2});
        assert_eq!(
            HttpTransport.request(&request).unwrap_err().0,
            NETWORK_ERROR
        );
    }
    #[test]
    fn credential_header_encoding_failures_use_frozen_worker_diagnostic() {
        let variable = "CATABOLIC_PROCESSOR_TEST_ENCODED_CREDENTIAL";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        // The ASCII control is the only request that reaches the server.
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(socket);
            let mut header = vec![];
            loop {
                let mut line = vec![];
                assert!(reader.read_until(b'\n', &mut line).unwrap() > 0);
                if line == b"\r\n" {
                    break;
                }
                if line.to_ascii_lowercase().starts_with(b"authorization:") {
                    header = line;
                }
            }
            let mut socket = reader.into_inner();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
            header
        });
        let request = json!({"url":endpoint,"method":"GET","timeout":2,"credential_env":variable});
        // SAFETY: this test alone uses the variable; no thread reads it concurrently.
        unsafe { std::env::set_var(variable, "FAKE-ascii") };
        assert_eq!(HttpTransport.request(&request).unwrap(), b"{}");
        let header = server.join().unwrap();
        assert!(header.to_ascii_lowercase().starts_with(b"authorization: "));
        assert!(header.ends_with(b": Bearer FAKE-ascii\r\n"), "{header:?}");
        // The listener is gone: a wider character must fail before connecting
        // with the frozen diagnostic, and a Latin-1 byte fails the same way.
        for secret in ["FAKE-\u{2603}", "FAKE-caf\u{e9}"] {
            // SAFETY: as above; only this test touches this unique variable.
            unsafe { std::env::set_var(variable, secret) };
            assert_eq!(
                HttpTransport.request(&request).unwrap_err().0,
                NETWORK_ERROR
            );
        }
        // SAFETY: as above; only this test touches this unique variable.
        unsafe { std::env::set_var(variable, "x".repeat(300 * 1024)) };
        assert_eq!(
            HttpTransport.request(&request).unwrap_err().0,
            "HTTP request exceeds input limit"
        );
        // SAFETY: as above; only this test touches this unique variable.
        unsafe { std::env::remove_var(variable) };
    }
    #[test]
    fn request_budget_is_the_exact_frozen_worker_envelope() {
        let base = json!({"url":"http://127.0.0.1:9/jobs/a/1","method":"PUT","timeout":15});
        let envelope = |body: &str, secret: Option<&str>| {
            use base64::Engine;
            let mut headers = json!({"Content-Type":"application/json"});
            if let Some(secret) = secret {
                headers["Authorization"] = json!(format!("Bearer {secret}"));
            }
            catabolic_core::encode(&json!({
                "body": base64::engine::general_purpose::STANDARD
                    .encode(catabolic_core::encode(&json!(body))),
                "headers": headers, "maximum": 1048576, "method": "PUT",
                "structured_errors": false, "timeout": 15, "url": base["url"],
            }))
            .len()
        };
        // Three body bytes per base64 quantum; adjust a secret to land exactly.
        let body = "b".repeat(196_000);
        let mut request = base.clone();
        request["body"] = json!(body);
        let fixed = envelope(&body, Some(""));
        let exact = "s".repeat(256 * 1024 - fixed);
        assert_eq!(envelope(&body, Some(&exact)), 256 * 1024);
        assert!(request_budget(&request, Some(&exact)).is_ok());
        let over = format!("{exact}\u{e9}");
        assert_eq!(envelope(&body, Some(&over)), 256 * 1024 + 2);
        assert_eq!(
            request_budget(&request, Some(&over)).unwrap_err().0,
            "HTTP request exceeds input limit"
        );
        assert!(request_budget(&request, None).is_ok());
    }
    #[test]
    fn global_timeout_bounds_slow_response_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let (mut socket, _, _) = incoming(socket);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{")
                .unwrap();
            std::thread::sleep(Duration::from_secs(2));
        });
        let started = std::time::Instant::now();
        let result = HttpTransport.request(&json!({"url":endpoint,"method":"GET","timeout":1}));
        assert!(result.is_err());
        assert!(
            started.elapsed() < Duration::from_millis(1800),
            "response body exceeded global timeout"
        );
        server.join().unwrap();
    }
}
