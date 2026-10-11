// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Bounded Plex/Jellyfin adapters. Discovery and scan requests run with the
//! catalog completely detached; only native Store code owns durable state.
use catabolic_store::{
    Error, Result, Store,
    consumers::{self as policy, Failure},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    time::Duration,
};

pub(crate) mod json_wire;
mod xml;
mod xml_encodings;

type NetworkResult<T> = std::result::Result<T, Failure>;
// Consumer administration owns this snapshot. Network reads deliberately end
// it, detach the catalog, then establish a fresh snapshot for revision fencing.
// Do not use this helper with an authorization Evaluation owned elsewhere.
pub(crate) fn network<T>(store: &mut Store, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let reading = !store.writable && !store.db.is_autocommit();
    if reading {
        store.db.execute_batch("COMMIT")?;
    }
    let outcome = store.detached(operation);
    if reading && outcome.is_ok() {
        store.db.execute_batch("BEGIN")?;
    }
    outcome
}

const MAXIMUM: usize = 8 * 1024 * 1024;
fn invalid() -> Failure {
    Failure::new("invalid_response")
}
fn field<'a>(value: &'a Value, key: &str) -> NetworkResult<&'a str> {
    policy::string(value, key).map_err(|_| invalid())
}
fn text(value: Option<&str>, maximum: usize) -> NetworkResult<&str> {
    value
        .filter(|s| !s.is_empty() && s.chars().count() <= maximum && !s.chars().any(|c| c < ' '))
        .ok_or_else(|| Failure::new("invalid_configuration"))
}
pub(crate) fn quote(value: &str) -> String {
    let mut result = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            result.push(b as char)
        } else {
            use std::fmt::Write;
            write!(result, "%{b:02X}").expect("write string");
        }
    }
    result
}
// ElementTree's unqualified lookup never accepts a namespaced lookalike.
fn xml_named(node: roxmltree::Node<'_, '_>, name: &str) -> bool {
    node.is_element()
        && node.tag_name().namespace().is_none_or(str::is_empty)
        && node.tag_name().name() == name
}
fn xml_attribute<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.namespace().is_none() && a.name() == name)
        .map(|a| a.value())
}
fn credentials(connection: &Value) -> NetworkResult<(String, Option<String>)> {
    let unauthorized = || Failure::new("unauthorized");
    let reference = field(connection, "credential_env")?;
    let (secret, client) = if let Some(filename) = reference.strip_prefix("file:") {
        if connection["application"] != "plex" {
            return Err(unauthorized());
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(filename)
            .map_err(|_| unauthorized())?;
        let metadata = file.metadata().map_err(|_| unauthorized())?;
        // SAFETY: getuid has no arguments and no failure mode.
        let owner = unsafe { libc::getuid() };
        if !metadata.is_file()
            || metadata.uid() != owner
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(unauthorized());
        }
        let mut bytes = Vec::new();
        file.take(16385)
            .read_to_end(&mut bytes)
            .map_err(|_| unauthorized())?;
        if bytes.len() > 16384 {
            return Err(unauthorized());
        }
        let value = catabolic_core::decode_json_bytes(&bytes).map_err(|_| unauthorized())?;
        (
            text(value["token"].as_str(), 4096)
                .map_err(|_| unauthorized())?
                .to_owned(),
            Some(
                text(value["client_id"].as_str(), 128)
                    .map_err(|_| unauthorized())?
                    .to_owned(),
            ),
        )
    } else {
        policy::connection_spec("default",&json!({"id":"credential","application":connection["application"],"endpoint":connection["endpoint"],"credential_env":reference})).map_err(|_|unauthorized())?;
        (std::env::var(reference).map_err(|_| unauthorized())?, None)
    };
    if secret.is_empty() || secret.len() > 262144 || secret.contains(['\r', '\n']) {
        return Err(unauthorized());
    }
    Ok((secret, client))
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
pub(crate) struct Adapter {
    connection: Value,
    agent: ureq::Agent,
    base: String,
}
impl Adapter {
    pub(crate) fn new(connection: &Value) -> NetworkResult<Self> {
        if !matches!(
            connection["application"].as_str(),
            Some("plex" | "jellyfin")
        ) {
            return Err(Failure::new("unsupported"));
        }
        Ok(Self {
            connection: connection.clone(),
            base: policy::endpoint(field(connection, "endpoint")?)
                .map_err(|_| Failure::new("invalid_configuration"))?,
            agent: ureq::Agent::config_builder()
                .proxy(None)
                // The frozen urllib adapter uses one connection per request.
                // Do not reuse a completed HTTP/1.0 connection while its FIN
                // is still in flight; a second request can otherwise be lost.
                .max_idle_connections(0)
                .max_redirects(0)
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(15)))
                .timeout_connect(Some(Duration::from_secs(10)))
                .build()
                .into(),
        })
    }
    fn plex(&self) -> bool {
        self.connection["application"] == "plex"
    }
    pub(crate) fn call(&self, route: &str, post: bool) -> NetworkResult<Vec<u8>> {
        self.request(route, if post { "POST" } else { "GET" })
    }
    /// A single fresh-connection metadata write. Never replay a lost response.
    pub(crate) fn put(&self, route: &str) -> NetworkResult<Vec<u8>> {
        self.request(route, "PUT")
    }
    fn request(&self, route: &str, method: &str) -> NetworkResult<Vec<u8>> {
        let (secret, client) = credentials(&self.connection)?;
        let header = if self.plex() {
            "X-Plex-Token"
        } else {
            "X-Emby-Token"
        };
        let accept = if self.plex() {
            "application/xml"
        } else {
            "application/json"
        };
        let url = format!("{}{route}", self.base);
        // Never forward credentials through a redirect or ambient proxy.
        let result = if method != "GET" {
            let mut request = if method == "PUT" {
                self.agent.put(&url)
            } else {
                self.agent.post(&url)
            }
            .header("Connection", "close")
            .header(header, &secret)
            .header("Accept", accept);
            if let Some(client) = &client {
                request = request
                    .header("X-Plex-Client-Identifier", client)
                    .header("X-Plex-Product", "Catabolic");
            }
            request.send_empty()
        } else {
            let mut request = self
                .agent
                .get(&url)
                .header("Connection", "close")
                .header(header, &secret)
                .header("Accept", accept);
            if let Some(client) = &client {
                request = request
                    .header("X-Plex-Client-Identifier", client)
                    .header("X-Plex-Product", "Catabolic");
            }
            request.call()
        };
        let mut response = result.map_err(|_| Failure::new("temporarily_failed"))?;
        if !response.status().is_success() {
            let code = match response.status().as_u16() {
                401 | 403 => "unauthorized",
                429 => "rate_limited",
                404 => "unavailable",
                405 | 501 => "unsupported",
                400 | 412 | 422 => "invalid_configuration",
                _ => "temporarily_failed",
            };
            let mut failure = Failure::new(code);
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
            .is_some_and(|n| n > MAXIMUM as u64)
        {
            return Err(Failure::new("response_limit"));
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take((MAXIMUM + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| Failure::new("temporarily_failed"))?;
        if bytes.len() > MAXIMUM {
            return Err(Failure::new("response_limit"));
        }
        Ok(bytes)
    }
    pub(crate) fn xml<'a>(&self, raw: &'a [u8]) -> NetworkResult<xml::XmlText<'a>> {
        xml::XmlText::decode(raw)
    }
    fn inspect(&self) -> NetworkResult<Value> {
        let capabilities = json!({"identity":true,"libraries":true,"create_library":self.plex(),"section_scan":true,"targeted_scan":false,"scan_activity":self.plex(),"indexed_paths":self.plex()});
        if self.plex() {
            let raw = self.call("/", false)?;
            let xml = self.xml(&raw)?;
            let document = xml.parse()?;
            let root = document.root_element();
            Ok(
                json!({"server_id":text(xml_attribute(root, "machineIdentifier"),4096)?,"version":text(xml_attribute(root, "version"),4096)?,"permissions":{"discovery":"authorized","mutations":"unknown; checked on request","read_only_libraries":xml_attribute(root, "readOnlyLibraries")},"capabilities":capabilities}),
            )
        } else {
            let value =
                json_wire::decode(&self.call("/System/Info", false)?).map_err(|_| invalid())?;
            if !value.is_object() {
                return Err(invalid());
            }
            Ok(
                json!({"server_id":text(value["Id"].as_str(),4096)?,"version":text(value["Version"].as_str(),4096)?,"permissions":{"discovery":"authorized","mutations":"unknown; checked on request"},"capabilities":capabilities}),
            )
        }
    }
    pub(crate) fn libraries(&self) -> NetworkResult<Vec<Value>> {
        if self.plex() {
            let raw = self.call("/library/sections", false)?;
            let xml = self.xml(&raw)?;
            let document = xml.parse()?;
            let nodes: Vec<_> = document
                .root_element()
                .children()
                .filter(|n| xml_named(*n, "Directory"))
                .collect();
            if nodes.len() > 1000 {
                return Err(Failure::new("response_limit"));
            }
            nodes.into_iter().map(|r|{
                let mut roots=r.children().filter(|n|xml_named(*n, "Location")).map(|n|policy::path(text(xml_attribute(n, "path"),4096)?,false).map_err(|e|Failure::new(&e.0))).collect::<NetworkResult<Vec<_>>>()?;roots.sort();
                Ok(json!({"id":text(xml_attribute(r, "key"),128)?,"uuid":text(xml_attribute(r, "uuid"),256)?,"name":text(xml_attribute(r, "title"),4096)?,"type":text(xml_attribute(r, "type"),64)?,"roots":roots,"scanner":xml_attribute(r, "scanner"),"agent":xml_attribute(r, "agent"),"created_at":xml_attribute(r, "createdAt"),"scanning":match xml_attribute(r, "refreshing"){Some("1")=>Some(true),Some("0")=>Some(false),_=>None}}))
            }).collect()
        } else {
            let raw = self.call("/Library/VirtualFolders", false)?;
            let value = json_wire::decode(&raw).map_err(|_| invalid())?;
            let rows = value
                .as_array()
                .filter(|r| r.len() <= 1000)
                .ok_or_else(invalid)?;
            rows.iter().map(|r|{
                if !r.is_object(){return Err(invalid());}
                let id=text(r["ItemId"].as_str(),255)?;
                if matches!(id,"."|"..")||id.contains(['/','\\']){return Err(invalid());}
                let empty=Vec::new();let locations=r.get("Locations").map(|v|v.as_array().ok_or_else(invalid)).transpose()?.unwrap_or(&empty);
                let mut roots=locations.iter().map(|p|policy::path(text(p.as_str(),4096)?,false).map_err(|e|Failure::new(&e.0))).collect::<NetworkResult<Vec<_>>>()?;roots.sort();
                let kind=if !matches!(&r["CollectionType"], Value::Null|Value::Bool(false)) && r["CollectionType"]!=""{text(r["CollectionType"].as_str(),4096)?}else{"mixed"};
                Ok(json!({"id":id,"uuid":id,"name":text(r["Name"].as_str(),4096)?,"type":kind,"roots":roots,"scanning":null}))
            }).collect()
        }
    }
    pub(crate) fn validate(&self, library: Option<&Value>) -> NetworkResult<Value> {
        let identity = self.inspect()?;
        if identity["server_id"] != self.connection["server_id"] {
            return Err(Failure::new("server_identity_changed"));
        }
        let Some(library) = library else {
            return Ok(identity);
        };
        let found: Vec<_> = self
            .libraries()?
            .into_iter()
            .filter(|r| r["id"] == library["id"])
            .collect();
        if found.len() != 1 || !policy::same_library(&found[0], library) {
            return Err(Failure::new("library_identity_changed"));
        }
        Ok(found[0].clone())
    }
    fn scan(&self, library: &Value) -> NetworkResult<()> {
        let id = quote(field(library, "id")?);
        if self.plex() {
            self.call(&format!("/library/sections/{id}/refresh"), false)?;
        } else {
            self.call(&format!("/Items/{id}/Refresh?Recursive=true&MetadataRefreshMode=Default&ImageRefreshMode=Default&ReplaceAllMetadata=false&ReplaceAllImages=false"),true)?;
        }
        Ok(())
    }
    fn create(&self, spec: &Value) -> NetworkResult<()> {
        if !self.plex() {
            return Err(Failure::new("unsupported"));
        }
        let mut params = Vec::new();
        for (key, field_name) in [
            ("name", "name"),
            ("type", "type"),
            ("location", "root"),
            ("scanner", "scanner"),
            ("agent", "agent"),
            ("language", "language"),
        ] {
            params.push(format!("{key}={}", quote(field(spec, field_name)?)));
        }
        self.call(&format!("/library/sections?{}", params.join("&")), true)?;
        Ok(())
    }
    fn indexed(&self, library: &Value, expected: &[String], limit: usize) -> NetworkResult<Value> {
        if !self.plex() {
            return Err(Failure::new("unsupported"));
        }
        if !(1..=1000).contains(&limit) || expected.len() > limit {
            return Err(Failure::new("response_limit"));
        }
        let media_type = match library["type"].as_str() {
            Some("movie") => 1,
            Some("show") => 4,
            Some("artist") => 10,
            Some("photo") => 13,
            _ => return Err(Failure::new("unsupported")),
        };
        let raw=self.call(&format!("/library/sections/{}/all?type={media_type}&X-Plex-Container-Start=0&X-Plex-Container-Size={limit}",quote(field(library,"id")?)),false)?;
        let xml = self.xml(&raw)?;
        let document = xml.parse()?;
        let root = document.root_element();
        let found: BTreeSet<_> = root
            .descendants()
            .filter(|n| xml_named(*n, "Part"))
            .filter_map(|n| xml_attribute(n, "file"))
            .collect();
        let matched: Vec<_> = expected
            .iter()
            .filter(|p| found.contains(p.as_str()))
            .cloned()
            .collect();
        let mut identities = Vec::new();
        for item in root.children().filter(|n| n.is_element()) {
            for part in item.descendants().filter(|n| xml_named(*n, "Part")) {
                if xml_attribute(part, "file").is_some_and(|p| matched.iter().any(|m| m == p)) {
                    identities.push(json!({"path":xml_attribute(part, "file"),"rating_key":xml_attribute(item, "ratingKey"),"guid":xml_attribute(item, "guid")}));
                }
            }
        }
        let complete = matched.len() == expected.len() && !expected.is_empty();
        Ok(
            json!({"status":if complete{"indexed"}else{"inconclusive"},"expected":expected.len(),"matched":matched.len(),"matched_paths":matched,"identities":identities,"complete":complete,"coverage":"bounded first page; exact server-reported file paths, not playback/readability or generation causality"}),
        )
    }
    fn choices(&self, kind: &str) -> NetworkResult<Value> {
        if !self.plex() {
            return Err(Failure::new("unsupported"));
        }
        let number = match kind {
            "movie" => 1,
            "show" => 2,
            "artist" => 8,
            "photo" => 13,
            _ => return Err(Failure::new("unsupported")),
        };
        let raw = self.call(&format!("/system/scanners/{number}"), false)?;
        let xml = self.xml(&raw)?;
        let scanners = xml
            .parse()?
            .descendants()
            .filter(|n| xml_named(*n, "Scanner"))
            .map(|n| text(xml_attribute(n, "name"), 4096).map(str::to_owned))
            .collect::<NetworkResult<BTreeSet<_>>>()?;
        let raw = self.call(&format!("/system/agents?mediaType={number}"), false)?;
        let xml = self.xml(&raw)?;
        let agents = xml
            .parse()?
            .descendants()
            .filter(|n| xml_named(*n, "Agent"))
            .map(|n| text(xml_attribute(n, "identifier"), 4096).map(str::to_owned))
            .collect::<NetworkResult<BTreeSet<_>>>()?;
        Ok(json!({"type":kind,"scanners":scanners,"agents":agents}))
    }
}
/// Run bounded work; each remote read is outside all Store ownership. Before
/// scan admission, recheck binding revisions, generation and complete local
/// publication. An accepted scan never claims that indexing has completed.
pub fn drain(store: &mut Store, profile: &str, limit: usize, automatic: bool) -> Result<Value> {
    if !(1..=100).contains(&limit) {
        return Err(Error("invalid_configuration".into()));
    }
    let mut outcomes = Vec::new();
    for _ in 0..limit {
        let Some(claim) = policy::claim(store, profile, automatic, &mut crate::fallback::probe)?
        else {
            break;
        };
        let discovered = network(store, || {
            Ok((|| {
                let adapter = Adapter::new(&claim.connection)?;
                let library = adapter.validate(Some(&claim.library))?;
                if library["scanning"] == true {
                    return Err(Failure::new("busy"));
                }
                Ok((adapter, library))
            })())
        })?;
        let failure = match discovered {
            Err(e) => Some(e),
            Ok((adapter, library)) => {
                match policy::before_send(store, profile, &claim, &mut crate::fallback::probe) {
                    Err(e) => Some(Failure::new(match e.0.as_str() {
                        "publication_changed" => "publication_changed",
                        "publication_unhealthy" => "publication_unhealthy",
                        "output_binding_changed" => "output_binding_changed",
                        "stale_claim" => "stale_claim",
                        "binding_changed" => "binding_changed",
                        _ => "temporarily_failed",
                    })),
                    Ok(()) => network(store, || Ok(adapter.scan(&library)))?.err(),
                }
            }
        };
        let mut outcome = policy::acknowledge(store, profile, &claim, failure.as_ref())?;
        outcome["group_id"] = json!(claim.group_id);
        outcomes.push(outcome);
    }
    let status = policy::status(store, profile, 100)?;
    Ok(
        json!({"processed":outcomes.len(),"deliveries":outcomes,"unresolved_bindings":status["unresolved_bindings"],"complete":status["complete"]}),
    )
}
fn creation(store: &mut Store, profile: &str, request: &Value) -> Result<Value> {
    let id = policy::text(&request["id"], 255)?;
    let spec = policy::creation_spec(&request["spec"])?;
    let connection = policy::connection(store, profile, policy::string(request, "connection")?)?;
    let existing = policy::creation_intent(store, profile, id)?.is_some();
    let (libraries, choices) = network(store, || {
        Ok((|| {
            let adapter = Adapter::new(&connection)?;
            adapter.validate(None)?;
            if !adapter.plex() {
                return Err(Failure::new("unsupported"));
            }
            let libraries = adapter.libraries()?;
            let choices = if existing {
                None
            } else {
                Some(adapter.choices(field(&spec, "type")?)?)
            };
            Ok((libraries, choices))
        })())
    })?
    .map_err(|e| e.error())?;
    let (report, send) = policy::creation_admit(
        store,
        profile,
        request,
        &connection,
        &libraries,
        choices.as_ref(),
    )?;
    if !send {
        return Ok(report);
    }
    // The durable uncertain intent is now the only authority to send. A failed
    // POST may have succeeded remotely; never retry it, including after restart.
    let _ = network(store, || {
        Ok(Adapter::new(&connection).and_then(|a| a.create(&spec)))
    })?;
    let mut reconciliation = request.clone();
    reconciliation["apply"] = json!(false);
    reconciliation["reconcile"] = json!(true);
    creation(store, profile, &reconciliation)
}
pub fn execute(
    store: &mut Store,
    profile: &str,
    operation: &str,
    request: &Value,
) -> Result<Value> {
    match operation {
        "creation" => creation(store, profile, request),
        "create" => {
            let mut proposed = request.clone();
            proposed["library_id"] = json!("creation-preview");
            policy::validate_binding_request(&proposed)?;
            let spec = policy::creation_spec(&request["spec"])?;
            if spec["root"] != request["remote_root"] || spec["type"] != request["type"] {
                return Err(Error("creation_binding_mismatch".into()));
            }
            catabolic_store::observation::binding(
                store,
                profile,
                "output",
                policy::string(request, "catalog")?,
            )?;
            if policy::binding(store, profile, policy::string(request, "id")?).is_ok()
                && policy::creation_intent(store, profile, policy::string(request, "id")?)?
                    .is_none()
            {
                return Err(Error("binding_id_already_exists".into()));
            }
            let report = creation(store, profile, request)?;
            if report["complete"] != true || report.get("library").is_none() {
                return Ok(report);
            }
            proposed["library_id"] = report["library"]["id"].clone();
            proposed["origin"] = report["origin"].clone();
            proposed["apply"] = json!(request["apply"] == true || request["reconcile"] == true);
            execute(store, profile, "bind", &proposed)
        }
        "connection-put" => {
            let spec = policy::connection_spec(profile, request)?;
            let identity = network(store, || Ok(Adapter::new(&spec).and_then(|a| a.inspect())))?
                .map_err(|e| e.error())?;
            policy::put_connection(store, profile, request, &spec, &identity)
        }
        "discover" => {
            let connection =
                policy::connection(store, profile, policy::string(request, "connection")?)?;
            network(store, || {
                (|| {
                    let adapter = Adapter::new(&connection)?;
                    let identity = adapter.validate(None)?;
                    let libraries = adapter.libraries()?;
                    let mut result =
                        json!({"identity":identity,"libraries":libraries,"complete":true});
                    if let Some(kind) = request["type"].as_str() {
                        result["creation_choices"] = adapter
                            .choices(kind)
                            .unwrap_or_else(|e| json!({"error":e.report(),"complete":false}));
                    }
                    Ok(result)
                })()
                .map_err(|e: Failure| e.error())
            })
        }
        "bind" => {
            policy::validate_binding_request(request)?;
            let connection =
                policy::connection(store, profile, policy::string(request, "connection")?)?;
            let libraries = network(store, || {
                Ok((|| {
                    let adapter = Adapter::new(&connection)?;
                    adapter.validate(None)?;
                    adapter.libraries()
                })())
            })?
            .map_err(|e| e.error())?;
            let mut result = policy::bind(
                store,
                profile,
                request,
                &connection,
                &libraries,
                &mut crate::fallback::probe,
            )?;
            if request["apply"] == true && request["initial_scan"] == true {
                after_publication(store, profile, &mut result)?;
            }
            Ok(result)
        }
        "verify-indexing" => {
            let limit = policy::limit(request, 100, 1000)?;
            let claim = policy::indexing_claim(
                store,
                profile,
                policy::string(request, "id")?,
                limit,
                &mut crate::fallback::probe,
            )?;
            let library = catabolic_core::decode_json(policy::string(&claim.binding, "library")?)?;
            let report = network(store, || {
                Ok((|| {
                    let adapter = Adapter::new(&claim.connection)?;
                    let library = adapter.validate(Some(&library))?;
                    adapter.indexed(&library, &claim.expected, limit)
                })())
            })?
            .map_err(|e| e.error())?;
            policy::indexing_observed(store, profile, &claim, &report)
        }
        "run" | "drain" => drain(
            store,
            profile,
            policy::limit(request, 10, 100)?,
            request["automatic"] == true,
        ),
        _ => policy::execute(store, profile, operation, request),
    }
}

/// Native equivalent of the command-close publication callback. One bounded
/// automatic drain is shared by every publication result in the command.
pub fn after_publication(store: &mut Store, profile: &str, result: &mut Value) -> Result<()> {
    fn marker(value: &Value) -> bool {
        value["scope"] == "profile"
            && value["processed"] == 0
            && value["unresolved_bindings"].is_u64()
            && matches!(value["state"].as_str(), Some("pending_worker" | "idle"))
    }
    fn has_destination(value: &Value) -> bool {
        match value {
            Value::Object(map) => {
                map.get("consumers").is_some_and(marker) || map.values().any(has_destination)
            }
            Value::Array(values) => values.iter().any(has_destination),
            _ => false,
        }
    }
    fn update(value: &mut Value, report: &Value) {
        match value {
            Value::Object(map) => {
                if map.get("consumers").is_some_and(marker) {
                    map.insert("consumers".into(), report.clone());
                }
                for (key, value) in map.iter_mut() {
                    if key != "consumers" {
                        update(value, report);
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    update(value, report)
                }
            }
            _ => {}
        }
    }
    if has_destination(result) {
        let report = drain(store, profile, 4, true).unwrap_or_else(|_| json!({"complete":false,"error":{"code":"worker_unavailable","safe_to_retry":true}}));
        update(result, &report);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_delays_and_path_quoting_are_bounded() {
        assert_eq!(retry_after(None), 0.0);
        assert_eq!(retry_after(Some("")), 0.0);
        assert_eq!(retry_after(Some(" -10 ")), 0.0);
        assert_eq!(retry_after(Some("120")), 120.0);
        assert_eq!(retry_after(Some("90000")), 86400.0);
        assert_eq!(
            retry_after(Some("999999999999999999999999999999999")),
            86400.0
        );
        assert_eq!(retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT")), 0.0);
        assert_eq!(retry_after(Some("invalid")), 60.0);
        assert_eq!(quote("a/b é?"), "a%2Fb%20%C3%A9%3F");
    }
    #[test]
    fn xml_never_resolves_entities_or_accepts_unbounded_trees() {
        let adapter=Adapter::new(&json!({"application":"plex","endpoint":"http://127.0.0.1:9","credential_env":"UNUSED"})).ok().unwrap();
        for bytes in [
            b"<!DOCTYPE x []><x/>".as_slice(),
            b"<!ENTITY x SYSTEM 'file:///must-not-be-read'><x/>",
            b"<x>",
        ] {
            assert!(
                adapter
                    .xml(bytes)
                    .and_then(|xml| xml.parse().map(|_| ()))
                    .is_err()
            );
        }
        assert!(adapter.xml(b"<x a='safe &amp; literal'/>").is_ok());
        let huge = format!("<x>{}</x>", "<y/>".repeat(200_000));
        assert!(
            adapter
                .xml(huge.as_bytes())
                .and_then(|xml| xml.parse().map(|_| ()))
                .is_err()
        );
    }
}
