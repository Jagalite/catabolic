# Native SMTP notification compatibility

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Reference: the frozen Catabolic notification worker and Apprise 1.13.1. Runtime
delivery is Rust only. Python in `smtp_wire_oracle.py` is an offline test oracle,
with SMTP replaced and socket creation denied. Its addresses and credentials are
synthetic. No live third-party delivery is part of this qualification.

| Behavior | Native scope | Remaining difference |
| --- | --- | --- |
| `mailto://`, `mailtos://` | Plain SMTP, STARTTLS, implicit TLS (`mode=ssl`), explicit host/port and `smtp=` | No opportunistic downgrade after a requested TLS upgrade fails |
| Provider defaults | Gmail, Yandex, Microsoft, Yahoo, GMX, Fastmail/aliases, Zoho, SendGrid, 163, QQ/Foxmail, Comcast, localhost | Domain defaults are pinned compatibility data, not proof a provider currently accepts password SMTP |
| Credentials | URL user/password, `user=`, `password=`, `pass=`, query precedence and password-only shorthand | ASCII AUTH credentials only; no OAuth. Mechanisms follow CPython `smtplib.login()` as used by the pinned adapter: advertised `CRAM-MD5` first (HMAC-MD5 over the decoded challenge, response `user hexdigest`; at most five challenges, then the whole login aborts), then `PLAIN`, then `LOGIN`, falling through to the next on a rejection. Advertisement matching is case-insensitive, whereas smtplib matches the server's spelling exactly. CRAM-MD5 adds the RustCrypto `md-5` 0.10.6 crate (MIT OR Apache-2.0, ships both license files; reuses the already locked `hmac` 0.12.1, `digest` 0.10.7 and `cfg-if`). MD5 is used only inside this challenge-response MAC, as the protocol requires; it is the reference's own first choice, and does not send the password even over plain SMTP |
| Addresses | Per-To fanout; path and `to=`; `from=`, `name=`, `cc=`, `bcc=`, `reply=`; common display-name forms and UTF-8 names | Deliberately bounded ASCII mailbox grammar; no quoted local parts, underscore/Unicode domain labels, or permissive trailing junk. **SMTPUTF8 is not a gap in the pinned reference**: Apprise 1.13.1 never sends the `SMTPUTF8` MAIL option. Its validator drops internationalized To/Cc/Bcc/Reply-To targets (a URL with only such To targets is admitted, logs no recipients and returns failure without connecting) and rejects an internationalized `from=` address (as a display name when `user` supplies the address). Native matches this, with 5 frozen cases and a loopback test whose server advertises SMTPUTF8 but never receives it. Implementing SMTPUTF8 would be a deliberate extension beyond Apprise and is not done |
| MIME | HTML multipart/alternative by default; text/markdown as text/plain; UTF-8 base64; Catabolic subject; pinned Apprise application header; safe static event body | Boundaries, Message-ID, folding and ordering of recipient sets differ. Date uses local time unless `tz=` is given (below) |
| Fanout/status | Partial RCPT refusal is successful if any envelope recipient was accepted; all-refused To transactions fail while remaining To transactions are attempted | Unrecoverable connection/protocol failure ends that SMTP session; durable retry can resend earlier successful recipients, as with the reference |
| Pinned Reply-To bug | Native safely applies Reply-To independently to each To recipient | Apprise 1.13.1 mutates its Reply-To set into a list and fails after the first message when multiple To targets have a nonempty Reply-To set. The oracle retains the false result and first message; native intentionally sends both |
| Retry/optional | `retry` clamped to 10, `wait` clamped to 20 seconds with pinned 0.5-second default, `optional=yes` failure suppression | A single overall 30-second deadline bounds all attempts; no retry starts after it expires |
| Timeouts/bounds | `cto` defaults to 15 seconds for each socket operation, always capped by the remaining deadline; DNS wait bounded; 16 KiB URL, 100 addresses, 64 KiB server reply budget, 256 KiB outgoing session budget | `rto` is parsed but does not alter SMTP sockets, matching the reference. Resolver-only workers may outlive a local wait; they never own credentials or send SMTP, and are capped at four |
| TLS verification | rustls hostname/chain verification and pinned WebPKI roots; untrusted certificates fail before authentication | No `verify=no`, custom/OS trust-store loading or certificate override |
| `tz=` | Date header zone, matching Apprise's `zoneinfo()` resolution: UTC aliases (`utc`, `z`, `gmt`, `etc/utc`, `etc/gmt`, `gmt0`, `utc0`), exact IANA keys, case-insensitive keys, then a trailing path suffix (`Tokyo`, `Argentina/Buenos_Aires`); the last repeated `tz=` wins; offset is formatted as `+HHMM` from the current instant in that zone | Invalid, empty or over-64-byte names reject the URL (Apprise also refuses to add it). A suffix that matches several zones is chosen by Apprise from an unordered set; native accepts it only if all matches have identical offsets at four instants spread over a year (e.g. `Pacific`), otherwise rejects. Zone rules come from the bundled chrono-tz database, not the host tzdata, so offsets for recent rule changes can differ from a differently-versioned Python install. Frozen cases pin keys for the accepted names and fixed offsets only for zones without DST |
| Custom headers | `+Name=value` query keys (also `%2BName`) for `mailto`/`mailtos`: names and values percent-decoded twice, `+` after the sigil in a name is a space, exact-name repeats keep first position with the last value, `X-Application` replaces the pinned `Apprise` value in place, other headers follow it in order before Subject/From/To/Message-ID/Date, non-ASCII values use RFC 2047 UTF-8 words | Deliberately stricter: header names must be 1-64 printable ASCII bytes without `:`; values at most 512 bytes after decoding; at most 32 headers; any control character (including CR/LF, tab, and Apprise's silently stripped leading CR/LF) rejects the URL before connecting, where Apprise either strips it or fails after connecting. Names that would duplicate or override structural fields (From, To, Cc, Bcc, Reply-To, Subject, Date, Message-ID, MIME-Version, Sender, Return-Path, Received, Autocrypt, `Content-*`, `Resent-*`, and `X-Application` in any other case) are rejected; Apprise appends its own fields after them and so emits duplicate headers, including a header `Bcc`. Custom headers never change envelope recipients. Encoded words are base64 rather than Apprise's quoted-printable |
| Unsupported inherited options | Rejected before connecting: enabled PGP/WKD, `pgppub`, `pgpkey`, `pgpprv`, `store=yes`, templates, unknown query options and `redirect=yes` | These are explicit compatibility gaps, not supported implementations. No key-file reads, remote templates, attachments or encryption downgrade are performed |
| Safe no-op options | `tag`, valid `overflow`, `inline`, `emojis`, disabled PGP/WKD/store/redirect | The fixed short ASCII event summaries have no attachment, emoji, overflow or rendering work to perform |

The test module includes synthetic loopback SMTP, STARTTLS and implicit TLS;
PLAIN/LOGIN/CRAM-MD5 authentication (8 frozen smtplib AUTH transcripts from `smtp_auth_oracle.py`, plus the RFC 2195 example vector); per-To continuation; Cc/Bcc handling; retry and optional
status; certificate and TLS-upgrade rejection; malformed/oversized replies;
trickling-server deadline enforcement; and 90 frozen parser/MIME cases. The TLS
test trust root is passed only to the private test transport. Production delivery
cannot select it or disable verification. The checked-in private key is exclusively
for the synthetic numeric-loopback fixture; it is not an account credential.

`smtp_auth_oracle.py` runs the pinned Apprise with the real `smtplib` AUTH logic against a scripted in-memory server (sockets and name resolution denied) and records the client AUTH commands; the Rust test replays the same server script over loopback and compares the command lines exactly. `smtp_wire_oracle.py` replaces SMTP wholesale and so cannot show AUTH.

Pinned source evidence: `apprise/plugins/email/base.py` constructor lines 267–550,
`apply_email_defaults` 552–644, `send` 646–777, `parse_url` 983–1136, and
`prepare_emails` 1149–1620. Recipient differences occur at 1247–1253, the pinned
Reply-To mutation at 1269, MIME at 1378, and final headers at 1599–1615.
`email/common.py` 58–73 defines TLS modes/ports; `email/templates.py` 36 onward
defines provider defaults; `apprise/url.py` 886–988 defines query precedence;
`plugins/base.py` 458–516 defines retry/wait; and `asset.py` 153/158 gives their
defaults. These references describe the inspected pinned installation, not a
claim about another Apprise version.
