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
| Credentials | URL user/password, `user=`, `password=`, `pass=`, query precedence and password-only shorthand | ASCII AUTH credentials; no OAuth or CRAM-MD5-only servers. PLAIN and LOGIN are supported; CRAM-MD5 is not preferred when a server also offers them |
| Addresses | Per-To fanout; path and `to=`; `from=`, `name=`, `cc=`, `bcc=`, `reply=`; common display-name forms and UTF-8 names | Deliberately bounded ASCII mailbox grammar; no SMTPUTF8, quoted local parts, underscore/Unicode domain labels, or permissive trailing junk |
| MIME | HTML multipart/alternative by default; text/markdown as text/plain; UTF-8 base64; Catabolic subject; pinned Apprise application header; safe static event body | Boundaries, Message-ID, folding and ordering of recipient sets differ. Date uses local time; explicit `tz=` is rejected |
| Fanout/status | Partial RCPT refusal is successful if any envelope recipient was accepted; all-refused To transactions fail while remaining To transactions are attempted | Unrecoverable connection/protocol failure ends that SMTP session; durable retry can resend earlier successful recipients, as with the reference |
| Pinned Reply-To bug | Native safely applies Reply-To independently to each To recipient | Apprise 1.13.1 mutates its Reply-To set into a list and fails after the first message when multiple To targets have a nonempty Reply-To set. The oracle retains the false result and first message; native intentionally sends both |
| Retry/optional | `retry` clamped to 10, `wait` clamped to 20 seconds with pinned 0.5-second default, `optional=yes` failure suppression | A single overall 30-second deadline bounds all attempts; no retry starts after it expires |
| Timeouts/bounds | `cto` defaults to 15 seconds for each socket operation, always capped by the remaining deadline; DNS wait bounded; 16 KiB URL, 100 addresses, 64 KiB server reply budget, 256 KiB outgoing session budget | `rto` is parsed but does not alter SMTP sockets, matching the reference. Resolver-only workers may outlive a local wait; they never own credentials or send SMTP, and are capped at four |
| TLS verification | rustls hostname/chain verification and pinned WebPKI roots; untrusted certificates fail before authentication | No `verify=no`, custom/OS trust-store loading or certificate override |
| Unsupported inherited options | Rejected before connecting: enabled PGP/WKD, `pgppub`, `pgpkey`, `pgpprv`, `store=yes`, `tz`, `+Header`, templates, unknown query options and `redirect=yes` | These are explicit compatibility gaps, not supported implementations. No key-file reads, remote templates, custom-header mutation, attachments or encryption downgrade are performed |
| Safe no-op options | `tag`, valid `overflow`, `inline`, `emojis`, disabled PGP/WKD/store/redirect | The fixed short ASCII event summaries have no attachment, emoji, overflow or rendering work to perform |

The test module includes synthetic loopback SMTP, STARTTLS and implicit TLS;
PLAIN/LOGIN authentication; per-To continuation; Cc/Bcc handling; retry and optional
status; certificate and TLS-upgrade rejection; malformed/oversized replies;
trickling-server deadline enforcement; and 34 frozen parser/MIME cases. The TLS
test trust root is passed only to the private test transport. Production delivery
cannot select it or disable verification. The checked-in private key is exclusively
for the synthetic numeric-loopback fixture; it is not an account credential.

Pinned source evidence: `apprise/plugins/email/base.py` constructor lines 267–550,
`apply_email_defaults` 552–644, `send` 646–777, `parse_url` 983–1136, and
`prepare_emails` 1149–1620. Recipient differences occur at 1247–1253, the pinned
Reply-To mutation at 1269, MIME at 1378, and final headers at 1599–1615.
`email/common.py` 58–73 defines TLS modes/ports; `email/templates.py` 36 onward
defines provider defaults; `apprise/url.py` 886–988 defines query precedence;
`plugins/base.py` 458–516 defines retry/wait; and `asset.py` 153/158 gives their
defaults. These references describe the inspected pinned installation, not a
claim about another Apprise version.
