// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Native transports for Catabolic's frozen, allowlisted Apprise URL dialect.
//! No event subject, envelope, provider response, URL or credential is logged.
//! See docs/RUST_MIGRATION_NATIVE_NOTIFICATIONS.md for explicit option gaps.
use base64::{Engine, engine::general_purpose::STANDARD};
use catabolic_store::notifications::Outcome;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Read,
    time::{Duration, Instant},
};
#[path = "smtp.rs"]
mod smtp;

const BUDGET: Duration = Duration::from_secs(30);
const RESPONSE_LIMIT: usize = 65536;
const IMAGE_BASE: &str =
    "https://github.com/caronc/apprise/raw/master/apprise/assets/themes/default";

pub(super) fn deliver(url: &str, event: &str, severity: &str) -> Outcome {
    let Some(body) = message(event) else {
        return Outcome::InvalidDestination;
    };
    let deadline = Instant::now() + BUDGET;
    if url
        .split_once(':')
        .is_some_and(|(s, _)| s.eq_ignore_ascii_case("mailto") || s.eq_ignore_ascii_case("mailtos"))
    {
        return smtp::deliver(url, body, deadline);
    }
    let Ok(mut plan) = prepare(url, body, severity) else {
        return Outcome::InvalidDestination;
    };
    let mut outcome = Outcome::TemporarilyFailed;
    for attempt in 0..=plan.retry {
        if attempt > 0 && !pause(plan.wait, deadline) {
            return Outcome::TemporarilyFailed;
        }
        outcome = execute(&mut plan, deadline);
        if outcome == Outcome::Complete {
            return outcome;
        }
        if Instant::now() >= deadline {
            return Outcome::TemporarilyFailed;
        }
    }
    if plan.optional {
        Outcome::Complete
    } else {
        outcome
    }
}

fn message(event: &str) -> Option<&'static str> {
    Some(match event {
        "job_completed" => "A Catabolic processing job completed.",
        "job_failed" => "A Catabolic processing job failed.",
        "fallback_selected" => "A Catabolic fallback was selected.",
        "fallback_unresolved" => "A Catabolic fallback remains unresolved.",
        "projection_updated" => "A Catabolic projection update was locally verified.",
        "scan_requested" => {
            "A consumer accepted a library scan request. Indexing has not been verified."
        }
        "scan_failed" => {
            "A library scan request could not be delivered. Retry the consumer delivery after repair."
        }
        "consumer_needs_repair" => {
            "A Catabolic consumer integration needs configuration or permission repair."
        }
        _ => return None,
    })
}

// Deliberately no Debug implementations: these structs contain secret material.
struct Destination {
    scheme: String,
    host: String,
    port: Option<u16>,
    user: Option<String>,
    password: Option<String>,
    token_hint: bool,
    path: Vec<String>,
    query: BTreeMap<String, String>,
}
impl Destination {
    fn parse(url: &str) -> Result<Self, ()> {
        if url.len() > 16384 || url.chars().any(|c| c.is_control()) || url.contains(['#', '\\']) {
            return Err(());
        }
        let (scheme, rest) = url.split_once("://").ok_or(())?;
        let scheme = scheme.to_ascii_lowercase();
        let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (userinfo, authority) = authority
            .rsplit_once('@')
            .map(|(a, b)| (Some(a), b))
            .unwrap_or((None, authority));
        let (mut user, mut password) = match userinfo {
            None => (None, None),
            Some(s) => {
                let (u, p) = s
                    .split_once(':')
                    .map(|(u, p)| (u, Some(p)))
                    .unwrap_or((s, None));
                (Some(u.to_owned()), p.map(str::to_owned))
            }
        };
        let (host, mut port) = if let Some(host) = authority.strip_prefix('[') {
            let (host, tail) = host.split_once(']').ok_or(())?;
            (
                format!("[{host}]"),
                if tail.is_empty() {
                    None
                } else {
                    Some(
                        tail.strip_prefix(':')
                            .ok_or(())?
                            .parse::<u16>()
                            .map_err(|_| ())?,
                    )
                },
            )
        } else if let Some((h, p)) = authority.rsplit_once(':') {
            (decode(h)?, Some(p.parse::<u16>().map_err(|_| ())?))
        } else {
            (decode(authority)?, None)
        };
        if host.is_empty() || port == Some(0) {
            return Err(());
        }
        let path = path
            .split('/')
            .filter(|p| !p.is_empty())
            .map(decode)
            .collect::<Result<Vec<_>, _>>()?;
        let mut q = BTreeMap::new();
        for pair in query.split(['&', ';']).filter(|s| !s.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            q.insert(
                decode(key)?.trim().to_ascii_lowercase(),
                decode(value)?.trim().to_owned(),
            );
        }
        if let Some(value) = q.get("password") {
            password = Some(value.clone());
        }
        if let Some(value) = q.get("pass") {
            password = Some(value.clone());
        }
        if let Some(value) = q.get("user") {
            if password.is_none() {
                password = user.clone();
            }
            user = Some(value.clone());
        }
        if let Some(value) = q.get("port") {
            port = Some(value.parse::<u16>().map_err(|_| ())?);
        }
        if port == Some(0) {
            return Err(());
        }
        let token_hint = user.as_ref().is_some_and(|u| {
            u.to_ascii_lowercase().starts_with("tk_") && !u.chars().any(char::is_whitespace)
        }) && password.as_deref().unwrap_or("").is_empty();
        let user = user.as_deref().map(decode).transpose()?;
        let password = password.as_deref().map(decode).transpose()?;
        Ok(Self {
            scheme,
            host,
            port,
            user,
            password,
            token_hint,
            path,
            query: q,
        })
    }
    fn q(&self, key: &str) -> &str {
        self.query.get(key).map(String::as_str).unwrap_or("")
    }
    fn flag(&self, key: &str, default: bool) -> bool {
        self.query.get(key).map(|v| boolean(v)).unwrap_or(default)
    }
    fn origin(&self) -> Result<String, ()> {
        let scheme = if self.scheme.ends_with('s') {
            "https"
        } else {
            "http"
        };
        let url = format!(
            "{scheme}://{}{}",
            self.host,
            self.port.map(|p| format!(":{p}")).unwrap_or_default()
        );
        super::callback_origin(&url).map_err(|_| ())?;
        Ok(url)
    }
    fn format(&self, default: &str) -> String {
        match self.q("format").to_ascii_lowercase().as_str() {
            "text" => "text",
            "html" => "html",
            "markdown" => "markdown",
            _ => default,
        }
        .into()
    }
}
fn decode(s: &str) -> Result<String, ()> {
    let mut out = Vec::with_capacity(s.len());
    let raw = s.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%'
            && i + 2 < raw.len()
            && let (Some(a), Some(b)) = (
                (raw[i + 1] as char).to_digit(16),
                (raw[i + 2] as char).to_digit(16),
            )
        {
            out.push((a * 16 + b) as u8);
            i += 3;
            continue;
        }
        out.push(raw[i]);
        i += 1;
    }
    let s = String::from_utf8(out).map_err(|_| ())?;
    if s.chars().any(|c| c.is_control()) {
        Err(())
    } else {
        Ok(s)
    }
}
fn encode(s: &str) -> String {
    let mut out = String::new();
    for c in s.bytes() {
        if c.is_ascii_alphanumeric() || b"-._~".contains(&c) {
            out.push(c as char)
        } else {
            out.push_str(&format!("%{c:02X}"))
        }
    }
    out
}
fn boolean(s: &str) -> bool {
    let prefix = s.to_ascii_lowercase().chars().take(2).collect::<String>();
    matches!(
        prefix.as_str(),
        "en" | "al" | "t" | "y" | "ye" | "on" | "1" | "tr"
    )
}
fn list(s: &str) -> Vec<String> {
    let mut v: Vec<_> = s
        .split(|c: char| c.is_whitespace() || ",;/|".contains(c))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    v.sort();
    v.dedup();
    v
}
fn image(severity: &str) -> String {
    format!(
        "{IMAGE_BASE}/apprise-{}-256x256.png",
        if severity == "error" {
            "failure"
        } else {
            severity
        }
    )
}
fn token(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(char::is_whitespace)
}

struct Request {
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Value>,
    codes: Vec<u16>,
    discord_retry: bool,
}
struct Plan {
    requests: Vec<Request>,
    discovery: Option<Request>,
    telegram_payload: Option<Value>,
    retry: usize,
    wait: Duration,
    optional: bool,
    connect: Duration,
    read: Duration,
    interval: Duration,
    last_request: std::cell::Cell<Option<Instant>>,
}
impl Plan {
    fn request(
        &mut self,
        url: String,
        body: Option<Value>,
        headers: Vec<(String, String)>,
        codes: &[u16],
        discord_retry: bool,
    ) {
        self.requests.push(Request {
            url,
            body,
            headers,
            codes: codes.to_vec(),
            discord_retry,
        });
    }
}
fn seconds(s: &str, default: f64, max: f64) -> Duration {
    Duration::from_secs_f64(
        s.parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && *n > 0.0)
            .unwrap_or(default)
            .min(max),
    )
}
fn prepare(url: &str, body: &str, severity: &str) -> Result<Plan, ()> {
    if !["info", "warning", "error"].contains(&severity) {
        return Err(());
    }
    // Telegram puts a colon in its token where a conventional URL has a port.
    let rewritten;
    let url = if url.to_ascii_lowercase().starts_with("tgram://") {
        let rest = &url[8..];
        let (auth, rest) = rest
            .rsplit_once('@')
            .filter(|(a, _)| !a.contains('/'))
            .map(|(a, b)| (format!("{a}@"), b))
            .unwrap_or((String::new(), rest));
        let rest = rest.strip_prefix("bot").unwrap_or(rest);
        let token = rest.split(['/', '?']).next().unwrap_or("");
        let replaced = decode(token)?.replace(':', "/");
        rewritten = format!("tgram://{auth}{replaced}{}", &rest[token.len()..]);
        &rewritten
    } else {
        url
    };
    let d = Destination::parse(url)?;
    // These inherited Apprise capabilities are intentionally outside this seam.
    if (d.query.contains_key("verify") && !d.flag("verify", true))
        || d.flag("redirect", false)
        || !d.q("template").is_empty()
        || d.query.keys().any(|k| k.starts_with(':'))
    {
        return Err(());
    }
    let mut plan = Plan {
        requests: vec![],
        discovery: None,
        telegram_payload: None,
        retry: d.q("retry").parse::<usize>().unwrap_or(0).min(10),
        wait: Duration::from_secs_f64(
            d.q("wait")
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .unwrap_or(0.5)
                .min(20.0),
        ),
        optional: d.flag("optional", false),
        connect: seconds(d.q("cto"), 4.0, 30.0),
        read: seconds(d.q("rto"), 4.0, 30.0),
        interval: match d.scheme.as_str() {
            "ntfy" | "ntfys" => Duration::from_millis(5500),
            "tgram" => Duration::from_millis(1),
            _ => Duration::ZERO,
        },
        last_request: std::cell::Cell::new(None),
    };
    match d.scheme.as_str() {
        "ntfy" | "ntfys" => ntfy(&d, body, severity, &mut plan)?,
        "gotify" | "gotifys" => gotify(&d, body, &mut plan)?,
        "discord" => discord(&d, body, severity, &mut plan)?,
        "tgram" => telegram(&d, body, &mut plan)?,
        _ => return Err(()),
    }
    // Request construction is validated completely before any outbound effect.
    for r in plan.requests.iter().chain(plan.discovery.iter()) {
        if super::callback_origin(&r.url).is_err()
            || r.url.len() > 16384
            || r.headers.iter().any(|(k, v)| {
                k.len() + v.len() > 16384 || v.chars().any(|c| c.is_control() || u32::from(c) > 255)
            })
            || r.body
                .as_ref()
                .is_some_and(|b| catabolic_core::encode(b).len() > 16384)
        {
            return Err(());
        }
    }
    Ok(plan)
}
fn ntfy(d: &Destination, body: &str, severity: &str, plan: &mut Plan) -> Result<(), ()> {
    let mut topics = list(&d.path.join("/"));
    topics.extend(list(d.q("to")));
    let cloud = match d.q("mode").to_ascii_lowercase().as_str() {
        "cloud" => true,
        "private" => false,
        "" => topics.is_empty(),
        _ => return Err(()),
    };
    if cloud
        && d.host != "ntfy.sh"
        && d.host
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        topics.push(d.host.clone());
    }
    topics.sort();
    topics.dedup();
    topics.retain(|t| {
        t.len() <= 64
            && t.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
    });
    let url = if cloud {
        "https://ntfy.sh".into()
    } else {
        d.origin()?
    };
    let mut headers = vec![("Content-Type".into(), "application/json".into())];
    let auth = decode(d.q("auth"))?.to_ascii_lowercase();
    let is_token = match auth.as_str() {
        "token" => true,
        "basic" => false,
        "" => !d.q("token").is_empty() || d.token_hint,
        _ => return Err(()),
    };
    if !cloud {
        if is_token {
            let secret = if !d.q("token").is_empty() {
                decode(d.q("token"))?
            } else {
                d.password
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .or(d.user.as_deref())
                    .unwrap_or("")
                    .to_owned()
            };
            if secret.is_empty() {
                return Ok(());
            }
            headers.push(("Authorization".into(), format!("Bearer {secret}")));
        } else if let Some(user) = d.user.as_ref().filter(|u| !u.is_empty()) {
            let credentials = format!("{user}:{}", d.password.as_deref().unwrap_or("None"));
            let bytes = credentials
                .chars()
                .map(|c| u8::try_from(u32::from(c)).map_err(|_| ()))
                .collect::<Result<Vec<_>, _>>()?;
            headers.push((
                "Authorization".into(),
                format!("Basic {}", STANDARD.encode(bytes)),
            ));
        }
    }
    if d.flag("image", true) {
        headers.push((
            "X-Icon".into(),
            if d.q("avatar_url").is_empty() {
                image(severity)
            } else {
                decode(d.q("avatar_url"))?
            },
        ));
    }
    if d.format("text") == "markdown" {
        headers.push(("X-Markdown".into(), "yes".into()));
    }
    let priority = prefix(
        d.q("priority"),
        &[
            ("l", "low"),
            ("mo", "low"),
            ("n", "default"),
            ("h", "high"),
            ("e", "max"),
            ("mi", "min"),
            ("ma", "max"),
            ("d", "default"),
            ("1", "min"),
            ("2", "low"),
            ("3", "default"),
            ("4", "high"),
            ("5", "max"),
        ],
        "default",
    );
    if priority != "default" {
        headers.push(("X-Priority".into(), priority.into()));
    }
    for (q, h) in [
        ("delay", "X-Delay"),
        ("email", "X-Email"),
        ("actions", "X-Actions"),
    ] {
        if !d.q(q).is_empty() {
            headers.push((h.into(), decode(d.q(q))?));
        }
    }
    if !d.q("click").is_empty() {
        let value = decode(d.q("click"))?;
        let encoded = value
            .bytes()
            .map(|c| {
                if c.is_ascii_alphanumeric() || b"-._~:/?@&=#".contains(&c) {
                    (c as char).to_string()
                } else {
                    format!("%{c:02X}")
                }
            })
            .collect::<String>();
        headers.push(("X-Click".into(), encoded));
    }
    let tags = if d.q("xtags").is_empty() {
        d.q("tags")
    } else {
        d.q("xtags")
    };
    if !tags.is_empty() {
        headers.push(("X-Tags".into(), list(&decode(tags)?).join(",")));
    }
    for topic in topics.into_iter().rev() {
        let mut payload = json!({"topic":topic,"title":"Catabolic","message":body});
        if !d.q("attach").is_empty() {
            payload["attach"] = decode(d.q("attach"))?.into();
            if !d.q("filename").is_empty() {
                payload["filename"] = decode(d.q("filename"))?
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .into();
            }
        }
        if catabolic_core::encode_with(&payload, true, false).len() > 8000 {
            return Err(());
        }
        plan.request(url.clone(), Some(payload), headers.clone(), &[200], false);
    }
    Ok(())
}
fn prefix<'a>(s: &str, map: &[(&str, &'a str)], default: &'a str) -> &'a str {
    let s = s.to_ascii_lowercase();
    map.iter()
        .find(|(k, _)| s.starts_with(k))
        .map(|(_, v)| *v)
        .unwrap_or(default)
}
fn gotify(d: &Destination, body: &str, plan: &mut Plan) -> Result<(), ()> {
    let secret = d.path.last().filter(|s| token(s)).ok_or(())?;
    let mut url = d.origin()?;
    url.push('/');
    for part in &d.path[..d.path.len() - 1] {
        url.push_str(&encode(part));
        url.push('/');
    }
    url.push_str("message");
    let priority = prefix(
        d.q("priority"),
        &[
            ("l", "1"),
            ("m", "3"),
            ("n", "5"),
            ("h", "8"),
            ("e", "10"),
            ("10", "10"),
            ("0", "1"),
            ("1", "1"),
            ("2", "1"),
            ("3", "3"),
            ("4", "3"),
            ("5", "5"),
            ("6", "5"),
            ("7", "5"),
            ("8", "8"),
            ("9", "8"),
        ],
        "5",
    )
    .parse::<i32>()
    .unwrap();
    let mut payload = json!({"priority":priority,"title":"Catabolic","message":body});
    if d.format("text") == "markdown" {
        payload["extras"] = json!({"client::display":{"contentType":"text/markdown"}});
    }
    plan.request(
        url,
        Some(payload),
        vec![
            ("Content-Type".into(), "application/json".into()),
            ("X-Gotify-Key".into(), secret.clone()),
        ],
        &[200],
        false,
    );
    Ok(())
}
fn discord(d: &Destination, body: &str, severity: &str, plan: &mut Plan) -> Result<(), ()> {
    let secret = d.path.first().filter(|s| token(s)).ok_or(())?;
    if !token(&d.host) {
        return Err(());
    }
    let mut url = format!(
        "https://discord.com/api/webhooks/{}/{}",
        encode(&d.host),
        encode(secret)
    );
    if !d.q("thread").is_empty() {
        url.push_str(&format!("?thread_id={}", encode(&decode(d.q("thread"))?)));
    }
    let tts = d.flag("tts", false);
    let mut payload = json!({"tts":tts,"wait":!tts});
    if d.flag("avatar", true) {
        payload["avatar_url"] = if d.q("avatar_url").is_empty() {
            image(severity)
        } else {
            decode(d.q("avatar_url"))?
        }
        .into();
    }
    let username = if d.query.contains_key("botname") {
        Some(decode(d.q("botname"))?)
    } else {
        d.user.clone()
    };
    if let Some(user) = username.filter(|u| !u.is_empty()) {
        payload["username"] = user.into();
    }
    if !d.q("flags").is_empty() {
        let flags = d.q("flags").parse::<u64>().map_err(|_| ())?;
        if flags > 0 {
            payload["flags"] = flags.into();
        }
    }
    let format = if !d.q("thread").is_empty()
        || (!d.query.contains_key("href") && d.query.contains_key("url"))
    {
        "markdown".into()
    } else {
        d.format("text")
    };
    if format == "markdown" {
        let color = match severity {
            "warning" => 0xCACF29,
            "error" => 0xA32037,
            _ => 0x3AA3E3,
        };
        let mut embed = json!({"author":{"name":"Apprise","url":"https://github.com/caronc/apprise"},"title":"Catabolic","description":body,"color":color});
        let href = if d.query.contains_key("href") {
            d.q("href")
        } else {
            d.q("url")
        };
        if !href.is_empty() {
            embed["url"] = decode(href)?.into();
        }
        if d.flag("footer", false) {
            embed["footer"] = json!({"text":"Apprise Notifications"});
            if d.flag("footer_logo", true) {
                embed["footer"]["icon_url"] = format!("{IMAGE_BASE}/apprise-logo.png").into();
            }
        }
        if d.flag("image", false) {
            embed["thumbnail"] = json!({"url":image(severity),"height":256,"width":256});
        }
        payload["embeds"] = json!([embed]);
    } else {
        payload["content"] = format!("Catabolic\r\n{body}").into();
    }
    if !d.q("ping").is_empty() {
        apply_ping(&mut payload, &decode(d.q("ping"))?);
    }
    plan.request(
        url,
        Some(payload),
        vec![(
            "Content-Type".into(),
            "application/json; charset=utf-8".into(),
        )],
        &[200, 204],
        true,
    );
    Ok(())
}
fn apply_ping(payload: &mut Value, ping: &str) {
    let mut users = vec![];
    let mut roles = vec![];
    let mut parse = vec![];
    for s in list(ping) {
        if s == "@everyone" || s == "@here" {
            parse.push(s[1..].to_string());
        } else if let Some(inner) = s.strip_prefix("<@").and_then(|s| s.strip_suffix('>')) {
            let role = inner.starts_with('&');
            let id = inner.trim_start_matches(['&', '!']);
            if !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit()) {
                if role {
                    roles.push(id.to_owned())
                } else {
                    users.push(id.to_owned())
                }
            }
        }
    }
    if users.is_empty() && roles.is_empty() && parse.is_empty() {
        return;
    }
    payload["allow_mentions"] = json!({"parse":parse,"users":users,"roles":roles});
    let mentions = parse
        .iter()
        .map(|s| format!("@{s}"))
        .chain(roles.iter().map(|s| format!("<@&{s}>")))
        .chain(users.iter().map(|s| format!("<@{s}>")))
        .collect::<Vec<_>>()
        .join(" ");
    payload["content"] =
        format!("{}👉 {mentions}", payload["content"].as_str().unwrap_or("")).into();
}
fn telegram(d: &Destination, body: &str, plan: &mut Plan) -> Result<(), ()> {
    if d.flag("image", false) {
        return Err(());
    } // Bundled image upload is an explicit compatibility gap.
    if !d.host.bytes().all(|c| c.is_ascii_digit()) {
        return Err(());
    }
    let secret = d
        .path
        .first()
        .filter(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
        })
        .ok_or(())?;
    let base = format!("https://api.telegram.org/bot{}:{secret}", d.host);
    if !d.q("content").is_empty()
        && !["before", "after"].contains(&d.q("content").to_ascii_lowercase().as_str())
    {
        return Err(());
    }
    let topic = if !d.q("topic").is_empty() {
        d.q("topic")
    } else {
        d.q("thread")
    };
    let topic = if topic.is_empty() {
        None
    } else {
        Some(topic.parse::<i64>().map_err(|_| ())?)
    };
    let format = d.format("html");
    let (mode, text) = if format == "markdown" {
        let mdv = prefix(
            d.q("mdv"),
            &[
                ("v2", "MarkdownV2"),
                ("2", "MarkdownV2"),
                ("v1", "MARKDOWN"),
                ("1", "MARKDOWN"),
            ],
            "MARKDOWN",
        );
        (mdv, format!("Catabolic\r\n{body}"))
    } else if format == "html" {
        ("HTML", format!("<b>Catabolic</b>\r\n{body}"))
    } else {
        ("HTML", format!("Catabolic\r\n{body}"))
    };
    let mut payload = json!({"disable_notification":d.flag("silent",false),"disable_web_page_preview":!d.flag("preview",false),"parse_mode":mode,"text":text});
    if let Some(topic) = topic.filter(|t| *t != 0) {
        payload["message_thread_id"] = topic.into();
    }
    let mut targets = list(&d.path[1..].join("/"));
    targets.extend(list(d.q("to")));
    targets.sort();
    targets.dedup();
    let explicit = !targets.is_empty();
    for target in targets {
        let (chat, own_topic) = target
            .rsplit_once(':')
            .map(|(a, b)| (a, Some(b)))
            .unwrap_or((&target, None));
        let mut p = payload.clone();
        let digits = chat.strip_prefix('-').unwrap_or(chat);
        if !digits.is_empty() && digits.len() <= 32 && digits.bytes().all(|c| c.is_ascii_digit()) {
            // JSON arbitrary precision is already enabled in the workspace.
            let normalized = chat
                .parse::<num_bigint::BigInt>()
                .map_err(|_| ())?
                .to_string();
            p["chat_id"] = serde_json::from_str(&normalized).map_err(|_| ())?;
        } else {
            let name = chat.strip_prefix('@').unwrap_or(chat);
            if name.len() < 2
                || !name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
                || !name
                    .as_bytes()
                    .first()
                    .is_some_and(|c| c.is_ascii_alphabetic() || b"_-".contains(c))
            {
                continue;
            }
            p["chat_id"] = format!("@{name}").into();
        }
        if let Some(topic) = own_topic {
            let Ok(topic) = topic.parse::<u64>() else {
                continue;
            };
            if topic > 0 {
                p["message_thread_id"] = topic.into();
            } else {
                p.as_object_mut().unwrap().remove("message_thread_id");
            }
        }
        plan.request(
            format!("{base}/sendMessage"),
            Some(p),
            vec![("Content-Type".into(), "application/json".into())],
            &[200],
            false,
        );
    }
    if !explicit && d.flag("detect", true) {
        plan.discovery = Some(Request {
            url: format!("{base}/getUpdates"),
            body: None,
            headers: vec![("Content-Type".into(), "application/json".into())],
            codes: vec![200],
            discord_retry: false,
        });
        plan.telegram_payload = Some(payload);
    }
    Ok(())
}
fn pause(duration: Duration, deadline: Instant) -> bool {
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        return false;
    };
    if duration >= remaining {
        return false;
    }
    if !duration.is_zero() {
        std::thread::sleep(duration);
    }
    true
}
fn send(
    request: &Request,
    plan: &Plan,
    deadline: Instant,
    read_body: bool,
) -> Result<(u16, Value, Option<Duration>), ()> {
    if !read_body {
        if let Some(last) = plan.last_request.get() {
            let wait = plan.interval.saturating_sub(last.elapsed());
            if !pause(wait, deadline) {
                return Err(());
            }
        }
        plan.last_request.set(Some(Instant::now()));
    }
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or(())?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .max_idle_connections(0)
        .max_response_header_size(16384)
        .http_status_as_error(false)
        .timeout_global(Some(remaining))
        .timeout_connect(Some(plan.connect.min(remaining)))
        .timeout_resolve(Some(plan.connect.min(remaining)))
        .timeout_recv_response(Some(plan.read.min(remaining)))
        .timeout_recv_body(Some(plan.read.min(remaining)))
        .build()
        .into();
    let mut outgoing = agent.post(&request.url).header("User-Agent", "Apprise");
    for (k, v) in &request.headers {
        let bytes = v
            .chars()
            .map(|c| u8::try_from(u32::from(c)).map_err(|_| ()))
            .collect::<Result<Vec<_>, _>>()?;
        let mut value = ureq::http::HeaderValue::from_bytes(&bytes).map_err(|_| ())?;
        if k.eq_ignore_ascii_case("Authorization") || k.eq_ignore_ascii_case("X-Gotify-Key") {
            value.set_sensitive(true);
        }
        outgoing = outgoing.header(k, value);
    }
    let mut response = match &request.body {
        Some(body) => outgoing.send(catabolic_core::encode_with(body, true, false).as_bytes()),
        None => outgoing.send_empty(),
    }
    .map_err(|_| ())?;
    let status = response.status().as_u16();
    let delay = response
        .headers()
        .get("X-RateLimit-Remaining")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v <= 0.0)
        .and_then(|_| response.headers().get("X-RateLimit-Reset"))
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .map(|v| Duration::from_secs((v - chrono::Utc::now().timestamp() + 1).max(0) as u64));
    let body = if read_body {
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(RESPONSE_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ())?;
        if bytes.len() > RESPONSE_LIMIT {
            return Err(());
        }
        catabolic_core::decode_json_bytes(&bytes).map_err(|_| ())?
    } else {
        Value::Null
    };
    Ok((status, body, delay))
}
fn execute(plan: &mut Plan, deadline: Instant) -> Outcome {
    if let Some(discovery) = &plan.discovery {
        let Ok((200, response, _)) = send(discovery, plan, deadline, true) else {
            return Outcome::TemporarilyFailed;
        };
        let Some(id) = response["result"]
            .as_array()
            .filter(|_| response["ok"] == true)
            .and_then(|entries| entries.iter().find_map(|v| v["message"]["from"].get("id")))
            .filter(|v| v.as_i64().is_some_and(|v| v != 0))
        else {
            return Outcome::TemporarilyFailed;
        };
        let mut payload = plan.telegram_payload.clone().unwrap();
        payload["chat_id"] = id.to_string().into();
        let url = discovery.url.trim_end_matches("getUpdates").to_owned() + "sendMessage";
        plan.request(
            url,
            Some(payload),
            vec![("Content-Type".into(), "application/json".into())],
            &[200],
            false,
        );
        plan.discovery = None;
    }
    if plan.requests.is_empty() {
        return Outcome::TemporarilyFailed;
    }
    let mut complete = true;
    for request in &plan.requests {
        let mut result = send(request, plan, deadline, false);
        if let Ok((429, _, delay)) = &result
            && request.discord_retry
            && pause(delay.unwrap_or(Duration::ZERO), deadline)
        {
            result = send(request, plan, deadline, false);
        }
        if !result.is_ok_and(|(status, _, _)| request.codes.contains(&status)) {
            complete = false;
        }
        if Instant::now() >= deadline {
            return Outcome::TemporarilyFailed;
        }
    }
    if complete {
        Outcome::Complete
    } else {
        Outcome::TemporarilyFailed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::{TcpListener, TcpStream},
        sync::{Arc, Mutex},
    };
    type Captures = Arc<Mutex<Vec<(String, BTreeMap<String, String>, Vec<u8>)>>>;
    fn fixture(
        replies: Vec<(u16, &'static str, &'static str)>,
    ) -> (String, Captures, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let captures = Arc::new(Mutex::new(vec![]));
        let result = captures.clone();
        let handle = std::thread::spawn(move || {
            for (status, extra, body) in replies {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let first = line.trim().to_string();
                let mut headers = BTreeMap::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    let (k, v) = line.split_once(':').unwrap();
                    headers.insert(k.to_ascii_lowercase(), v.trim().into());
                }
                let n = headers
                    .get("content-length")
                    .and_then(|v: &String| v.parse::<usize>().ok())
                    .unwrap_or(0);
                assert!(n <= 16384);
                let mut raw = vec![0; n];
                reader.read_exact(&mut raw).unwrap();
                result.lock().unwrap().push((first, headers, raw));
                write!(reader.get_mut(),"HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",body.len()).unwrap();
            }
        });
        (url, captures, handle)
    }
    fn loopback_plan(plan: &mut Plan, base: &str) {
        for request in plan.requests.iter_mut().chain(plan.discovery.iter_mut()) {
            let (_, rest) = request.url.split_once("://").unwrap();
            let path = rest.find('/').map(|i| &rest[i..]).unwrap_or("/");
            request.url = format!("{base}{path}");
        }
    }
    #[test]
    fn frozen_apprise_provider_wire_contracts() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../tests/reference/provider_wire.json")).unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            assert_eq!(case["added"], true);
            assert_eq!(case["complete"], true);
            let plan = prepare(
                case["url"].as_str().unwrap(),
                message("job_completed").unwrap(),
                case["severity"].as_str().unwrap(),
            )
            .unwrap_or_else(|_| panic!("case rejected: {}", case["url"]));
            let native: Vec<_> = plan
                .requests
                .iter()
                .map(|r| {
                    let mut headers: BTreeMap<_, _> = r.headers.iter().cloned().collect();
                    headers.insert("User-Agent".into(), "Apprise".into());
                    json!({"url":r.url,"headers":headers,"body":r.body})
                })
                .collect();
            assert_eq!(
                json!(native),
                case["requests"],
                "{} {}",
                case["url"],
                case["severity"]
            );
        }
    }
    #[test]
    fn owned_ntfy_and_gotify_delivery_wire_and_status() {
        for scheme in ["ntfy", "gotify"] {
            for status in [200, 201, 204, 302, 400, 429, 500] {
                let (base, captures, handle) =
                    fixture(vec![(status, "Location: http://127.0.0.1:1/leak\r\n", "")]);
                let url = base.replacen("http", scheme, 1) + "/synthetic";
                let outcome = deliver(&url, "scan_failed", "error");
                assert_eq!(
                    outcome,
                    if status == 200 {
                        Outcome::Complete
                    } else {
                        Outcome::TemporarilyFailed
                    }
                );
                handle.join().unwrap();
                let captures = captures.lock().unwrap();
                assert_eq!(captures.len(), 1);
                let (_, headers, raw) = &captures[0];
                assert_eq!(headers["user-agent"], "Apprise");
                let body: Value = serde_json::from_slice(raw).unwrap();
                assert_eq!(body["title"], "Catabolic");
                assert_eq!(body["message"], message("scan_failed").unwrap());
                if scheme == "ntfy" {
                    assert_eq!(body["topic"], "synthetic");
                    assert!(headers["x-icon"].contains("failure"));
                } else {
                    assert_eq!(headers["x-gotify-key"], "synthetic");
                }
            }
        }
    }
    #[test]
    fn fixed_endpoint_http_providers_use_same_transport_and_retry_budget() {
        let (base, captures, handle) = fixture(vec![(429, "", ""), (204, "", "")]);
        let mut plan = prepare(
            "discord://123/synthetic?avatar=no",
            message("job_completed").unwrap(),
            "info",
        )
        .unwrap();
        loopback_plan(&mut plan, &base);
        assert_eq!(
            execute(&mut plan, Instant::now() + Duration::from_secs(2)),
            Outcome::Complete
        );
        handle.join().unwrap();
        let captures = captures.lock().unwrap();
        assert_eq!(captures.len(), 2);
        assert_eq!(captures[0].2, captures[1].2);
        assert_eq!(captures[0].0, "POST /api/webhooks/123/synthetic HTTP/1.1");
        drop(captures);
        let (base, captures, handle) = fixture(vec![
            (
                200,
                "",
                r#"{"ok":true,"result":[{"message":{"from":{"id":123}}}]}"#,
            ),
            (200, "", ""),
        ]);
        let mut plan = prepare(
            "tgram://123:synthetic",
            message("job_failed").unwrap(),
            "error",
        )
        .unwrap();
        loopback_plan(&mut plan, &base);
        assert_eq!(
            execute(&mut plan, Instant::now() + Duration::from_secs(2)),
            Outcome::Complete
        );
        handle.join().unwrap();
        let captures = captures.lock().unwrap();
        assert!(captures[0].2.is_empty());
        let body: Value = serde_json::from_slice(&captures[1].2).unwrap();
        assert_eq!(body["chat_id"], "123");
        assert!(body["text"].as_str().unwrap().contains("failed"));
    }
    #[test]
    fn fanout_keeps_partial_failure_and_static_body() {
        let (base, captures, handle) = fixture(vec![(500, "", ""), (200, "", "")]);
        let url = base.replacen("http", "ntfy", 1) + "/alpha/zeta?image=no";
        assert_eq!(
            deliver(&url, "consumer_needs_repair", "warning"),
            Outcome::TemporarilyFailed
        );
        handle.join().unwrap();
        let captures = captures.lock().unwrap();
        assert_eq!(captures.len(), 2);
        assert_eq!(
            serde_json::from_slice::<Value>(&captures[0].2).unwrap()["topic"],
            "zeta"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&captures[1].2).unwrap()["topic"],
            "alpha"
        );
    }
    #[test]
    fn boolean_aliases_preserve_silent_intent_and_fail_closed_controls() {
        for value in [
            "t", "y", "allow", "always", "enabled", "true", "yes", "on", "1",
        ] {
            let plan = prepare(
                &format!("tgram://123:synthetic/456?silent={value}"),
                message("job_completed").unwrap(),
                "info",
            )
            .unwrap();
            assert_eq!(
                plan.requests[0].body.as_ref().unwrap()["disable_notification"],
                true
            );
            assert!(
                prepare(
                    &format!("tgram://123:synthetic/456?image={value}"),
                    message("job_completed").unwrap(),
                    "info"
                )
                .is_err()
            );
            assert!(
                prepare(
                    &format!("ntfy://127.0.0.1/topic?redirect={value}"),
                    message("job_completed").unwrap(),
                    "info"
                )
                .is_err()
            );
        }
        for value in ["f", "n", "never", "disable", "deny", "false", "off", "0"] {
            assert!(
                prepare(
                    &format!("ntfy://127.0.0.1/topic?verify={value}"),
                    message("job_completed").unwrap(),
                    "info"
                )
                .is_err()
            );
        }
        let plan = prepare(
            "gotify://127.0.0.1/key?wait=0",
            message("job_completed").unwrap(),
            "info",
        )
        .unwrap();
        assert!(plan.wait.is_zero());
    }

    #[test]
    fn query_auth_and_port_overrides_reach_only_the_selected_owned_receiver() {
        let (base, captures, handle) = fixture(vec![(200, "", "")]);
        let port = base.rsplit(':').next().unwrap();
        let url = format!(
            "ntfy://old:wrong@127.0.0.1:1/topic?user=new&password=first&pass=synthetic&port={port}&image=no"
        );
        assert_eq!(deliver(&url, "job_completed", "info"), Outcome::Complete);
        handle.join().unwrap();
        assert_eq!(
            captures.lock().unwrap()[0].1["authorization"],
            format!("Basic {}", STANDARD.encode("new:synthetic"))
        );
    }

    #[test]
    fn inherited_retry_optional_and_fail_closed_options() {
        let (base, captures, handle) = fixture(vec![(503, "", ""), (200, "", "")]);
        assert_eq!(
            deliver(
                &(base.replacen("http", "gotify", 1) + "/synthetic?retry=1"),
                "job_completed",
                "info"
            ),
            Outcome::Complete
        );
        handle.join().unwrap();
        assert_eq!(captures.lock().unwrap().len(), 2);
        let (base, _, handle) = fixture(vec![(500, "", "")]);
        assert_eq!(
            deliver(
                &(base.replacen("http", "gotify", 1) + "/synthetic?optional=yes"),
                "job_completed",
                "info"
            ),
            Outcome::Complete
        );
        handle.join().unwrap();
        for url in [
            "file:///tmp/private",
            "ntfy://127.0.0.1/topic?verify=no",
            "ntfy://127.0.0.1/topic?redirect=yes",
            "discord://123/token?template=/tmp/private",
            "tgram://123:token/456?image=yes",
            "gotify://127.0.0.1/key%0Asecret",
            "ntfy://user:pass@127.0.0.1/topic?token=abc%0D%0Aheader",
            "ntfy://127.0.0.1:0/topic",
            "ntfy://127.0.0.1/topic#secret",
        ] {
            assert!(
                prepare(url, message("job_completed").unwrap(), "info").is_err(),
                "{url}"
            );
        }
    }
    #[test]
    fn response_size_and_request_deadline_are_bounded() {
        let huge = "x".repeat(RESPONSE_LIMIT + 1);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0; 4096];
            assert!(stream.read(&mut buf).unwrap() > 0);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                huge.len(),
                huge
            );
        });
        let mut plan = prepare(
            "tgram://123:synthetic",
            message("job_completed").unwrap(),
            "info",
        )
        .unwrap();
        loopback_plan(&mut plan, &base);
        assert_eq!(
            execute(&mut plan, Instant::now() + Duration::from_secs(2)),
            Outcome::TemporarilyFailed
        );
        handle.join().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(150));
        });
        let mut plan = prepare(
            &(base.replacen("http", "gotify", 1) + "/synthetic"),
            message("job_completed").unwrap(),
            "info",
        )
        .unwrap();
        let start = Instant::now();
        assert_eq!(
            execute(&mut plan, start + Duration::from_millis(50)),
            Outcome::TemporarilyFailed
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        handle.join().unwrap();
        assert!(TcpStream::connect(base.trim_start_matches("http://")).is_err());
    }
}
