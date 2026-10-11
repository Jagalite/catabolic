// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Bounded external receipt-v1 dispatch. HTTP runs outside the catalog writer
//! lock and every returned result is fenced again before durable acceptance.
use catabolic_store::{Error, Result, Store, processor_mutation as storage};
use serde_json::{Value, json};
use std::{io::Read, time::Duration};
const MAX_RESPONSE: usize = 1024 * 1024;

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
        let mut headers = vec![("Content-Type", "application/json".to_owned())];
        if let Some(environment) = request["credential_env"].as_str() {
            catabolic_store::notifications::credential_reference(environment)?;
            let secret = std::env::var(environment)
                .ok()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    Error(format!("set credential environment variable {environment}"))
                })?;
            if secret.contains(['\n', '\r']) || secret.len() > 256 * 1024 {
                return Err(Error("invalid credential value".into()));
            }
            headers.push(("Authorization", format!("Bearer {secret}")));
        }
        let body = request
            .get("body")
            .filter(|v| !v.is_null())
            .map(catabolic_core::encode);
        let header_bytes = headers
            .iter()
            .map(|(k, v)| k.len() + v.len())
            .sum::<usize>();
        // Bound the old worker envelope as well as the direct native request.
        if url.len() + header_bytes + body.as_ref().map_or(0, |b| b.len().div_ceil(3) * 4) + 1024
            > 256 * 1024
        {
            return Err(Error("HTTP request exceeds input limit".into()));
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(timeout)))
            .build()
            .into();
        let failed = || Error("HTTP worker failed; check endpoint and network".into());
        let mut response = match request["method"].as_str() {
            Some("PUT") => {
                let mut outgoing = agent.put(url);
                for (key, value) in &headers {
                    outgoing = outgoing.header(*key, value);
                }
                outgoing
                    .send(body.as_deref().unwrap_or("").as_bytes())
                    .map_err(|_| failed())?
            }
            Some("GET") => {
                let mut outgoing = agent.get(url);
                for (key, value) in &headers {
                    outgoing = outgoing.header(*key, value);
                }
                outgoing.call().map_err(|_| failed())?
            }
            _ => return Err(Error("invalid processor HTTP method".into())),
        };
        if !response.status().is_success() {
            return Err(Error(format!(
                "HTTP request failed with status {}",
                response.status().as_u16()
            )));
        }
        if response
            .headers()
            .get("Content-Length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .is_some_and(|n| n > MAX_RESPONSE as u64)
        {
            return Err(Error("HTTP response exceeds limit".into()));
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_RESPONSE as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| failed())?;
        if bytes.len() > MAX_RESPONSE {
            return Err(Error("HTTP response exceeds limit".into()));
        }
        Ok(bytes)
    }
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
        return Err(Error("HTTP response exceeds limit".into()));
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
        for raw in [
            b"NaN".to_vec(),
            b"{\"state\":\"unexpected\"}".to_vec(),
            vec![b' '; MAX_RESPONSE + 1],
        ] {
            let mut reply = Reply {
                raw,
                path: Some(f.store.path.clone()),
                cancel: false,
                calls: 0,
            };
            assert!(f.run(&mut reply).is_err());
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
        for response in ["HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",MAX_RESPONSE+1)]{
            let listener=TcpListener::bind("127.0.0.1:0").unwrap();let endpoint=format!("http://{}",listener.local_addr().unwrap());let server=std::thread::spawn(move||{let(socket,_)=listener.accept().unwrap();let(mut socket,_,_)=incoming(socket);socket.write_all(response.as_bytes()).unwrap();});let request=json!({"url":endpoint,"method":"GET","timeout":2});assert!(HttpTransport.request(&request).is_err());server.join().unwrap();
        }
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
