// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

//! Bounded native SMTP delivery. Secrets and server replies never leave this
//! module. The only background work is bounded DNS resolution, never delivery.
use base64::{Engine, engine::general_purpose::STANDARD};
use catabolic_store::notifications::Outcome;
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
        for pair in query.split(['&', ';']).filter(|s| !s.is_empty()) {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            options.insert(
                decode(name)?.trim().to_ascii_lowercase(),
                decode(value)?.trim().to_owned(),
            );
        }
        for (key, value) in &options {
            match key.as_str() {
                "from" | "name" | "smtp" | "mode" | "to" | "cc" | "bcc" | "reply" | "user"
                | "password" | "pass" | "port" | "format" | "cto" | "rto" | "retry" | "wait"
                | "optional" => (),
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

fn formatted(mailbox: &Mailbox) -> String {
    if mailbox.name.is_empty() {
        return mailbox.address.clone();
    }
    let name = if mailbox.name.is_ascii() {
        format!(
            "\"{}\"",
            mailbox.name.replace('\\', "\\\\").replace('"', "\\\"")
        )
    } else {
        let mut words = Vec::new();
        let mut rest = mailbox.name.as_str();
        while !rest.is_empty() {
            let mut end = rest.len().min(45);
            while !rest.is_char_boundary(end) {
                end -= 1;
            }
            words.push(format!("=?utf-8?b?{}?=", STANDARD.encode(&rest[..end])));
            rest = &rest[end..];
        }
        words.join("\r\n ")
    };
    format!("{name} <{}>", mailbox.address)
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
    let mut content = format!(
        "MIME-Version: 1.0\r\nX-Application: Apprise\r\nSubject: Catabolic\r\nFrom: {}\r\nTo: {}\r\nMessage-ID: <{}@{}>\r\nDate: {}\r\n",
        formatted(&settings.from),
        formatted(target),
        uuid::Uuid::new_v4(),
        settings.host,
        chrono::Local::now().format("%a, %d %b %Y %H:%M:%S %z")
    );
    if !cc.is_empty() {
        content.push_str(&format!(
            "Cc: {}\r\n",
            cc.iter()
                .map(|m| formatted(m))
                .collect::<Vec<_>>()
                .join(",\r\n ")
        ));
    }
    if !reply.is_empty() {
        content.push_str(&format!(
            "Reply-To: {}\r\n",
            reply
                .iter()
                .map(|m| formatted(m))
                .collect::<Vec<_>>()
                .join(",\r\n ")
        ));
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
            "tz=Europe/Paris",
            "+Bcc=other@example.com",
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
            assert_eq!(case["added"], true, "synthetic oracle admission: {url}");
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
                assert_eq!(expected_message["application"], "Apprise");
                assert!(content.contains("Subject: Catabolic\r\n"));
                assert!(content.contains("X-Application: Apprise\r\n"));
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
