// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Preserved TMDB candidate intake. Reviewable proposals remain owned by the
//! existing curation transaction; this operation never accepts a candidate.
use base64::{Engine, engine::general_purpose::STANDARD};
use catabolic_store::{Error, Result, Store, curation};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    io::{Read, Write},
    path::Path,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub const WORKER_ARGUMENT: &str = "__identify-tmdb-worker";
const ENDPOINT: &str = "https://api.themoviedb.org/3/search/movie";
const MAXIMUM: usize = 1024 * 1024;
const INPUT_MAXIMUM: usize = 256 * 1024;
const INTERRUPTED: &str = "native legacy CLI interrupted";
const NETWORK_ERROR: &str = "HTTP request failed; check endpoint, credentials and network";
const WORKER_ERROR: &str = "HTTP worker failed; check endpoint and network";

fn checkpoint(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(Error(INTERRUPTED.into()))
    } else {
        Ok(())
    }
}
fn token(environment: &str) -> Result<String> {
    if environment.is_empty()
        || !environment
            .bytes()
            .enumerate()
            .all(|(i, c)| c.is_ascii_alphabetic() || c == b'_' || i > 0 && c.is_ascii_digit())
    {
        return Err(Error(
            "credential_env must be an environment variable name".into(),
        ));
    }
    let value = std::env::var(environment)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error(format!("set credential environment variable {environment}")))?;
    if value.contains(['\r', '\n']) {
        return Err(Error("invalid credential value".into()));
    }
    Ok(value)
}
fn endpoint() -> Result<String> {
    let Some(endpoint) = std::env::var_os("CATABOLIC_TMDB_FIXTURE_ENDPOINT") else {
        return Ok(ENDPOINT.into());
    };
    let invalid =
        || Error("TMDB fixture endpoint requires explicit numeric-loopback opt-in".into());
    if std::env::var("CATABOLIC_TMDB_FIXTURE").as_deref() != Ok("1") {
        return Err(invalid());
    }
    let endpoint = endpoint.to_str().ok_or_else(invalid)?;
    let socket: std::net::SocketAddr = endpoint
        .strip_prefix("http://")
        .ok_or_else(invalid)?
        .trim_end_matches('/')
        .parse()
        .map_err(|_| invalid())?;
    if !socket.ip().is_loopback() || socket.port() == 0 {
        return Err(invalid());
    }
    Ok(format!("http://{socket}/3/search/movie"))
}
fn options(request: &Value) -> Result<(&str, Option<i64>, &str)> {
    let query = request["query"]
        .as_str()
        .filter(|s| {
            !s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
                .is_empty()
                && s.len() <= 512
        })
        .ok_or_else(|| Error("query must be nonempty text of at most 512 bytes".into()))?;
    let year = if request["year"].is_null() {
        None
    } else {
        Some(
            request["year"]
                .as_i64()
                .filter(|y| (1..=9999).contains(y))
                .ok_or_else(|| Error("year must be 1..9999".into()))?,
        )
    };
    let credential = request
        .get("credential_env")
        .and_then(Value::as_str)
        .unwrap_or("TMDB_TOKEN");
    Ok((query, year, credential))
}
fn route(query: &str, year: Option<i64>) -> String {
    let mut route = format!(
        "?query={}&include_adult=false&page=1",
        crate::consumers::quote(query).replace("%20", "+")
    );
    if let Some(year) = year {
        route.push_str(&format!("&year={year}"));
    }
    route
}
fn fetch(request: &Value) -> Result<Vec<u8>> {
    let (query, year, credential) = options(request)?;
    let secret = token(credential)?;
    let url = format!("{}{}", endpoint()?, route(query, year));
    let header = format!("Bearer {secret}")
        .chars()
        .map(|c| u8::try_from(u32::from(c)))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| Error(NETWORK_ERROR.into()))?;
    let mut header =
        ureq::http::HeaderValue::from_bytes(&header).map_err(|_| Error(NETWORK_ERROR.into()))?;
    header.set_sensitive(true);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .max_idle_connections(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into();
    let mut response = agent
        .get(&url)
        .header("Authorization", header)
        .header("Accept", "application/json")
        .header("Connection", "close")
        .call()
        .map_err(|_| Error(NETWORK_ERROR.into()))?;
    let status = response.status().as_u16();
    if [301, 302, 303, 307, 308].contains(&status) && response.headers().contains_key("location") {
        return Err(Error(
            "redirect refused; configure the final endpoint explicitly".into(),
        ));
    }
    if !(200..300).contains(&status) {
        return Err(Error(format!("HTTP request failed with status {status}")));
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take((MAXIMUM + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Error(NETWORK_ERROR.into()))?;
    if bytes.len() > MAXIMUM {
        return Err(Error("HTTP response exceeds byte limit".into()));
    }
    Ok(bytes)
}
/// Private native helper, supervised by the same bounded process owner as media
/// tools. It receives no catalog path and never puts credentials in argv/output.
pub fn worker_main() {
    let result = (|| -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take((INPUT_MAXIMUM + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > INPUT_MAXIMUM {
            return Err(Error("HTTP request exceeds input limit".into()));
        }
        let request =
            catabolic_core::decode_json_bytes(&bytes).map_err(|_| Error(WORKER_ERROR.into()))?;
        fetch(&request)
    })();
    let result = match result {
        Ok(bytes) => json!({"body":STANDARD.encode(bytes)}),
        Err(error) => json!({"error":error.0}),
    };
    let _ = writeln!(std::io::stdout(), "{}", catabolic_core::encode(&result));
}
fn retrieve(executable: &Path, request: &Value, cancel: &AtomicBool) -> Result<Value> {
    let (query, year, credential) = options(request)?;
    // Keep the frozen request-size admission, including the actual escaped token,
    // but pass only its environment reference through the helper protocol.
    let secret = token(credential)?;
    let url = format!("{ENDPOINT}{}", route(query, year));
    let budget = catabolic_core::encode(
        &json!({"url":url,"headers":{"Authorization":format!("Bearer {secret}"),"Accept":"application/json"},"method":"GET","maximum":MAXIMUM,"timeout":15,"body":null,"structured_errors":false}),
    );
    if budget.len() > INPUT_MAXIMUM {
        return Err(Error("HTTP request exceeds input limit".into()));
    }
    endpoint()?;
    checkpoint(cancel)?;
    let input =
        catabolic_core::encode(&json!({"query":query,"year":year,"credential_env":credential}));
    let result = crate::process::command_output(
        &[executable.as_os_str(), OsStr::new(WORKER_ARGUMENT)],
        crate::process::CommandOptions {
            input_bytes: Some(input.as_bytes()),
            cancel: Some(cancel),
            timeout: Duration::from_secs(15),
            maximum: 2 * MAXIMUM + 4096,
            ..Default::default()
        },
    );
    checkpoint(cancel)?;
    let result = result.map_err(|e| {
        Error(
            match e.state.as_str() {
                "cancelled" => INTERRUPTED,
                "timeout" => "HTTP request exceeded the elapsed-time limit",
                _ => WORKER_ERROR,
            }
            .into(),
        )
    })?;
    if result.returncode != 0 || !result.stderr.is_empty() {
        return Err(Error(WORKER_ERROR.into()));
    }
    let value = catabolic_core::decode_json_bytes(&result.stdout)
        .map_err(|_| Error(WORKER_ERROR.into()))?;
    if let Some(error) = value["error"].as_str() {
        return Err(Error(error.into()));
    }
    let raw = STANDARD
        .decode(
            value["body"]
                .as_str()
                .ok_or_else(|| Error(WORKER_ERROR.into()))?,
        )
        .map_err(|_| Error(WORKER_ERROR.into()))?;
    if raw.len() > MAXIMUM {
        return Err(Error("HTTP response exceeds byte limit".into()));
    }
    crate::consumers::json_wire::decode(&raw)
        .map_err(|_| Error("invalid TMDB JSON response".into()))
}
fn unicode() -> &'static Value {
    static DATA: OnceLock<Value> = OnceLock::new();
    DATA.get_or_init(|| {
        serde_json::from_str(include_str!("../resources/identify-unicode.json"))
            .expect("frozen Unicode ranges")
    })
}
fn in_ranges(character: char, kind: &str) -> bool {
    let ranges = unicode()[kind].as_array().unwrap();
    let code = u64::from(character);
    let index = ranges.partition_point(|range| range[1].as_u64().unwrap() < code);
    ranges
        .get(index)
        .is_some_and(|range| range[0].as_u64().unwrap() <= code)
}
fn terms(text: &str) -> BTreeSet<String> {
    catabolic_store::sql::casefold(text)
        .split(|c| !in_ranges(c, "word"))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
fn released_year(text: &str) -> Option<u32> {
    let chars = text.chars().take(5).collect::<Vec<_>>();
    if chars.len() != 5 || chars[4] != '-' {
        return None;
    }
    chars[..4].iter().try_fold(0, |year, c| {
        let code = u64::from(*c);
        unicode()["decimal"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|range| {
                let start = range[0].as_u64().unwrap();
                (start <= code && code <= range[1].as_u64().unwrap()).then(|| (code - start) % 10)
            })
            .map(|digit| year * 10 + digit as u32)
    })
}
fn candidates(response: &Value, request: &Value, fetched: &str) -> Result<Vec<Value>> {
    let results = response
        .as_object()
        .and_then(|v| v.get("results"))
        .and_then(Value::as_array)
        .ok_or_else(|| Error("invalid TMDB response".into()))?;
    let (query, year, _) = options(request)?;
    let words = terms(query);
    let mut candidates = vec![];
    for result in results.iter().take(20) {
        let Some(number) = result.get("id").filter(|v| v.is_number()) else {
            continue;
        };
        let id = catabolic_core::encode(number);
        if id.contains(['.', 'e', 'E']) {
            continue;
        }
        let Some(title) = result["title"].as_str() else {
            continue;
        };
        let mut metadata = json!({"title":title});
        if let Some(year) = result["release_date"].as_str().and_then(released_year) {
            metadata["year"] = json!(year);
        }
        let found = terms(title);
        let union = words.union(&found).count();
        let score = if union == 0 {
            json!(0)
        } else {
            json!(words.intersection(&found).count() as f64 / union as f64)
        };
        candidates.push(json!({"file_id":request["file_id"],"payload":{"item":{"kind":"movie","identities":{"tmdb.movie":id},"metadata":metadata}},
            "evidence":{"provider":"tmdb.movie","url":ENDPOINT,"retrieved_at":fetched,"query":query,"title_word_overlap":score,"year_matches":year.map(|y| metadata["year"].as_i64() == Some(y)),"ranking_method":"title token overlap; not a probability"}}));
    }
    Ok(candidates)
}
fn complete(response: &Value) -> Result<bool> {
    match response.get("total_pages") {
        None | Some(Value::Bool(_)) => Ok(true),
        Some(Value::Number(n)) => Ok(n
            .as_f64()
            .map(|n| n <= 1.)
            .unwrap_or_else(|| n.to_string().starts_with('-'))),
        _ => Err(Error("invalid TMDB total_pages".into())),
    }
}
/// Candidate intake owns no alternative persistence protocol. Apply forwards
/// every candidate to the existing per-proposal transaction in provider order,
/// then ranks the returned report with the frozen stable overlap ordering.
pub fn execute(
    store: &mut Store,
    profile: &str,
    request: &Value,
    executable: &Path,
    cancel: &AtomicBool,
) -> Result<Value> {
    catabolic_store::mutation::profile(store, profile)?;
    options(request)?;
    if request["apply"] == true && (!store.writable || !store.owns_writer_lock()) {
        return Err(Error(
            "TMDB candidate apply requires a writable catalog session".into(),
        ));
    }
    checkpoint(cancel)?;
    let response = crate::consumers::network(store, || retrieve(executable, request, cancel))?;
    checkpoint(cancel)?;
    catabolic_store::mutation::profile(store, profile)?;
    let fetched = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false);
    let mut candidates = candidates(&response, request, &fetched)?;
    for candidate in &mut candidates {
        checkpoint(cancel)?;
        if request["apply"] == true {
            let proposal = curation::proposal_put(
                store,
                profile,
                &json!({"file_id":candidate["file_id"],"payload":candidate["payload"],"source":"tmdb.movie","evidence":candidate["evidence"]}),
            )?;
            candidate["proposal"] = proposal["id"].clone();
        }
    }
    candidates.sort_by(|a, b| {
        b["evidence"]["title_word_overlap"]
            .as_f64()
            .unwrap()
            .total_cmp(&a["evidence"]["title_word_overlap"].as_f64().unwrap())
    });
    Ok(
        json!({"candidates":candidates,"complete":complete(&response)?,"provider_page":1,"attribution":"This product uses the TMDB API but is not endorsed or certified by TMDB."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_word_and_decimal_semantics_handle_non_ascii_numbers() {
        assert_eq!(
            terms("Straße Ⅷ café \u{0345} \u{0301}"),
            BTreeSet::from(["strasse".into(), "ⅷ".into(), "café".into(), "ι".into()])
        );
        assert_eq!(released_year("٢٠٢٦-01-01"), Some(2026));
        assert_eq!(released_year("𝟚𝟘𝟚𝟞-"), Some(2026));
        assert_eq!(released_year("²⁰²⁶-"), None);
        assert_eq!(released_year("2026"), None);
        assert!(!in_ranges('\u{0301}', "word"));
        assert!(in_ranges('²', "word"));
    }

    #[test]
    fn candidates_filter_before_ranking_without_replenishing_first_page() {
        let request = json!({"query":"one", "file_id":"absent"});
        let mut rows = vec![json!({"id":1.0,"title":"one"}); 20];
        rows.push(json!({"id":2,"title":"one"}));
        assert!(
            candidates(&json!({"results":rows}), &request, "fixture")
                .unwrap()
                .is_empty()
        );
        let rows = candidates(&json!({"results":[{"id":true,"title":"one"},{"id":-2,"title":"ONE"},{"id":3,"title":""}]}), &request, "fixture").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["evidence"]["title_word_overlap"], 1.0);
        assert_eq!(rows[1]["evidence"]["title_word_overlap"], 0.0);
        assert_eq!(rows[0]["payload"]["item"]["identities"]["tmdb.movie"], "-2");
        let empty = candidates(
            &json!({"results":[{"id":1,"title":"!?"}]}),
            &json!({"query":"!", "file_id":"x"}),
            "fixture",
        )
        .unwrap();
        assert_eq!(empty[0]["evidence"]["title_word_overlap"].to_string(), "0");
    }

    #[test]
    fn readonly_apply_and_cancellation_never_start_a_helper() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("catalog.sqlite3");
        catabolic_store::migration::initialize(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        let mut store = Store::open(&path, false, false).unwrap();
        let request = json!({"query":"movie","file_id":"file","apply":true});
        let missing = root.path().join("must-not-start");
        let error = execute(
            &mut store,
            "default",
            &request,
            &missing,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.0.contains("writable catalog"));
        let error = execute(
            &mut store,
            "default",
            &json!({"query":"movie"}),
            &missing,
            &AtomicBool::new(true),
        )
        .unwrap_err();
        assert_eq!(error.0, INTERRUPTED);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
