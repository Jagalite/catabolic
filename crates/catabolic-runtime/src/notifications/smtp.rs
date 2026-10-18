// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Bounded native SMTP delivery. Secrets and server replies never leave this
//! module. The only background work is bounded DNS resolution, never delivery.
use base64::{Engine, engine::general_purpose::STANDARD};
use catabolic_store::notifications::Outcome;
use hmac::{Hmac, Mac};
use md5::Md5;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned, pki_types::ServerName};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const MAX_URL: usize = 16384;
const MAX_RECIPIENTS: usize = 100;
const MAX_REPLY: usize = 65536;
const MAX_LINE: usize = 4096;
const MAX_OUTPUT: usize = 262144;
const MAX_CHALLENGES: usize = 5;
const MAX_HEADERS: usize = 32;
const MAX_HEADER_NAME: usize = 64;
const MAX_HEADER_VALUE: usize = 512;
static RESOLVERS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Plain,
    StartTls,
    Tls,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Mailbox {
    name: String,
    address: String,
}

struct Settings {
    host: String,
    port: u16,
    mode: Mode,
    user: String,
    password: String,
    from: Mailbox,
    to: Vec<Mailbox>,
    cc: Vec<Mailbox>,
    bcc: Vec<Mailbox>,
    reply: Vec<Mailbox>,
    html: bool,
    timeout: Duration,
    retry: u32,
    wait: Duration,
    optional: bool,
    // Apprise `+Name=value` headers, in first-seen order (exact-name upsert).
    headers: Vec<(String, String)>,
    // Explicit Date-header zone; None keeps the reference's local-time default.
    tz: Option<chrono_tz::Tz>,
}

type Parsed<T> = Result<T, ()>;

fn decode(value: &str) -> Parsed<String> {
    let mut result = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let digit = |x: u8| (x as char).to_digit(16).map(|x| x as u8).ok_or(());
            if index + 2 >= bytes.len() {
                return Err(());
            }
            result.push(digit(bytes[index + 1])? * 16 + digit(bytes[index + 2])?);
            index += 3;
        } else {
            result.push(bytes[index]);
            index += 1;
        }
    }
    let value = String::from_utf8(result).map_err(|_| ())?;
    if value.chars().any(char::is_control) {
        return Err(());
    }
    Ok(value)
}

// Headers whose duplication or override would change addressing, routing,
// threading or MIME structure. Apprise appends its own copies after custom
// headers, producing duplicate fields; native rejects them instead. Only the
// exact name `X-Application` may be replaced, as in the reference.
fn reserved_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "from"
            | "to"
            | "cc"
            | "bcc"
            | "reply-to"
            | "subject"
            | "date"
            | "message-id"
            | "mime-version"
            | "sender"
            | "return-path"
            | "received"
            | "autocrypt"
            | "x-application"
    ) || name.starts_with("content-")
        || name.starts_with("resent-")
}

// Apprise stores `+Name=value` query keys unmodified apart from `+` -> space
// in everything after the first character, then percent-decodes names and
// values a second time. Returns None for ordinary options.
fn custom_header(raw_name: &str, raw_value: &str) -> Parsed<Option<(String, String)>> {
    if !(raw_name.starts_with('+') || raw_name.starts_with("%2B") || raw_name.starts_with("%2b")) {
        return Ok(None);
    }
    let first = raw_name.chars().next().ok_or(())?;
    let key = decode(&format!(
        "{first}{}",
        raw_name[first.len_utf8()..].replace('+', " ")
    ))?;
    let Some(name) = key.strip_prefix('+') else {
        return Err(());
    };
    let name = decode(name)?;
    let value = decode(decode(raw_value)?.trim())?;
    if name.is_empty()
        || name.len() > MAX_HEADER_NAME
        || !name.bytes().all(|c| (33..=126).contains(&c) && c != b':')
        || value.len() > MAX_HEADER_VALUE
        || (reserved_header(&name) && name != "X-Application")
    {
        return Err(());
    }
    Ok(Some((name, value)))
}

const UTC_ALIASES: &[&str] = &["utc", "z", "gmt", "etc/utc", "etc/gmt", "gmt0", "utc0"];

// Apprise's forgiving `zoneinfo()`: UTC aliases, an exact IANA key, a
// case-insensitive key, then a trailing path suffix after the region
// (`Tokyo`, `Argentina/Buenos_Aires`). The reference scans an unordered set,
// so a suffix that names zones whose UTC offsets ever differ is rejected here
// rather than guessed. Offsets are sampled across a year.
fn resolve_zone(name: &str) -> Parsed<chrono_tz::Tz> {
    let name = name.trim();
    let lowered = name.to_lowercase();
    if name.is_empty() || name.len() > 64 {
        return Err(());
    }
    if UTC_ALIASES.contains(&lowered.as_str()) {
        return Ok(chrono_tz::UTC);
    }
    if let Some(zone) = chrono_tz::TZ_VARIANTS.iter().find(|z| z.name() == name) {
        return Ok(*zone);
    }
    if let Some(zone) = chrono_tz::TZ_VARIANTS
        .iter()
        .find(|z| z.name().to_lowercase() == lowered)
    {
        return Ok(*zone);
    }
    let matches: Vec<_> = chrono_tz::TZ_VARIANTS
        .iter()
        .filter(|zone| {
            let lower = zone.name().to_lowercase();
            let parts: Vec<_> = lower.split('/').skip(1).take(2).collect();
            !parts.is_empty() && parts.join("/") == lowered
        })
        .collect();
    let first = **matches.first().ok_or(())?;
    let now = chrono::Utc::now();
    let offset = |zone: chrono_tz::Tz, days: i64| {
        use chrono::Offset;
        (now + chrono::Duration::days(days))
            .with_timezone(&zone)
            .offset()
            .fix()
    };
    for days in [0, 91, 182, 273] {
        if matches
            .iter()
            .any(|z| offset(**z, days) != offset(first, days))
        {
            return Err(());
        }
    }
    Ok(first)
}

fn boolean(value: &str) -> Parsed<bool> {
    Ok(super::boolean(value))
}

fn host_valid(host: &str) -> bool {
    host.parse::<IpAddr>().is_ok()
        || (!host.is_empty()
            && host.len() <= 253
            && host.trim_end_matches('.').split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            }))
}

fn mailbox(value: &str) -> Option<Mailbox> {
    let value = value.trim();
    let (name, address) = if let Some((name, rest)) = value.split_once('<') {
        (
            name.trim().trim_matches(['"', '\'']),
            rest.strip_suffix('>')?.trim(),
        )
    } else if let Some((name, address)) = value.rsplit_once(':') {
        (name.trim().trim_matches(['"', '\'']), address.trim())
    } else if let Some((name, address)) = value.rsplit_once(char::is_whitespace) {
        if name.contains('@') {
            return None;
        }
        (name.trim().trim_matches(['"', '\'']), address.trim())
    } else {
        ("", value)
    };
    let (local, domain) = address.rsplit_once('@')?;
    if local.is_empty()
        || local.len() > 64
        || address.len() > 254
        || name.len() > 256
        || !local
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&c))
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
        || !host_valid(domain)
        || if let Some((_, last)) = domain.rsplit_once('.') {
            last.len() < 2
        } else {
            domain.len() < 6
        }
        || name.chars().any(char::is_control)
    {
        return None;
    }
    Some(Mailbox {
        name: name.to_owned(),
        address: address.to_owned(),
    })
}

fn mailboxes(value: &str) -> Vec<Mailbox> {
    // Respect quoted display names and angle brackets when splitting lists.
    let mut result = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut bracketed = false;
    let mut escaped = false;
    for (index, c) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' && quoted {
            escaped = true;
            continue;
        }
        if c == '"' {
            quoted = !quoted;
        }
        if !quoted {
            if c == '<' {
                bracketed = true;
            }
            if c == '>' {
                bracketed = false;
            }
            if matches!(c, ',' | ';') && !bracketed {
                if let Some(address) = mailbox(&value[start..index]) {
                    result.push(address);
                }
                start = index + c.len_utf8();
            }
        }
    }
    let rest = &value[start..];
    if let Some(address) = mailbox(rest) {
        result.push(address);
    } else if !rest.contains(['<', '>', ':', '"']) {
        result.extend(rest.split_whitespace().filter_map(mailbox));
    }
    result
}

fn unique(list: &mut Vec<Mailbox>) {
    let mut seen = std::collections::HashSet::new();
    list.retain(|item| seen.insert(item.address.clone()));
}

// Service defaults are URL behavior, not live account configuration. Explicit
// smtp= bypasses them, exactly as in the pinned adapter.
fn service_default(domain: &str) -> Option<(&'static str, Mode, bool)> {
    let domain = domain.to_ascii_lowercase();
    let in_family = |prefix: &str, suffixes: &[&str]| {
        domain
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffixes.contains(&suffix))
    };
    if domain == "gmail.com" {
        Some(("smtp.gmail.com", Mode::StartTls, true))
    } else if in_family(
        "yandex.",
        &["com", "ru", "ua", "by", "kz", "uz", "tr", "fr"],
    ) {
        Some(("smtp.yandex.ru", Mode::Tls, false))
    } else if ["hotmail.com", "hotmail.com.au", "live.com", "live.com.au"]
        .contains(&domain.as_str())
    {
        Some(("smtp-mail.outlook.com", Mode::StartTls, true))
    } else if [
        "outlook.com",
        "outlook.com.au",
        "smtp.outlook.com",
        "smtp.outlook.com.au",
    ]
    .contains(&domain.as_str())
    {
        Some(("smtp.outlook.com", Mode::StartTls, true))
    } else if ["office365.com", "smtp.office365.com"].contains(&domain.as_str()) {
        Some(("smtp.office365.com", Mode::StartTls, false))
    } else if in_family("yahoo.", &["ca", "com"]) {
        Some(("smtp.mail.yahoo.com", Mode::Tls, true))
    } else if in_family("gmx.", &["com", "net", "de", "at", "ch", "fr"]) {
        Some(("mail.gmx.com", Mode::StartTls, true))
    } else if in_family(
        "fastmail.",
        &[
            "com", "cn", "co.uk", "com.au", "de", "es", "fm", "fr", "im", "in", "jp", "mx", "net",
            "nl", "org", "se", "to", "tw", "uk", "us",
        ],
    ) || FASTMAIL_ALIASES.contains(&domain.as_str())
    {
        Some(("smtp.fastmail.com", Mode::Tls, true))
    } else if ["zoho.com", "zohomail.com"].contains(&domain.as_str()) {
        Some(("smtp.zoho.com", Mode::StartTls, true))
    } else if ["sendgrid.com", "sendgrid.net"].contains(&domain.as_str()) {
        Some(("smtp.sendgrid.net", Mode::Tls, false))
    } else if domain == "163.com" {
        Some(("smtp.163.com", Mode::Tls, true))
    } else if ["foxmail.com", "qq.com"].contains(&domain.as_str()) {
        Some(("smtp.qq.com", Mode::StartTls, true))
    } else if domain == "comcast.net" {
        Some(("smtp.comcast.net", Mode::Tls, true))
    } else {
        None
    }
}

const FASTMAIL_ALIASES: &[&str] = &[
    "123mail.org",
    "airpost.net",
    "eml.cc",
    "fmail.co.uk",
    "fmgirl.com",
    "fmguy.com",
    "mailbolt.com",
    "mailcan.com",
    "mailhaven.com",
    "mailmight.com",
    "ml1.net",
    "mm.st",
    "myfastmail.com",
    "proinbox.com",
    "promessage.com",
    "rushpost.com",
    "sent.as",
    "sent.at",
    "sent.com",
    "speedymail.org",
    "warpmail.net",
    "xsmail.com",
    "150mail.com",
    "150ml.com",
    "16mail.com",
    "2-mail.com",
    "4email.net",
    "50mail.com",
    "allmail.net",
    "bestmail.us",
    "cluemail.com",
    "elitemail.org",
    "emailcorner.net",
    "emailengine.net",
    "emailengine.org",
    "emailgroups.net",
    "emailplus.org",
    "emailuser.net",
    "f-m.fm",
    "fast-email.com",
    "fast-mail.org",
    "fastem.com",
    "fastemail.us",
    "fastemailer.com",
    "fastest.cc",
    "fastimap.com",
    "fastmailbox.net",
    "fastmessaging.com",
    "fea.st",
    "fmailbox.com",
    "ftml.net",
    "h-mail.us",
    "hailmail.net",
    "imap-mail.com",
    "imap.cc",
    "imapmail.org",
    "inoutbox.com",
    "internet-e-mail.com",
    "internet-mail.org",
    "internetemails.net",
    "internetmailing.net",
    "jetemail.net",
    "justemail.net",
    "letterboxes.org",
    "mail-central.com",
    "mail-page.com",
    "mailandftp.com",
    "mailas.com",
    "mailc.net",
    "mailforce.net",
    "mailftp.com",
    "mailingaddress.org",
    "mailite.com",
    "mailnew.com",
    "mailsent.net",
    "mailservice.ms",
    "mailup.net",
    "mailworks.org",
    "mymacmail.com",
    "nospammail.net",
    "ownmail.net",
    "petml.com",
    "postinbox.com",
    "postpro.net",
    "realemail.net",
    "reallyfast.biz",
    "reallyfast.info",
    "speedpost.net",
    "ssl-mail.com",
    "swift-mail.com",
    "the-fastest.net",
    "the-quickest.com",
    "theinternetemail.com",
    "veryfast.biz",
    "veryspeedy.net",
    "yepmail.net",
];

impl Settings {
    fn parse(url: &str) -> Parsed<Self> {
        if url.len() > MAX_URL || url.chars().any(char::is_control) || url.contains('\\') {
            return Err(());
        }
        let (scheme, rest) = url.split_once("://").ok_or(())?;
        let mut mode = match scheme.to_ascii_lowercase().as_str() {
            "mailto" => Mode::Plain,
            "mailtos" => Mode::StartTls,
            _ => return Err(()),
        };
        let rest = rest.strip_suffix('#').unwrap_or(rest);
        if rest.contains('#') {
            return Err(());
        }
        let (location, query) = rest.split_once('?').unwrap_or((rest, ""));
        let mut options = BTreeMap::new();
        let mut headers: Vec<(String, String)> = Vec::new();
        for pair in query.split(['&', ';']).filter(|s| !s.is_empty()) {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            if let Some((name, value)) = custom_header(name, value)? {
                match headers.iter_mut().find(|(known, _)| *known == name) {
                    Some(known) => known.1 = value,
                    None => headers.push((name, value)),
                }
                if headers.len() > MAX_HEADERS {
                    return Err(());
                }
                continue;
            }
            options.insert(
                decode(name)?.trim().to_ascii_lowercase(),
                decode(value)?.trim().to_owned(),
            );
        }
        for (key, value) in &options {
            match key.as_str() {
                "from" | "name" | "smtp" | "mode" | "to" | "cc" | "bcc" | "reply" | "user"
                | "password" | "pass" | "port" | "format" | "cto" | "rto" | "retry" | "wait"
                | "optional" | "tz" => (),
                "verify" if boolean(value)? => (),
                "redirect" | "wkd" | "store" if !boolean(value)? => (),
                "pgp"
                    if ["no", "none", "false", "off", "0", "n", ""]
                        .contains(&value.to_ascii_lowercase().as_str()) => {}
                "overflow" if ["upstream", "truncate", "split"].contains(&value.as_str()) => (),
                // No attachments or emoji-like sequences exist in safe event summaries.
                "inline" | "emojis" => {
                    boolean(value)?;
                }
                "tag" => (),
                _ => return Err(()),
            }
        }
        let (authority, path) = location.split_once('/').unwrap_or((location, ""));
        let (auth, authority) = authority
            .rsplit_once('@')
            .map(|(a, h)| (Some(a), h))
            .unwrap_or((None, authority));
        let authority_password_present = auth.is_some_and(|value| value.contains(':'));
        let (mut user, mut password) = match auth {
            Some(auth) => {
                let (u, p) = auth.split_once(':').unwrap_or((auth, ""));
                (decode(u)?, decode(p)?)
            }
            None => (String::new(), String::new()),
        };
        if let Some(new_user) = options.get("user") {
            if password.is_empty()
                && !authority_password_present
                && !options.contains_key("password")
                && !options.contains_key("pass")
            {
                password = user.clone();
            }
            user = decode(new_user)?;
        }
        if let Some(value) = options.get("password") {
            password = decode(value)?;
        }
        if let Some(value) = options.get("pass") {
            password = decode(value)?;
        }
        if user.len() > 4096 || password.len() > 4096 {
            return Err(());
        }
        let (raw_host, raw_port) = if let Some(rest) = authority.strip_prefix('[') {
            let (host, suffix) = rest.split_once(']').ok_or(())?;
            (
                host,
                if suffix.is_empty() {
                    None
                } else {
                    Some(suffix.strip_prefix(':').ok_or(())?)
                },
            )
        } else if let Some((host, port)) = authority.rsplit_once(':') {
            (host, Some(port))
        } else {
            (authority, None)
        };
        let mut domain = decode(raw_host)?.to_ascii_lowercase();
        if !host_valid(&domain) {
            return Err(());
        }
        let explicit_mode = options.get("mode").filter(|s| !s.is_empty());
        if let Some(value) = explicit_mode {
            mode = match value.to_ascii_lowercase().as_str() {
                "insecure" => Mode::Plain,
                "starttls" => Mode::StartTls,
                "ssl" => Mode::Tls,
                _ => return Err(()),
            };
        }
        let raw_port = options.get("port").map(String::as_str).or(raw_port);
        let mut port = raw_port
            .filter(|p| !p.is_empty())
            .map(|p| p.parse::<u16>().map_err(|_| ()))
            .transpose()?;
        if port == Some(0) {
            return Err(());
        }
        let explicit_smtp = options.get("smtp").filter(|s| !s.is_empty());
        let mut host = explicit_smtp
            .map(|h| decode(h))
            .transpose()?
            .unwrap_or_else(|| domain.clone());
        if explicit_smtp.is_none()
            && !user.is_empty()
            && let Some((service, default_mode, email_login)) = service_default(&domain)
        {
            host = service.into();
            if explicit_mode.is_none() {
                mode = default_mode;
            }
            if port.is_none() {
                port = Some(if default_mode == Mode::Tls { 465 } else { 587 });
            }
            if email_login {
                if let Some((_, user_domain)) = user.rsplit_once('@') {
                    domain = user_domain.to_owned();
                } else {
                    user = format!("{user}@{domain}");
                }
            } else if !["office365.com", "smtp.office365.com"].contains(&domain.as_str())
                && let Some((local, _)) = user.split_once('@')
            {
                user = local.rsplit('+').next().unwrap_or(local).to_owned();
            }
        }
        host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        if !host_valid(&host) {
            return Err(());
        }
        let mut from = if !user.is_empty() {
            Mailbox {
                name: String::new(),
                address: format!("{}@{domain}", user.split(['@', ' ']).next().ok_or(())?),
            }
        } else if explicit_smtp.is_none()
            && ["localhost", "localhost.localdomain"].contains(&domain.as_str())
        {
            mailbox(&format!("root@{domain}")).ok_or(())?
        } else {
            Mailbox {
                name: String::new(),
                address: String::new(),
            }
        };
        if !user.is_empty() {
            from.name = "Apprise".into();
        }
        if let Some(value) = options.get("from").filter(|s| !s.is_empty()) {
            let value = decode(value)?;
            if let Some(parsed) = mailbox(&value) {
                from = parsed;
            } else {
                from.name = value;
            }
        }
        if let Some(value) = options.get("name").filter(|s| !s.is_empty()) {
            from.name = decode(value)?;
        }
        if mailbox(&from.address).is_none() || from.name.len() > 256 {
            return Err(());
        }
        let mut to = Vec::new();
        let mut supplied_target = false;
        for target in path.split('/').filter(|s| !s.is_empty()) {
            supplied_target = true;
            to.extend(mailboxes(&decode(target)?));
        }
        if let Some(target) = options.get("to").filter(|s| !s.is_empty()) {
            supplied_target = true;
            to.extend(mailboxes(target));
        }
        if !supplied_target {
            to.push(Mailbox {
                name: String::new(),
                address: from.address.clone(),
            });
        }
        let list = |name| {
            let mut list = options.get(name).map(|v| mailboxes(v)).unwrap_or_default();
            unique(&mut list);
            list
        };
        let cc = list("cc");
        let bcc = list("bcc");
        let reply = list("reply");
        if to.len() + cc.len() + bcc.len() + reply.len() > MAX_RECIPIENTS {
            return Err(());
        }
        let html = match options
            .get("format")
            .map(String::as_str)
            .unwrap_or("html")
            .to_ascii_lowercase()
            .as_str()
        {
            "html" => true,
            "text" | "markdown" => false,
            _ => return Err(()),
        };
        // Pinned SMTP applies cto to the entire socket; rto is an HTTP option.
        let seconds = options
            .get("cto")
            .map(|v| v.parse::<f64>().map_err(|_| ()))
            .transpose()?
            .unwrap_or(15.0);
        if !seconds.is_finite() || seconds <= 0.0 {
            return Err(());
        }
        if let Some(value) = options.get("rto") {
            let seconds = value.parse::<f64>().map_err(|_| ())?;
            if !seconds.is_finite() || seconds <= 0.0 {
                return Err(());
            }
        }
        Ok(Self {
            host,
            port: port.unwrap_or(match mode {
                Mode::Plain => 25,
                Mode::StartTls => 587,
                Mode::Tls => 465,
            }),
            mode,
            user,
            password,
            from,
            to,
            cc,
            bcc,
            reply,
            html,
            timeout: Duration::from_secs_f64(seconds.min(30.0)),
            retry: options
                .get("retry")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0)
                .min(10),
            wait: Duration::from_secs_f64(
                options
                    .get("wait")
                    .and_then(|s| s.parse::<f64>().ok())
                    .filter(|s| s.is_finite() && *s >= 0.0)
                    .unwrap_or(0.5)
                    .min(20.0),
            ),
            optional: options
                .get("optional")
                .map(|s| boolean(s))
                .transpose()?
                .unwrap_or(false),
            headers,
            tz: options.get("tz").map(|v| resolve_zone(v)).transpose()?,
        })
    }
}

fn failed() -> io::Error {
    io::Error::other("notification delivery failed")
}
fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "notification deadline"))
}

fn resolve(host: &str, port: u16, deadline: Instant) -> io::Result<Vec<SocketAddr>> {
    remaining(deadline)?;
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    // getaddrinfo has no portable cancellation. Only address resolution may
    // outlive a deadline; no URL, password, message or socket is in this worker.
    // Four occupied slots fail closed rather than accumulating blocked threads.
    RESOLVERS
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < 4).then_some(n + 1)
        })
        .map_err(|_| failed())?;
    let (send, receive) = mpsc::sync_channel(1);
    let host = host.to_owned();
    let worker = std::thread::Builder::new()
        .name("notification-dns".into())
        .spawn(move || {
            struct Slot;
            impl Drop for Slot {
                fn drop(&mut self) {
                    RESOLVERS.fetch_sub(1, Ordering::Release);
                }
            }
            let _slot = Slot;
            let result = (host.as_str(), port)
                .to_socket_addrs()
                .map(|addresses| addresses.take(32).collect());
            let _ = send.send(result);
        });
    if worker.is_err() {
        RESOLVERS.fetch_sub(1, Ordering::Release);
        return Err(failed());
    }
    receive
        .recv_timeout(remaining(deadline)?)
        .map_err(|_| failed())?
}

struct DeadlineSocket {
    stream: TcpStream,
    deadline: Instant,
    timeout: Duration,
}
impl Read for DeadlineSocket {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stream
            .set_read_timeout(Some(remaining(self.deadline)?.min(self.timeout)))?;
        self.stream.read(buffer)
    }
}
impl Write for DeadlineSocket {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stream
            .set_write_timeout(Some(remaining(self.deadline)?.min(self.timeout)))?;
        self.stream.write(buffer)
    }
    fn flush(&mut self) -> io::Result<()> {
        remaining(self.deadline)?;
        self.stream.flush()
    }
}

enum Wire {
    Plain(DeadlineSocket),
    Tls(Box<StreamOwned<ClientConnection, DeadlineSocket>>),
}
impl Read for Wire {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(buffer),
            Self::Tls(s) => s.read(buffer),
        }
    }
}
impl Write for Wire {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(buffer),
            Self::Tls(s) => s.write(buffer),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(s) => s.flush(),
        }
    }
}

fn tls_config() -> Arc<ClientConfig> {
    let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Arc::new(
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("bundled TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

impl Wire {
    fn secure(self, host: &str, config: Arc<ClientConfig>) -> io::Result<Self> {
        let Self::Plain(mut socket) = self else {
            return Err(failed());
        };
        let name = ServerName::try_from(host.to_owned()).map_err(|_| failed())?;
        let mut connection = ClientConnection::new(config, name).map_err(|_| failed())?;
        while connection.is_handshaking() {
            connection.complete_io(&mut socket)?;
        }
        Ok(Self::Tls(Box::new(StreamOwned::new(connection, socket))))
    }
}

struct Session {
    wire: Wire,
    deadline: Instant,
    read_bytes: usize,
    written_bytes: usize,
}
struct Reply {
    code: u16,
    lines: Vec<String>,
}

impl Session {
    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        remaining(self.deadline)?;
        self.written_bytes = self
            .written_bytes
            .checked_add(bytes.len())
            .ok_or_else(failed)?;
        if self.written_bytes > MAX_OUTPUT {
            return Err(failed());
        }
        self.wire.write_all(bytes)?;
        self.wire.flush()
    }
    fn reply(&mut self) -> io::Result<Reply> {
        let mut lines = Vec::new();
        let mut expected = None;
        loop {
            remaining(self.deadline)?;
            let mut line = Vec::new();
            loop {
                let mut byte = [0];
                self.wire.read_exact(&mut byte)?;
                self.read_bytes += 1;
                if self.read_bytes > MAX_REPLY || line.len() >= MAX_LINE {
                    return Err(failed());
                }
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            if line.len() < 5
                || !line.ends_with(b"\r\n")
                || !line[..3].iter().all(u8::is_ascii_digit)
            {
                return Err(failed());
            }
            let code = u16::from(line[0] - b'0') * 100
                + u16::from(line[1] - b'0') * 10
                + u16::from(line[2] - b'0');
            if expected.is_some_and(|old| old != code)
                || !matches!(line[3], b' ' | b'-' | b'\r')
                || (line[3] == b'\r' && line.len() != 5)
            {
                return Err(failed());
            }
            expected = Some(code);
            let done = line[3] != b'-';
            lines.push(if line[3] == b'\r' {
                String::new()
            } else {
                String::from_utf8_lossy(&line[4..line.len() - 2]).into_owned()
            });
            if done {
                return Ok(Reply { code, lines });
            }
            if lines.len() >= 100 {
                return Err(failed());
            }
        }
    }
    fn command(&mut self, command: &str) -> io::Result<Reply> {
        if command.len() > 12000 || command.contains(['\r', '\n']) {
            return Err(failed());
        }
        self.send(format!("{command}\r\n").as_bytes())?;
        self.reply()
    }
    fn hello(&mut self) -> io::Result<Reply> {
        let reply = self.command("EHLO [127.0.0.1]")?;
        if reply.code == 250 {
            return Ok(reply);
        }
        let reply = self.command("HELO [127.0.0.1]")?;
        if reply.code != 250 {
            return Err(failed());
        }
        Ok(Reply {
            code: 250,
            lines: Vec::new(),
        })
    }
    fn authenticate(&mut self, settings: &Settings, hello: &Reply) -> io::Result<()> {
        if settings.user.is_empty() || settings.password.is_empty() {
            return Ok(());
        }
        if !settings.user.is_ascii() || !settings.password.is_ascii() {
            return Err(failed());
        }
        let mechanisms: Vec<String> = hello
            .lines
            .iter()
            .filter_map(|line| {
                let upper = line.to_ascii_uppercase();
                upper
                    .strip_prefix("AUTH ")
                    .or_else(|| upper.strip_prefix("AUTH="))
                    .map(str::to_owned)
            })
            .flat_map(|line| {
                line.split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect();
        // smtplib's preference order is CRAM-MD5, PLAIN, LOGIN; a rejected
        // mechanism falls through to the next advertised one.
        if mechanisms.iter().any(|m| m == "CRAM-MD5") {
            let mut reply = self.command("AUTH CRAM-MD5")?;
            let mut challenges = 0;
            while reply.code == 334 {
                challenges += 1;
                let challenge = STANDARD
                    .decode(reply.lines.concat().trim())
                    .map_err(|_| failed())?;
                let mut mac = Hmac::<Md5>::new_from_slice(settings.password.as_bytes())
                    .map_err(|_| failed())?;
                mac.update(&challenge);
                let digest: String = mac
                    .finalize()
                    .into_bytes()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                reply = self.command(&STANDARD.encode(format!("{} {digest}", settings.user)))?;
                // smtplib aborts the whole login after more than five challenges.
                if challenges > MAX_CHALLENGES {
                    return Err(failed());
                }
            }
            if matches!(reply.code, 235 | 503) {
                return Ok(());
            }
        }
        if mechanisms.iter().any(|m| m == "PLAIN") {
            let token = STANDARD.encode(format!("\0{}\0{}", settings.user, settings.password));
            let mut reply = self.command(&format!("AUTH PLAIN {token}"))?;
            if reply.code == 334 {
                reply = self.command(&token)?;
            }
            if matches!(reply.code, 235 | 503) {
                return Ok(());
            }
        }
        if mechanisms.iter().any(|m| m == "LOGIN") {
            let mut reply =
                self.command(&format!("AUTH LOGIN {}", STANDARD.encode(&settings.user)))?;
            if reply.code == 334 {
                reply = self.command(&STANDARD.encode(&settings.password))?;
            }
            if matches!(reply.code, 235 | 503) {
                return Ok(());
            }
        }
        Err(failed())
    }
}

// RFC 2047 UTF-8 base64 words, folded by a space. Every physical line that
// holds an encoded word stays within 76 characters: the first line is sized
// for `first_prefix` characters already on it, and when that leaves no room
// for a useful word the value folds right after the colon (returned text then
// starts with CRLF + space). Python's email package does not fold these.
fn encoded_words(text: &str, first_prefix: usize) -> String {
    const OVERHEAD: usize = 12; // =?utf-8?b? ... ?=
    let bytes_for = |line: usize| line.saturating_sub(OVERHEAD) / 4 * 3;
    let mut limit = bytes_for(76usize.saturating_sub(first_prefix));
    let mut words = Vec::new();
    let mut rest = text;
    if limit < 4 {
        words.push(String::new());
        limit = bytes_for(75);
    }
    while !rest.is_empty() {
        let mut end = rest.len().min(limit);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        words.push(format!("=?utf-8?b?{}?=", STANDARD.encode(&rest[..end])));
        rest = &rest[end..];
        limit = bytes_for(75);
    }
    words.join("\r\n ")
}

fn date_header(zone: Option<chrono_tz::Tz>) -> String {
    const FORMAT: &str = "%a, %d %b %Y %H:%M:%S %z";
    match zone {
        Some(zone) => chrono::Utc::now()
            .with_timezone(&zone)
            .format(FORMAT)
            .to_string(),
        None => chrono::Local::now().format(FORMAT).to_string(),
    }
}

// One complete header field (without the final CRLF).
fn header_field(name: &str, value: &str) -> String {
    if value.is_ascii() {
        return format!("{name}: {value}");
    }
    let words = encoded_words(value, name.len() + 2);
    if words.starts_with("\r\n") {
        format!("{name}:{words}")
    } else {
        format!("{name}: {words}")
    }
}

// Length of the physical line on which `text` ends, given that it starts at
// column `start` of a line.
fn end_column(text: &str, start: usize) -> usize {
    match text.rfind("\r\n") {
        Some(index) => text.len() - index - 2,
        None => start + text.len(),
    }
}

// One mailbox starting at `column` of its line. `trailing` reserves room for a
// list comma that follows the address. Lines carrying an encoded word stay
// within 76 characters including that comma; "<address>" moves to its own
// line when it would not fit.
fn formatted(mailbox: &Mailbox, column: usize, trailing: usize) -> String {
    if mailbox.name.is_empty() {
        return mailbox.address.clone();
    }
    if mailbox.name.is_ascii() {
        return format!(
            "\"{}\" <{}>",
            mailbox.name.replace('\\', "\\\\").replace('"', "\\\""),
            mailbox.address
        );
    }
    let name = encoded_words(&mailbox.name, column);
    let last = end_column(&name, column);
    let separator = if last + 1 + mailbox.address.len() + 2 + trailing > 76 {
        "\r\n "
    } else {
        " "
    };
    format!("{name}{separator}<{}>", mailbox.address)
}

// A complete address header (without the final CRLF). Every entry after the
// first starts on a continuation line at column 1; when the first word needs
// the whole line the value folds right after the colon.
fn address_field(field: &str, list: &[&Mailbox]) -> String {
    let column = field.len() + 2;
    let mut entries = Vec::new();
    for (index, mailbox) in list.iter().enumerate() {
        let trailing = usize::from(index + 1 < list.len());
        entries.push(formatted(
            mailbox,
            if index == 0 { column } else { 1 },
            trailing,
        ));
    }
    let value = entries.join(",\r\n ");
    if value.starts_with("\r\n") {
        format!("{field}:{value}")
    } else {
        format!("{field}: {value}")
    }
}

fn encoded_body(body: &str) -> String {
    let encoded = STANDARD.encode(body);
    encoded
        .as_bytes()
        .chunks(76)
        .map(|line| format!("{}\r\n", std::str::from_utf8(line).expect("base64 ASCII")))
        .collect()
}

fn message(settings: &Settings, target: &Mailbox, body: &str) -> (Vec<String>, String) {
    let bcc: Vec<_> = settings
        .bcc
        .iter()
        .filter(|m| m.address != target.address)
        .collect();
    let cc: Vec<_> = settings
        .cc
        .iter()
        .filter(|m| m.address != target.address && !bcc.iter().any(|b| b.address == m.address))
        .collect();
    let reply: Vec<_> = settings
        .reply
        .iter()
        .filter(|m| m.address != target.address)
        .collect();
    let recipients = std::iter::once(target.address.clone())
        .chain(cc.iter().chain(bcc.iter()).map(|m| m.address.clone()))
        .collect();
    let application = settings
        .headers
        .iter()
        .find(|(name, _)| name == "X-Application")
        .map_or("Apprise", |(_, value)| value.as_str());
    let mut content = format!(
        "MIME-Version: 1.0\r\n{}\r\n",
        header_field("X-Application", application)
    );
    for (name, value) in settings
        .headers
        .iter()
        .filter(|(n, _)| n != "X-Application")
    {
        content.push_str(&header_field(name, value));
        content.push_str("\r\n");
    }
    content.push_str(&format!(
        "Subject: Catabolic\r\n{}\r\n{}\r\nMessage-ID: <{}@{}>\r\nDate: {}\r\n",
        address_field("From", &[&settings.from]),
        address_field("To", &[target]),
        uuid::Uuid::new_v4(),
        settings.host,
        date_header(settings.tz)
    ));
    if !cc.is_empty() {
        content.push_str(&address_field("Cc", &cc));
        content.push_str("\r\n");
    }
    if !reply.is_empty() {
        content.push_str(&address_field("Reply-To", &reply));
        content.push_str("\r\n");
    }
    let part = |kind: &str| {
        format!(
            "Content-Type: text/{kind}; charset=\"utf-8\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}",
            encoded_body(body)
        )
    };
    if settings.html {
        let boundary = format!("catabolic-{}", uuid::Uuid::new_v4());
        content.push_str(&format!("Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\n{}\r\n--{boundary}\r\n{}\r\n--{boundary}--\r\n", part("plain"), part("html")));
    } else {
        content.push_str(&part("plain"));
    }
    (recipients, content)
}

fn send(
    settings: &Settings,
    body: &str,
    deadline: Instant,
    tls: Arc<ClientConfig>,
) -> io::Result<()> {
    if settings.to.is_empty() {
        return Err(failed());
    }
    let addresses = resolve(&settings.host, settings.port, deadline)?;
    let mut connected = None;
    for address in addresses {
        if let Ok(socket) =
            TcpStream::connect_timeout(&address, remaining(deadline)?.min(settings.timeout))
        {
            connected = Some(socket);
            break;
        }
    }
    let socket = connected.ok_or_else(failed)?;
    socket.set_nodelay(true)?;
    let wire = Wire::Plain(DeadlineSocket {
        stream: socket,
        deadline,
        timeout: settings.timeout,
    });
    let wire = if settings.mode == Mode::Tls {
        wire.secure(&settings.host, tls.clone())?
    } else {
        wire
    };
    let mut session = Session {
        wire,
        deadline,
        read_bytes: 0,
        written_bytes: 0,
    };
    if session.reply()?.code != 220 {
        return Err(failed());
    }
    let mut hello = session.hello()?;
    if settings.mode == Mode::StartTls {
        if !hello
            .lines
            .iter()
            .any(|line| line.eq_ignore_ascii_case("STARTTLS"))
        {
            return Err(failed());
        }
        if session.command("STARTTLS")?.code != 220 {
            return Err(failed());
        }
        session.wire = session.wire.secure(&settings.host, tls)?;
        hello = session.hello()?;
    }
    session.authenticate(settings, &hello)?;
    let size = hello.lines.iter().any(|line| {
        line.split_whitespace()
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case("SIZE"))
    });
    let mut any_failure = false;
    for target in &settings.to {
        let (recipients, content) = message(settings, target, body);
        let command = format!(
            "MAIL FROM:<{}>{}",
            settings.from.address,
            if size {
                format!(" size={}", content.len())
            } else {
                String::new()
            }
        );
        if session.command(&command)?.code != 250 {
            any_failure = true;
            if session.command("RSET")?.code != 250 {
                return Err(failed());
            }
            continue;
        }
        let mut accepted = 0;
        for recipient in recipients {
            if matches!(
                session.command(&format!("RCPT TO:<{recipient}>"))?.code,
                250 | 251
            ) {
                accepted += 1;
            }
        }
        // smtplib.sendmail succeeds when at least one envelope recipient was
        // accepted; partial refusal is not upgraded to a full delivery failure.
        if accepted == 0 {
            any_failure = true;
            if session.command("RSET")?.code != 250 {
                return Err(failed());
            }
            continue;
        }
        if session.command("DATA")?.code != 354 {
            any_failure = true;
            if session.command("RSET")?.code != 250 {
                return Err(failed());
            }
            continue;
        }
        let escaped = content
            .split_inclusive("\r\n")
            .map(|line| {
                if line.starts_with('.') {
                    format!(".{line}")
                } else {
                    line.to_owned()
                }
            })
            .collect::<String>();
        session.send(format!("{escaped}.\r\n").as_bytes())?;
        if session.reply()?.code != 250 {
            any_failure = true;
        }
    }
    // Delivery is acknowledged by DATA's 250, not by QUIT. The best-effort
    // write also obeys the same deadline and is never retried in a background job.
    let _ = session.send(b"QUIT\r\n");
    if any_failure { Err(failed()) } else { Ok(()) }
}

pub(super) fn deliver(url: &str, body: &str, deadline: Instant) -> Outcome {
    let settings = match Settings::parse(url) {
        Ok(settings) => settings,
        Err(()) => return Outcome::InvalidDestination,
    };
    if body.len() > 4096 || body.chars().any(char::is_control) {
        return Outcome::InvalidDestination;
    }
    let deadline = deadline.min(Instant::now() + Duration::from_secs(30));
    let tls = tls_config();
    for attempt in 0..=settings.retry {
        if send(&settings, body, deadline, tls.clone()).is_ok() {
            return Outcome::Complete;
        }
        if attempt < settings.retry {
            let Ok(left) = remaining(deadline) else {
                break;
            };
            if settings.wait >= left {
                break;
            }
            std::thread::sleep(settings.wait);
        }
    }
    if settings.optional {
        Outcome::Complete
    } else {
        Outcome::TemporarilyFailed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::{
        ServerConfig, ServerConnection,
        pki_types::{CertificateDer, PrivateKeyDer, UnixTime, pem::PemObject},
    };
    use std::{net::TcpListener, thread};

    const BODY: &str = "A Catabolic processing job completed.";
    const CA: &[u8] = include_bytes!("../../tests/fixtures/smtp/ca.pem");
    const CERT: &[u8] = include_bytes!("../../tests/fixtures/smtp/cert.pem");
    const KEY: &[u8] = include_bytes!("../../tests/fixtures/smtp/key.pem");

    #[derive(Debug)]
    struct FixtureTime;
    impl rustls::time_provider::TimeProvider for FixtureTime {
        fn current_time(&self) -> Option<UnixTime> {
            Some(UnixTime::since_unix_epoch(Duration::from_secs(1800000000)))
        }
    }

    fn client_config() -> Arc<ClientConfig> {
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from_pem_slice(CA).unwrap())
            .unwrap();
        let mut config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth();
        config.time_provider = Arc::new(FixtureTime);
        Arc::new(config)
    }

    fn server_config() -> Arc<ServerConfig> {
        Arc::new(
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from_pem_slice(CERT).unwrap()],
                    PrivateKeyDer::from_pem_slice(KEY).unwrap(),
                )
                .unwrap(),
        )
    }

    fn line(stream: &mut impl Read) -> io::Result<String> {
        let mut result = Vec::new();
        loop {
            let mut byte = [0];
            stream.read_exact(&mut byte)?;
            result.push(byte[0]);
            if result.len() > 32768 {
                return Err(failed());
            }
            if result.ends_with(b"\r\n") {
                return String::from_utf8(result).map_err(|_| failed());
            }
        }
    }

    fn say(stream: &mut impl Write, value: &str) {
        stream.write_all(value.as_bytes()).unwrap();
        stream.flush().unwrap();
    }

    #[derive(Default)]
    struct Capture {
        commands: Vec<String>,
        messages: Vec<String>,
    }

    fn exchange(
        stream: &mut (impl Read + Write),
        auth: &str,
        refuse_first: bool,
        messages: usize,
    ) -> Capture {
        assert_eq!(line(stream).unwrap(), "EHLO [127.0.0.1]\r\n");
        say(
            stream,
            &format!("250-owned fixture\r\n250-SIZE 262144\r\n250 AUTH {auth}\r\n"),
        );
        if auth == "PLAIN" {
            assert_eq!(
                line(stream).unwrap(),
                format!(
                    "AUTH PLAIN {}\r\n",
                    STANDARD.encode("\0user\0synthetic-password")
                )
            );
            say(stream, "235 authenticated\r\n");
        } else if auth == "LOGIN" {
            assert_eq!(
                line(stream).unwrap(),
                format!("AUTH LOGIN {}\r\n", STANDARD.encode("user"))
            );
            say(stream, "334 UGFzc3dvcmQ6\r\n");
            assert_eq!(
                line(stream).unwrap(),
                format!("{}\r\n", STANDARD.encode("synthetic-password"))
            );
            say(stream, "235 authenticated\r\n");
        }
        let mut capture = Capture::default();
        let mut refused = false;
        while capture.messages.len() < messages {
            let command = line(stream).unwrap();
            capture.commands.push(command.clone());
            if command.starts_with("MAIL FROM:") {
                say(stream, "250\r\n");
            } else if command.starts_with("RCPT TO:") {
                if refuse_first && !refused {
                    say(stream, "550 recipient unavailable\r\n");
                    refused = true;
                } else {
                    say(stream, "250 accepted\r\n");
                }
            } else if command == "RSET\r\n" {
                say(stream, "250 reset\r\n");
            } else if command == "DATA\r\n" {
                say(stream, "354 start data\r\n");
                let mut content = String::new();
                loop {
                    let part = line(stream).unwrap();
                    if part == ".\r\n" {
                        break;
                    }
                    content.push_str(&part);
                }
                capture.messages.push(content);
                say(stream, "250 queued\r\n");
            } else {
                panic!("unexpected synthetic SMTP command");
            }
        }
        assert_eq!(line(stream).unwrap(), "QUIT\r\n");
        capture
    }

    fn fixture(
        mode: Mode,
        auth: &'static str,
        refuse_first: bool,
        messages: usize,
    ) -> (u16, thread::JoinHandle<Capture>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            if mode == Mode::Plain {
                say(&mut socket, "220 owned SMTP fixture\r\n");
                exchange(&mut socket, auth, refuse_first, messages)
            } else {
                if mode == Mode::StartTls {
                    say(&mut socket, "220 owned SMTP fixture\r\n");
                    assert_eq!(line(&mut socket).unwrap(), "EHLO [127.0.0.1]\r\n");
                    say(&mut socket, "250-owned fixture\r\n250 STARTTLS\r\n");
                    assert_eq!(line(&mut socket).unwrap(), "STARTTLS\r\n");
                    say(&mut socket, "220 begin TLS\r\n");
                }
                let mut stream =
                    StreamOwned::new(ServerConnection::new(server_config()).unwrap(), socket);
                if mode == Mode::Tls {
                    say(&mut stream, "220 owned SMTPS fixture\r\n");
                }
                exchange(&mut stream, auth, refuse_first, messages)
            }
        });
        (port, task)
    }

    // RFC 5322 line limit, and RFC 2047 section 2's 76 for lines with encoded words.
    fn assert_line_limits(content: &str, context: &str) {
        for line in content.split("\r\n") {
            assert!(line.len() <= 998, "{context}: {line}");
            if line.contains("=?utf-8?") {
                assert!(line.len() <= 76, "{context}: {} {line}", line.len());
            }
        }
    }

    // Top-level header fields other than those native always generates, with
    // RFC 2047 UTF-8 base64 words decoded and folding removed.
    fn custom_headers(content: &str) -> Vec<(String, String)> {
        let block = content
            .split_once("\r\n\r\n")
            .unwrap()
            .0
            .replace("\r\n ", " ");
        block
            .split("\r\n")
            .filter_map(|line| {
                let (name, value) = line.split_once(':').unwrap();
                if [
                    "content-type",
                    "content-transfer-encoding",
                    "mime-version",
                    "x-application",
                    "subject",
                    "from",
                    "to",
                    "cc",
                    "bcc",
                    "reply-to",
                    "message-id",
                    "date",
                ]
                .contains(&name.to_ascii_lowercase().as_str())
                {
                    return None;
                }
                let value = value.strip_prefix(' ').unwrap_or(value);
                let value = if value.starts_with("=?utf-8?b?") {
                    value
                        .split(' ')
                        .map(|word| {
                            let word = word.strip_prefix("=?utf-8?b?").unwrap();
                            let word = word.strip_suffix("?=").unwrap();
                            String::from_utf8(STANDARD.decode(word).unwrap()).unwrap()
                        })
                        .collect()
                } else {
                    value.to_owned()
                };
                Some((name.to_owned(), value.trim().to_owned()))
            })
            .collect()
    }

    // Scripted AUTH server mirroring tests/reference/smtp_auth_oracle.py.
    // Returns client command lines from after EHLO until MAIL FROM (or EOF) and
    // whether MAIL FROM arrived.
    fn auth_server(
        advertised: String,
        accepted: Vec<String>,
        challenge: String,
        challenges: usize,
    ) -> (u16, thread::JoinHandle<(Vec<String>, bool)>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            say(&mut socket, "220 scripted fixture\r\n");
            assert_eq!(line(&mut socket).unwrap(), "EHLO [127.0.0.1]\r\n");
            say(
                &mut socket,
                &format!("250-scripted fixture\r\n250-SIZE 262144\r\n250 AUTH {advertised}\r\n"),
            );
            let accepts = |name: &str| accepted.iter().any(|a| a == name);
            let verdict = |socket: &mut TcpStream, name: &str| {
                say(
                    socket,
                    if accepts(name) {
                        "235 authenticated\r\n"
                    } else {
                        "535 authentication failed\r\n"
                    },
                );
            };
            let mut seen = Vec::new();
            let mut mail = false;
            let mut awaiting: Option<&str> = None;
            let mut sent_challenges = 0;
            while let Ok(text) = line(&mut socket) {
                let command = text.trim_end().to_owned();
                if command.starts_with("MAIL FROM:") {
                    say(&mut socket, "250 ok\r\n");
                    mail = true;
                    break;
                }
                seen.push(command.clone());
                match awaiting {
                    Some("cram") => {
                        if sent_challenges < challenges {
                            sent_challenges += 1;
                            say(&mut socket, &format!("334 {challenge}\r\n"));
                        } else {
                            awaiting = None;
                            verdict(&mut socket, "CRAM-MD5");
                        }
                        continue;
                    }
                    Some("login") => {
                        awaiting = None;
                        verdict(&mut socket, "LOGIN");
                        continue;
                    }
                    _ => {}
                }
                if command == "AUTH CRAM-MD5" {
                    awaiting = Some("cram");
                    sent_challenges = 1;
                    say(&mut socket, &format!("334 {challenge}\r\n"));
                } else if command.starts_with("AUTH PLAIN ") {
                    verdict(&mut socket, "PLAIN");
                } else if command.starts_with("AUTH LOGIN ") {
                    awaiting = Some("login");
                    say(&mut socket, "334 UGFzc3dvcmQ6\r\n");
                } else {
                    say(&mut socket, "504 mechanism unavailable\r\n");
                }
            }
            (seen, mail)
        });
        (port, task)
    }

    #[test]
    fn frozen_apprise_smtp_auth_transcripts_include_cram_md5() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/reference/smtp_auth.json")).unwrap();
        let (user, password) = (
            oracle["user"].as_str().unwrap(),
            oracle["password"].as_str().unwrap(),
        );
        for scenario in oracle["scenarios"].as_array().unwrap() {
            let name = scenario["name"].as_str().unwrap();
            let accepted = scenario["accepted"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect();
            let (port, server) = auth_server(
                scenario["advertised"].as_str().unwrap().to_owned(),
                accepted,
                oracle["challenge_b64"].as_str().unwrap().to_owned(),
                1,
            );
            let url = format!(
                "mailto://{user}:{password}@127.0.0.1:{port}/one@example.net?from=sender@example.com&mode=insecure"
            );
            // The fixture ends at MAIL FROM, so a native success then reads EOF.
            let actual = deliver(&url, BODY, Instant::now() + Duration::from_secs(3));
            let (seen, mail) = server.join().unwrap();
            let expected: Vec<_> = scenario["auth_commands"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .filter(|c| !c.eq_ignore_ascii_case("quit"))
                .collect();
            assert_eq!(seen, expected, "{name}");
            assert_eq!(scenario["mail_sent"], mail, "{name}");
            if !mail {
                assert_eq!(actual, Outcome::TemporarilyFailed, "{name}");
            }
        }
    }

    #[test]
    fn cram_md5_matches_rfc_2195_and_bounds_server_challenges() {
        // RFC 2195 section 2 example.
        let challenge = STANDARD.encode("<1896.697170952@postoffice.reston.mci.net>");
        let expected = STANDARD.encode("tim b913a602c7eda7a495b4e6e7334d3890");
        let (port, server) = auth_server(
            "CRAM-MD5".into(),
            vec!["CRAM-MD5".into()],
            challenge.clone(),
            1,
        );
        let url = format!(
            "mailto://tim:tanstaaftanstaaf@127.0.0.1:{port}/one@example.net?from=sender@example.com&mode=insecure"
        );
        let _ = deliver(&url, BODY, Instant::now() + Duration::from_secs(3));
        assert_eq!(
            server.join().unwrap().0,
            ["AUTH CRAM-MD5".to_owned(), expected]
        );
        // smtplib gives up after more than five challenges and never tries
        // another mechanism; each response is for the newest challenge.
        let (port, server) =
            auth_server("CRAM-MD5 PLAIN".into(), vec!["PLAIN".into()], challenge, 50);
        let url = format!(
            "mailto://tim:tanstaaftanstaaf@127.0.0.1:{port}/one@example.net?from=sender@example.com&mode=insecure"
        );
        assert_eq!(
            deliver(&url, BODY, Instant::now() + Duration::from_secs(3)),
            Outcome::TemporarilyFailed
        );
        let seen = server.join().unwrap().0;
        assert_eq!(seen.len(), 7, "AUTH plus six responses: {seen:?}");
        assert!(seen.iter().all(|c| !c.starts_with("AUTH PLAIN")));
        // Invalid challenge encodings and non-ASCII credentials fail closed.
        let (port, server) =
            auth_server("CRAM-MD5".into(), vec!["CRAM-MD5".into()], "!!!".into(), 1);
        let url = format!(
            "mailto://tim:tanstaaftanstaaf@127.0.0.1:{port}/one@example.net?from=sender@example.com&mode=insecure"
        );
        assert_eq!(
            deliver(&url, BODY, Instant::now() + Duration::from_secs(3)),
            Outcome::TemporarilyFailed
        );
        assert_eq!(server.join().unwrap().0, ["AUTH CRAM-MD5".to_owned()]);
    }

    #[test]
    fn explicit_timezone_sets_date_offset_and_ambiguous_names_fail_closed() {
        use chrono::Offset;
        let zone = |name: &str| {
            Settings::parse(&format!(
                "mailto://127.0.0.1:1/one@example.net?from=sender@example.com&tz={name}"
            ))
            .map(|s| s.tz)
        };
        assert_eq!(zone("Asia/Tokyo").unwrap(), Some(chrono_tz::Asia::Tokyo));
        assert_eq!(zone("tokyo").unwrap(), Some(chrono_tz::Asia::Tokyo));
        assert_eq!(zone("GMT0").unwrap(), Some(chrono_tz::UTC));
        assert_eq!(
            zone("%20Europe/paris%20").unwrap(),
            Some(chrono_tz::Europe::Paris)
        );
        // US/Pacific and Canada/Pacific agree on every sampled offset.
        assert!(zone("Pacific").unwrap().is_some());
        for bad in [
            "",
            "%20",
            "Nope",
            "%2E%2E/Asia/Tokyo",
            "Asia//Tokyo",
            "Asia%00Tokyo",
            "Etc",
        ] {
            assert!(zone(bad).is_err(), "{bad}");
        }
        assert!(zone(&"a".repeat(65)).is_err());
        let paris = Settings::parse(
            "mailto://127.0.0.1:1/one@example.net?from=sender@example.com&tz=Europe/Paris",
        )
        .unwrap();
        let (_, content) = message(&paris, &paris.to[0], BODY);
        let date = content
            .lines()
            .find_map(|line| line.strip_prefix("Date: "))
            .unwrap();
        let seconds = chrono::Utc::now()
            .with_timezone(&chrono_tz::Europe::Paris)
            .offset()
            .fix()
            .local_minus_utc();
        let expected = format!(
            "{}{:02}{:02}",
            if seconds < 0 { '-' } else { '+' },
            seconds.abs() / 3600,
            seconds.abs() % 3600 / 60
        );
        assert!(date.ends_with(&format!(" {expected}")), "{date}");
        let parsed = chrono::DateTime::parse_from_rfc2822(date).unwrap();
        assert!((chrono::Utc::now() - parsed.to_utc()).num_seconds().abs() < 60);
    }

    #[test]
    fn custom_header_options_follow_pinned_apprise_and_fail_closed() {
        let base = "mailto://127.0.0.1:1/one@example.net?from=sender@example.com&";
        let headers = |suffix: &str| Settings::parse(&format!("{base}{suffix}")).map(|s| s.headers);
        assert_eq!(
            headers("+X-A=1&+x-a=2&+X-A=3").unwrap(),
            [
                ("X-A".to_owned(), "3".to_owned()),
                ("x-a".to_owned(), "2".to_owned())
            ]
        );
        // Names and values are percent-decoded twice; `+` after the sigil is a space.
        assert_eq!(
            headers("%2BX-B=%2541&+X-C=a+b").unwrap(),
            [
                ("X-B".to_owned(), "A".to_owned()),
                ("X-C".to_owned(), "a+b".to_owned())
            ]
        );
        assert!(headers("+X+B=1").is_err());
        assert!(headers("+X-%2541=1").is_ok());
        for bad in [
            "+X-A=a%0Ab",
            "+X-A=a%250Ab",
            "+X-A=a%250Db",
            "+X-A=a%09b",
            "+X-A%0D%0AB=1",
            "+%C3%A9=1",
            "+X-A%3AB=1",
            "+=1",
            "+X-A=%",
            "+Bcc=hidden@example.net",
            "+BCC=hidden@example.net",
            "+Content-Disposition=attachment",
            "+Resent-To=a@example.net",
            "+x-application=Other",
            "%20X-A=1",
        ] {
            assert!(headers(bad).is_err(), "{bad}");
        }
        let long_value = format!("+X-A={}", "a".repeat(MAX_HEADER_VALUE + 1));
        assert!(headers(&long_value).is_err());
        let many: Vec<_> = (0..=MAX_HEADERS).map(|i| format!("+X-{i}=v")).collect();
        assert!(headers(&many.join("&")).is_err());
        let settings = Settings::parse(&format!("{base}+X-Application=Mine&+X-Café%3D=1")).is_err();
        assert!(settings);
        let settings =
            Settings::parse(&format!("{base}+X-Application=Mine&+X-B=caf%C3%A9")).unwrap();
        let (_, content) = message(&settings, &settings.to[0], BODY);
        assert!(content.contains("MIME-Version: 1.0\r\nX-Application: Mine\r\nX-B: =?utf-8?b?Y2Fmw6k=?=\r\nSubject: Catabolic\r\n"));
        assert!(content.is_ascii());
    }

    // Decodes one address header from a message: unfolds, splits entries and
    // decodes quoted or RFC 2047 UTF-8 names. Returns None if absent.
    fn address_header(content: &str, field: &str) -> Option<Vec<(String, String)>> {
        let block = content.split_once("\r\n\r\n")?.0;
        let start = block
            .split("\r\n")
            .scan(false, |inside, line| {
                let starts = line.starts_with(&format!("{field}:"));
                if starts {
                    *inside = true;
                } else if !line.starts_with(' ') {
                    *inside = false;
                }
                Some((*inside, line))
            })
            .filter(|(inside, _)| *inside)
            .map(|(_, line)| line)
            .collect::<Vec<_>>()
            .join("\r\n");
        if start.is_empty() {
            return None;
        }
        let value = start.strip_prefix(&format!("{field}:")).unwrap();
        Some(
            value
                .split(",\r\n ")
                .map(|entry| {
                    let entry = entry.replace("\r\n ", " ");
                    let entry = entry.trim();
                    let Some((name, address)) = entry.rsplit_once(" <") else {
                        return (String::new(), entry.to_owned());
                    };
                    let address = address.strip_suffix('>').unwrap().to_owned();
                    let name = if let Some(quoted) = name.strip_prefix('"') {
                        quoted
                            .strip_suffix('"')
                            .unwrap()
                            .replace("\\\"", "\"")
                            .replace("\\\\", "\\")
                    } else {
                        name.split(' ')
                            .map(|word| {
                                let word = word.strip_prefix("=?utf-8?b?").unwrap();
                                let word = word.strip_suffix("?=").unwrap();
                                String::from_utf8(STANDARD.decode(word).unwrap()).unwrap()
                            })
                            .collect()
                    };
                    (name, address)
                })
                .collect(),
        )
    }

    #[test]
    fn address_headers_hold_line_limits_across_every_boundary() {
        let base = "mailto://127.0.0.1:1/one@example.net?from=sender@example.com";
        let mailbox = |name: &str, local: usize, tag: usize| Mailbox {
            name: name.to_owned(),
            address: format!("{}{tag}@example.net", "a".repeat(local)),
        };
        let mut checked = 0;
        // Name lengths sweep every encoded-word line from well under to well
        // over 76; address lengths put "<address>" and a list comma on both
        // sides of the boundary.
        for field in ["From", "To", "Cc", "Reply-To"] {
            for count in if matches!(field, "Cc" | "Reply-To") {
                1..=3
            } else {
                1..=1
            } {
                for name_chars in 0..=70 {
                    for local in [1, 8, 20, 28, 34, 35, 36, 40, 52, 64] {
                        let names = [
                            "\u{e9}".repeat(name_chars),
                            // 3-byte and 4-byte characters shift word edges.
                            "\u{20ac}".repeat(name_chars / 2),
                            "\u{1f600}".repeat(name_chars / 3),
                        ];
                        for name in names {
                            let list: Vec<_> =
                                (0..count).map(|i| mailbox(&name, local, i)).collect();
                            let mut settings = Settings::parse(base).unwrap();
                            let target = mailbox("", 5, 9);
                            match field {
                                "From" => settings.from = list[0].clone(),
                                "To" => settings.to = vec![list[0].clone()],
                                "Cc" => settings.cc = list.clone(),
                                _ => settings.reply = list.clone(),
                            }
                            let target = if field == "To" {
                                settings.to[0].clone()
                            } else {
                                target
                            };
                            let (_, content) = message(&settings, &target, BODY);
                            let context = format!("{field} x{count} {name_chars} {local}");
                            assert_line_limits(&content, &context);
                            let decoded = address_header(&content, field).expect(&context);
                            let expected: Vec<_> = list
                                .iter()
                                .map(|m| (m.name.clone(), m.address.clone()))
                                .collect();
                            assert_eq!(decoded, expected, "{context}");
                            assert!(!content.contains("Bcc:"));
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert!(checked > 10_000);
        // Bcc recipients are envelope-only and never become a header.
        let mut settings = Settings::parse(base).unwrap();
        settings.bcc = vec![mailbox(&"\u{e9}".repeat(30), 20, 0)];
        let (recipients, content) = message(&settings, &mailbox("", 5, 9), BODY);
        assert_eq!(recipients.len(), 2);
        assert!(!content.contains("Bcc"));
    }

    #[test]
    fn encoded_header_lines_respect_rfc_2047_limits() {
        // Python leaves a 76+ character line for 25 e-acute (and never folds);
        // native folds. Name lengths cover first-line budgets down to folding
        // right after the colon.
        for name_len in [1, 8, 30, 55, 60, 64] {
            let name = format!("X{}", "a".repeat(name_len - 1));
            for value in [
                "\u{e9}".repeat(25),
                "\u{e9}".repeat(256),
                "\u{1f600}".repeat(120),
                format!("{} caf\u{e9}", "w".repeat(300)),
            ] {
                let encoded = value.replace('%', "%25");
                let encoded: String = encoded
                    .bytes()
                    .map(|b| {
                        if b.is_ascii_alphanumeric() {
                            (b as char).to_string()
                        } else {
                            format!("%{b:02X}")
                        }
                    })
                    .collect();
                let url = format!(
                    "mailto://127.0.0.1:1/one@example.net?from=sender@example.com&+{name}={encoded}"
                );
                let settings = Settings::parse(&url).unwrap();
                let (_, content) = message(&settings, &settings.to[0], BODY);
                assert_line_limits(&content, &name);
                assert_eq!(
                    custom_headers(&content),
                    [(name.clone(), value.clone())],
                    "{name_len}"
                );
            }
        }
        let long_name = "\u{e9}".repeat(40);
        let settings = Settings::parse(&format!(
            "mailto://127.0.0.1:1/one@example.net?from=sender@example.com&name={}",
            long_name.replace('\u{e9}', "%C3%A9")
        ))
        .unwrap();
        let (_, content) = message(&settings, &settings.to[0], BODY);
        assert_line_limits(&content, "display name");
    }

    #[test]
    fn custom_headers_reach_the_wire_once_and_cannot_add_envelope_recipients() {
        let (port, server) = fixture(Mode::Plain, "", false, 1);
        let url = format!(
            "mailto://127.0.0.1:{port}/one@example.net?from=sender@example.com&+X-Trace=abc&+X-Application=Custom"
        );
        assert_eq!(
            deliver(&url, BODY, Instant::now() + Duration::from_secs(3)),
            Outcome::Complete
        );
        let capture = server.join().unwrap();
        let message = &capture.messages[0];
        assert!(
            message.contains("X-Application: Custom\r\nX-Trace: abc\r\nSubject: Catabolic\r\n")
        );
        assert_eq!(message.matches("X-Application:").count(), 1);
        assert_eq!(
            capture
                .commands
                .iter()
                .filter(|c| c.starts_with("RCPT TO:"))
                .count(),
            1
        );
    }

    #[test]
    fn parses_defaults_overrides_names_and_literal_plus() {
        let gmail = Settings::parse("mailto://name:synthetic@gmail.com/to@example.net").unwrap();
        assert_eq!(
            (
                gmail.host.as_str(),
                gmail.port,
                gmail.mode,
                gmail.user.as_str()
            ),
            ("smtp.gmail.com", 587, Mode::StartTls, "name@gmail.com")
        );
        let custom = Settings::parse("mailtos://original@example.com/Person%20Name:one@example.net?user=new%40example.org&pass=p+q%252B&smtp=127.0.0.1&port=2525&mode=insecure&from=sender@example.com&name=Service&cc=Copy%3Acc@example.net&bcc=hidden@example.net&reply=help@example.net&to=second@example.net&format=text&cto=0.5").unwrap();
        assert_eq!(
            (custom.host.as_str(), custom.port, custom.mode),
            ("127.0.0.1", 2525, Mode::Plain)
        );
        assert_eq!(
            (custom.user.as_str(), custom.password.as_str()),
            ("new@example.org", "p+q+")
        );
        assert_eq!(
            custom.from,
            Mailbox {
                name: "Service".into(),
                address: "sender@example.com".into()
            }
        );
        assert_eq!(custom.to.len(), 2);
        assert_eq!(custom.to[0].name, "Person Name");
        assert_eq!(custom.cc[0].name, "Copy");
        assert_eq!(custom.timeout, Duration::from_millis(500));
        assert!(!custom.html);
        assert_eq!(
            Settings::parse("mailto://localhost").unwrap().from.address,
            "root@localhost"
        );
        assert_eq!(
            Settings::parse("mailtos://user:synthetic@example.com?mode=ssl")
                .unwrap()
                .port,
            465
        );
        let fallback = Settings::parse("mailto://synthetic@example.com?user=person").unwrap();
        assert_eq!(
            (fallback.user.as_str(), fallback.password.as_str()),
            ("person", "synthetic")
        );
    }

    #[test]
    fn rejects_security_changing_and_unsupported_options_before_network() {
        for suffix in [
            "verify=no",
            "redirect=yes",
            "pgp=encrypt",
            "wkd=yes",
            "pgpprv=/tmp/key",
            "pgppub=/tmp/key",
            "pgpkey=/tmp/key",
            "store=yes",
            "tz=Nope/Zone",
            "tz=",
            "+Bcc=other@example.com",
            "+Subject=Other",
            "template=/tmp/body",
            "unknown=yes",
            "mode=other",
            "port=0",
            "cto=nan",
            "rto=-1",
            "from=evil%0D%0ABcc:other@example.com",
            "pass=%00",
        ] {
            let url =
                format!("mailto://127.0.0.1:1/to@example.net?from=sender@example.com&{suffix}");
            assert_eq!(
                deliver(&url, BODY, Instant::now() + Duration::from_millis(100)),
                Outcome::InvalidDestination,
                "option {suffix}"
            );
        }
        assert!(Settings::parse("mailto://user:synthetic@example.com/%ZZ").is_err());
    }

    #[test]
    fn mime_fanout_keeps_bcc_out_of_headers() {
        let settings = Settings::parse("mailto://user:synthetic@example.com/One:one@example.net/Two:two@example.net?cc=one@example.net,copy@example.net,hidden@example.net&bcc=hidden@example.net&reply=one@example.net,help@example.net&name=%C3%89lodie").unwrap();
        let (recipients, body) = message(&settings, &settings.to[0], BODY);
        assert_eq!(
            recipients,
            ["one@example.net", "copy@example.net", "hidden@example.net"]
        );
        assert!(body.contains("From: =?utf-8?b?w4lsb2RpZQ==?= <user@example.com>"));
        assert!(body.contains("To: \"One\" <one@example.net>\r\n"));
        assert!(body.contains("Cc: copy@example.net\r\n"));
        assert!(body.contains("Reply-To: help@example.net\r\n"));
        assert!(!body.contains("hidden@example.net"));
        assert!(!body.contains("synthetic"));
        assert!(body.contains("multipart/alternative"));
        assert!(body.contains("text/plain"));
        assert!(body.contains("text/html"));
        assert_eq!(body.matches(&STANDARD.encode(BODY)).count(), 2);
    }

    #[test]
    fn owned_plain_smtp_auth_fanout_and_partial_refusal() {
        let (port, server) = fixture(Mode::Plain, "PLAIN", true, 2);
        let url = format!(
            "mailto://user:synthetic-password@127.0.0.1:{port}/one@example.net/two@example.net?from=sender@example.com&cc=copy@example.net&bcc=hidden@example.net&reply=help@example.net"
        );
        assert_eq!(
            deliver(&url, BODY, Instant::now() + Duration::from_secs(3)),
            Outcome::Complete
        );
        let capture = server.join().unwrap();
        assert_eq!(capture.messages.len(), 2);
        assert_eq!(
            capture
                .commands
                .iter()
                .filter(|command| command.starts_with("RCPT TO:"))
                .count(),
            6
        );
        assert!(
            capture
                .commands
                .iter()
                .any(|command| command == "RCPT TO:<hidden@example.net>\r\n")
        );
        assert!(
            capture
                .messages
                .iter()
                .all(|message| !message.contains("hidden@example.net"))
        );
    }

    #[test]
    fn owned_starttls_and_implicit_tls_with_verified_fixture_ca() {
        for mode in [Mode::StartTls, Mode::Tls] {
            let (port, server) = fixture(mode, "LOGIN", false, 1);
            let url = format!(
                "mailtos://user:synthetic-password@127.0.0.1:{port}/to@example.net?from=sender@example.com&mode={}&format=text",
                if mode == Mode::Tls { "ssl" } else { "starttls" }
            );
            let settings = Settings::parse(&url).unwrap();
            send(
                &settings,
                BODY,
                Instant::now() + Duration::from_secs(3),
                client_config(),
            )
            .unwrap();
            let capture = server.join().unwrap();
            assert_eq!(capture.messages.len(), 1);
            assert!(capture.messages[0].contains("text/plain"));
            assert!(!capture.messages[0].contains("text/html"));
        }
    }

    #[test]
    fn untrusted_tls_certificate_never_receives_auth_or_mail() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut stream =
                StreamOwned::new(ServerConnection::new(server_config()).unwrap(), socket);
            let mut byte = [0];
            assert!(stream.read(&mut byte).is_err());
        });
        let url = format!(
            "mailtos://user:synthetic-password@127.0.0.1:{port}/to@example.net?from=sender@example.com&mode=ssl"
        );
        assert_eq!(
            deliver(&url, BODY, Instant::now() + Duration::from_secs(2)),
            Outcome::TemporarilyFailed
        );
        server.join().unwrap();
    }

    #[test]
    fn smtp_errors_malformed_and_oversized_replies_are_sanitized() {
        for greeting in [
            "550 unavailable\r\n".to_owned(),
            "220-one\r\n250 other\r\n".into(),
            "220 bad\n".into(),
            format!("220 {}\r\n", "x".repeat(MAX_LINE)),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let _ = socket.write_all(greeting.as_bytes());
            });
            let url = format!("mailto://127.0.0.1:{port}/to@example.net?from=sender@example.com");
            assert_eq!(
                deliver(&url, BODY, Instant::now() + Duration::from_secs(2)),
                Outcome::TemporarilyFailed
            );
            server.join().unwrap();
        }
    }

    #[test]
    fn trickling_server_cannot_extend_global_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            for byte in b"220 very slow greeting\r\n" {
                if socket.write_all(&[*byte]).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        let start = Instant::now();
        let url = format!("mailto://127.0.0.1:{port}/to@example.net?from=sender@example.com");
        assert_eq!(
            deliver(&url, BODY, start + Duration::from_millis(120)),
            Outcome::TemporarilyFailed
        );
        assert!(start.elapsed() < Duration::from_millis(600));
        server.join().unwrap();
    }

    #[test]
    fn exhausted_deadline_prevents_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!(
            "mailto://127.0.0.1:{}/to@example.net?from=sender@example.com",
            listener.local_addr().unwrap().port()
        );
        assert_eq!(
            deliver(&url, BODY, Instant::now()),
            Outcome::TemporarilyFailed
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn frozen_apprise_smtp_parser_and_mime_cases() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/reference/smtp_wire.json")).unwrap();
        for case in oracle["cases"].as_array().unwrap() {
            let url = case["url"].as_str().unwrap();
            if case["native_expectation"] == "reject" {
                // Apprise admits or fails these (duplicate structural headers,
                // invalid names, CR/LF); native rejects before connecting.
                assert!(Settings::parse(url).is_err(), "{url}");
                assert_eq!(
                    deliver(url, BODY, Instant::now() + Duration::from_millis(100)),
                    Outcome::InvalidDestination,
                    "{url}"
                );
                continue;
            }
            assert_eq!(case["added"], true, "synthetic oracle admission: {url}");
            if case["native_expectation"] == "no_recipients" {
                // Apprise 1.13.1 drops internationalized To targets, never
                // connects and reports failure; it has no SMTPUTF8 path.
                assert_eq!(case["complete"], false, "{url}");
                assert!(case["messages"].as_array().unwrap().is_empty(), "{url}");
                assert!(case["settings"]["targets"].as_array().unwrap().is_empty());
                let settings = Settings::parse(url).unwrap();
                assert!(settings.to.is_empty(), "{url}");
                assert_eq!(
                    deliver(url, BODY, Instant::now() + Duration::from_millis(100)),
                    Outcome::TemporarilyFailed,
                    "{url}"
                );
                continue;
            }
            if case["known_reference_bug"] == "reply_to_set_mutation" {
                assert_eq!(case["complete"], false, "retained pinned bug: {url}");
                assert_eq!(
                    case["messages"].as_array().unwrap().len(),
                    1,
                    "pinned first delivery: {url}"
                );
                assert_eq!(
                    case["settings"]["targets"].as_array().unwrap().len(),
                    2,
                    "pinned second target: {url}"
                );
            } else {
                assert_eq!(case["complete"], true, "synthetic oracle delivery: {url}");
            }
            let settings = Settings::parse(url)
                .unwrap_or_else(|_| panic!("synthetic URL was not admitted: {url}"));
            let expected = &case["settings"];
            assert_eq!(settings.host, expected["smtp_host"], "synthetic URL {url}");
            assert_eq!(u64::from(settings.port), expected["port"].as_u64().unwrap());
            assert_eq!(
                match settings.mode {
                    Mode::Plain => "insecure",
                    Mode::StartTls => "starttls",
                    Mode::Tls => "ssl",
                },
                expected["secure_mode"]
            );
            assert_eq!(settings.user, expected["user"].as_str().unwrap_or(""));
            assert_eq!(
                settings.password,
                expected["password"].as_str().unwrap_or("")
            );
            assert_eq!(
                settings.from.name,
                expected["from_addr"][0].as_str().unwrap_or("")
            );
            assert_eq!(settings.from.address, expected["from_addr"][1]);
            assert_eq!(
                settings.to.len(),
                expected["targets"].as_array().unwrap().len()
            );
            assert_eq!(
                u64::from(settings.retry),
                expected["retry"].as_u64().unwrap()
            );
            assert_eq!(
                settings.wait.as_secs_f64(),
                expected["wait"].as_f64().unwrap()
            );
            assert_eq!(settings.optional, expected["optional"].as_bool().unwrap());
            if let Some(key) = expected["tz_key"].as_str() {
                // Python on a case-insensitive filesystem keeps the spelling given.
                assert!(
                    settings.tz.unwrap().name().eq_ignore_ascii_case(key),
                    "{url}: {key}"
                );
            } else {
                assert!(settings.tz.is_none(), "{url}");
            }
            for (target, expected_target) in settings
                .to
                .iter()
                .zip(expected["targets"].as_array().unwrap())
            {
                assert_eq!(target.name, expected_target[0].as_str().unwrap_or(""));
                assert_eq!(target.address, expected_target[1]);
            }
            for (target, expected_message) in
                settings.to.iter().zip(case["messages"].as_array().unwrap())
            {
                let (mut recipients, content) = message(&settings, target, BODY);
                recipients.sort();
                assert_eq!(
                    serde_json::json!(recipients),
                    expected_message["recipients"],
                    "synthetic URL {url}"
                );
                assert_eq!(expected_message["subject"], "Catabolic");
                assert!(content.contains("Subject: Catabolic\r\n"));
                assert_line_limits(&content, url);
                if let Some(offset) = expected_message["date_offset"].as_str() {
                    let date = content
                        .lines()
                        .find_map(|line| line.strip_prefix("Date: "))
                        .unwrap();
                    assert!(date.ends_with(&format!(" {offset}")), "{url}: {date}");
                }
                let application = expected_message["application"].as_str().unwrap();
                assert!(content.contains(&format!("X-Application: {application}\r\n")));
                assert_eq!(content.matches("X-Application:").count(), 1);
                let expected_extra: Vec<_> = expected_message["extra_headers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|pair| {
                        (
                            pair[0].as_str().unwrap().to_owned(),
                            pair[1].as_str().unwrap().trim().to_owned(),
                        )
                    })
                    .collect();
                assert_eq!(custom_headers(&content), expected_extra, "{url}");
                let mut parts = Vec::new();
                for part in content.split("Content-Type: text/").skip(1) {
                    let (kind, rest) = part.split_once(';').unwrap();
                    let (_, encoded) = rest
                        .split_once("Content-Transfer-Encoding: base64\r\n\r\n")
                        .unwrap();
                    let encoded = encoded
                        .lines()
                        .take_while(|line| !line.is_empty() && !line.starts_with("--"))
                        .collect::<String>();
                    let body = String::from_utf8(STANDARD.decode(encoded).unwrap()).unwrap();
                    parts.push(serde_json::json!({"type":format!("text/{kind}"),"body":body}));
                }
                assert_eq!(
                    serde_json::json!(parts),
                    expected_message["parts"],
                    "synthetic URL {url}"
                );
                let headers = content.split_once("\r\n\r\n").unwrap().0;
                assert!(!headers.contains("Bcc:"));
            }
        }
    }

    #[test]
    fn internationalized_mailboxes_are_dropped_like_apprise_and_never_use_smtputf8() {
        // The synthetic server would accept SMTPUTF8, but the pinned reference
        // never requests it; non-ASCII targets are dropped before connecting.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        for target in ["j%C3%B3se@example.net", "one@ex%C3%A4mple.net"] {
            let url = format!("mailto://127.0.0.1:{port}/{target}?from=sender@example.com");
            assert_eq!(
                deliver(&url, BODY, Instant::now() + Duration::from_millis(500)),
                Outcome::TemporarilyFailed
            );
        }
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(listener);
        let (port, server) = fixture(Mode::Plain, "", false, 1);
        let url = format!(
            "mailto://127.0.0.1:{port}/one@example.net/j%C3%B3se@example.net?from=sender@example.com&cc=j%C3%B3se@example.net&reply=r%C3%A9@example.net"
        );
        assert_eq!(
            deliver(&url, BODY, Instant::now() + Duration::from_secs(3)),
            Outcome::Complete
        );
        let capture = server.join().unwrap();
        assert_eq!(capture.messages.len(), 1);
        assert!(
            capture
                .commands
                .iter()
                .all(|c| c.is_ascii() && !c.to_ascii_uppercase().contains("SMTPUTF8"))
        );
        assert!(capture.messages[0].is_ascii());
        assert!(
            capture.messages[0].contains("To: one@example.net\r\n")
                && !capture.messages[0].contains("Cc:")
                && !capture.messages[0].contains("Reply-To:")
        );
        assert!(
            Settings::parse("mailto://127.0.0.1:1/one@example.net?from=s%C3%A9@example.com")
                .is_err()
        );
    }

    #[test]
    fn per_to_failure_still_attempts_remaining_targets() {
        let (port, server) = fixture(Mode::Plain, "", true, 1);
        let url = format!(
            "mailto://127.0.0.1:{port}/one@example.net/two@example.net?from=sender@example.com"
        );
        assert_eq!(
            deliver(&url, BODY, Instant::now() + Duration::from_secs(3)),
            Outcome::TemporarilyFailed
        );
        let capture = server.join().unwrap();
        assert!(
            capture
                .commands
                .iter()
                .any(|command| command == "RCPT TO:<one@example.net>\r\n")
        );
        assert!(
            capture
                .commands
                .iter()
                .any(|command| command == "RCPT TO:<two@example.net>\r\n")
        );
        assert!(capture.messages[0].contains("To: two@example.net\r\n"));
    }

    #[test]
    fn starttls_missing_or_refused_does_not_send_credentials() {
        for advertised in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                say(&mut socket, "220 fixture\r\n");
                assert_eq!(line(&mut socket).unwrap(), "EHLO [127.0.0.1]\r\n");
                if advertised {
                    say(&mut socket, "250 STARTTLS\r\n");
                    assert_eq!(line(&mut socket).unwrap(), "STARTTLS\r\n");
                    say(&mut socket, "454 TLS unavailable\r\n");
                } else {
                    say(&mut socket, "250 AUTH PLAIN\r\n");
                }
                let mut byte = [0];
                assert_eq!(socket.read(&mut byte).unwrap(), 0);
            });
            let url = format!(
                "mailtos://user:synthetic-password@127.0.0.1:{port}/to@example.net?from=sender@example.com"
            );
            assert_eq!(
                deliver(&url, BODY, Instant::now() + Duration::from_secs(2)),
                Outcome::TemporarilyFailed
            );
            server.join().unwrap();
        }
    }

    #[test]
    fn retry_and_optional_status_match_pinned_adapter() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            say(&mut first, "550 unavailable\r\n");
            drop(first);
            let (mut second, _) = listener.accept().unwrap();
            second
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            say(&mut second, "220 retry fixture\r\n");
            exchange(&mut second, "", false, 1)
        });
        let url = format!(
            "mailto://127.0.0.1:{port}/to@example.net?from=sender@example.com&retry=1&wait=0.001"
        );
        assert_eq!(
            deliver(&url, BODY, Instant::now() + Duration::from_secs(2)),
            Outcome::Complete
        );
        assert_eq!(server.join().unwrap().messages.len(), 1);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            say(&mut socket, "550 unavailable\r\n");
        });
        let url = format!(
            "mailto://127.0.0.1:{port}/to@example.net?from=sender@example.com&optional=yes"
        );
        assert_eq!(
            deliver(&url, BODY, Instant::now() + Duration::from_secs(2)),
            Outcome::Complete
        );
        server.join().unwrap();
    }
}
