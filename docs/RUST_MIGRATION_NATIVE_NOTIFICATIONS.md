# Native human-notification providers

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

## Checkpoint scope

The native runtime now delivers the eight frozen human-notification summaries
without launching Python or importing Apprise. The retained URL schemes are
`ntfy`, `ntfys`, `gotify`, `gotifys`, `discord`, `tgram`, `mailto`, and `mailtos`.
The original frozen Python implementation and dependency lock remain reference evidence;
this checkpoint removes only the unused embedded native-crate adapter, without deleting the original reference engine or switching the default installed CLI.

This is a bounded native implementation, **not full Apprise option parity**.
The supported and deliberately rejected inherited options are enumerated below.
No live provider account, real credentials, production service, wheel, PyO3 gate,
or release/default switch is part of this checkpoint.

Catabolic-documented behavior is in `docs/CONSUMERS.md`, “Webhooks and optional
human notifications”: the scheme allowlist, protected environment destination,
static generic summaries, severity filtering, tags, independent durable leases,
five-attempt retry, partial destination success, repair outcomes and redaction.
The frozen worker at Catabolic commit
`453fca983222c6665a48775eef67c489c96b527e` fixes the eight message bodies, title
`Catabolic`, and mapping of `error` to provider `failure` severity. These
application policies, event envelopes, SQLite schemas and receipt owner remain
unchanged. Provider payloads never include envelope subjects, media titles,
paths, job errors, or database paths.

The previous `CATABOLIC_APPRISE_PYTHON` setting no longer affects native delivery.
A missing native helper retains the existing sanitized `uninstalled` repair
outcome; an absent destination credential remains `credential_unavailable`.
The optional-dependency installation instructions still describe the retained
Python entry point, not this native path.

## Supervision and ownership

`notifications::send_provider` retains the existing `process::command_output`
process-group owner and its inherited cancellation token. The child is the
private Rust `__notification-provider-worker`, dispatched by both CLI binaries.
It receives at most 16 KiB on stdin and emits only a constant JSON status, with a
4 KiB combined stdout/stderr limit, `umask(077)`, and a 30-second outer deadline.
The supervisor kills the process group and reaps its leader on completion,
error, timeout, cancellation, or unwinding. Existing Linux parent-death behavior
is retained. No credentials enter argv, logs, status errors or durable receipts.

Native provider operations also share an internal 30-second deadline across
DNS, TLS, recipient/topic fanout and local retries. HTTP request timeouts are
capped by the remaining budget, have a 16 KiB header limit and no retained idle
connections. Telegram discovery reads at most 64 KiB. Other HTTP success results
are status-only and never read unbounded response bodies. SMTP separately bounds
its URL, recipients, reply lines/total replies, session output and socket I/O.
The outer process owner is the hard boundary even if DNS or an OS call stalls;
resolver threads cannot survive the worker process. Per-request timeouts alone
are not claimed as cancellation or whole-delivery evidence.

Helper precedence is unchanged for existing configured helpers: registered
companion first, explicit `CATABOLIC_NATIVE_BINARY` second. An invalid or replaced
selection fails; it cannot silently use another executable. A preserved
standalone CLI additionally pins its own executable as a notification-only
fallback when no helper is configured. This fallback cannot change other
runtime owners' companion requirements. Source guards allow only the exact
registration block at that native entry point and reject missing, changed,
moved, duplicated or additional process-discovery calls.

## Provider options

All URL-provider details below are inherited Apprise 1.13.1 behavior, rather than
separately documented Catabolic options. Unknown inert HTTP query keys remain
ignored like the reference; security/side-effect capabilities listed as gaps
are rejected before any outbound request. SMTP has a stricter explicit key list.

| Provider | Native supported behavior | Explicit remaining gaps |
| --- | --- | --- |
| ntfy/ntfys | Self-hosted HTTP/HTTPS and cloud topic shorthand; multiple sorted/deduplicated topics; Basic/token auth; `to`, `mode`, `auth`, `token`, priority, image/avatar URL, Markdown flag, click/delay/email, xtags/legacy tags, actions; attachment **URL metadata** and filename | No file upload or payload-to-file conversion. Payloads over ntfy's 8,000-byte JSON threshold are rejected rather than silently converting to an attachment |
| Gotify | HTTP/HTTPS, reverse-proxy path, X-Gotify-Key, priority aliases, text and Markdown extras | No additional provider-specific option gaps established for the fixed summaries; see shared restrictions |
| Discord | Fixed HTTPS webhook endpoint, text/HTML or Markdown embeds, username/botname, avatar, image, footer/logo, href/url, thread, flags, TTS, explicit pings; exact 200/204 success and one 429 retry | No file/remote JSON templates or template-token evaluation. Multiple mention set ordering is deterministic and may differ from Python set ordering |
| Telegram | Fixed HTTPS Bot API; numeric/named chat targets, per-target/default thread, silent/preview, text/HTML/Markdown versions, multiple targets and bounded optional owner discovery | No `image=yes` bundled-image upload; no file/remote rich-message templates or tokens. Owner detection retains the frozen first-message-sender rule and is not proof of administrative ownership |
| SMTP | Plain, STARTTLS and implicit TLS; provider SMTP defaults; CRAM-MD5/PLAIN/LOGIN in smtplib's order; sender/name/to/cc/bcc/reply; default HTML or text/markdown MIME; Apprise `+Header=value` custom headers (stricter validation, structural names rejected); explicit `tz=` Date zone (ambiguous suffix names fail closed); local retry/wait/optional behavior | Permissive exotic mailbox forms (SMTPUTF8 is not offered by the pinned Apprise either: internationalized targets are dropped, never sent with `SMTPUTF8`, and native matches); PGP/WKD/key files; templates; storage; custom/OS TLS roots. Full details and pinned evidence are in [SMTP compatibility](../crates/catabolic-runtime/tests/reference/SMTP_COMPATIBILITY.md) |

Shared HTTP inherited options `retry` (clamped to 10), `wait` (clamped to 20
seconds, default 0.5), `optional`, `cto` and `rto` are implemented under the
single delivery deadline. ntfy's 5.5-second inter-send throttle and Telegram's
1 ms throttle are retained. Static short ASCII messages do not exercise emoji,
overflow or rich body transformations. `optional=yes` preserves the reference's
failure suppression: a `complete` status for such a URL is not proof the remote
service accepted the notification.

### Deliberate fail-closed differences

- `verify=no` and equivalent false values are rejected. All native TLS verifies
  the certificate and hostname using pinned WebPKI roots. OS/custom certificate
  stores are not loaded
- Redirect enabling is rejected. Provider HTTP always disables redirects,
  including the reference's implicit default redirect behavior
- Ambient `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` settings are ignored. Apprise's
  requests-based adapters could inherit them. No credential forwarding through
  ambient proxies is supported
- Filesystem/remote templates and template tokens are rejected before delivery
- HTTP headers and Basic-auth credentials retain requests-compatible Latin-1
  bytes; header characters above U+00FF are rejected before any request
- Control characters, invalid UTF-8 percent decoding, nonempty fragments and
  unsafe URL authorities are rejected. This parser is deliberately stricter
  than Apprise's permissive malformed-input repairs
- SMTP address/output/response budgets can reject inputs the reference accepts
- Native SMTP intentionally fixes the pinned multiple-To plus Reply-To bug.
  Apprise 1.13.1 sends its first message then fails after mutating a set to a
  list. The frozen fixture retains the actual false result and first message;
  a separate native test requires both requested messages exactly once. This
  is explicitly **non-parity**, not counted as a matching success case

## Evidence and reproduction

Tests use owned numeric-loopback listeners, synthetic URL credentials and a
synthetic TLS fixture CA. Fixed-host Discord/Telegram wire plans are compared to
frozen captures; their transport is tested by private test-only endpoint
substitution. Production endpoints have no override to redirect those providers.
No test contacts Discord, Telegram, ntfy.sh, an SMTP provider or another live
third party.

- `notifications::providers` unit tests cover frozen provider wire plans, owned
  HTTP/SMTP/TLS, exact response status rules, auth, fanout, malformed/oversized/
  trickling replies, retry/optional behavior and deadlines
- `native_notification_providers` CLI tests exercise both real private-worker
  entry points, full native queue drains with an empty PATH and no Python,
  partial success and durable retry, static/redacted payloads, stalled HTTP and
  SMTP cancellation, and outer timeout/socket closure
- Existing native notification tests continue to check policy, lease fencing,
  writer-lock release, event identity and ordinary webhook wire behavior. Two
  obsolete fake-Python-provider tests now exercise native transports instead
- Narrow source guard tests retain the old forbidden-call checks and add exact
  notification-helper registration negative witnesses

The HTTP oracle contains 87 matching captures from 29 URL configurations times
three severities. The SMTP oracle has 90 parser/MIME cases, including one
explicitly annotated non-parity Reply-To failure. Oracle scripts monkeypatch
requests/SMTP and reject socket creation before executing the already installed,
pinned Apprise 1.13.1. They are test-only Python, not runtime helpers; no downloaded
source or packages are installed or run.

The SMTP fixtures `smtp_wire.json` and `smtp_auth.json` were regenerated with
`python -I smtp_wire_oracle.py` and `python -I smtp_auth_oracle.py` in a scratch
virtual environment outside the repository, installed with `--require-hashes
--only-binary=:all:` from `requirements/notifications.lock` (Apprise 1.13.1,
wheel hash verified). Regenerating `smtp_wire.json` reproduced the previously
committed 34 cases byte for byte before new cases were appended. The oracle's
`tz_key` values depend on the host's tz database for names that match no zone
file case-sensitively, so tests compare keys case-insensitively.

Commands (using the preexisting offline toolchain and an assigned build cache):

```sh
cargo test --offline -p catabolic-runtime notifications::providers --lib
cargo test --offline -p catabolic-cli --test native_notification_providers
```

(Kyoto builds compile only; the resulting test executable is then run directly.)

Pinned upstream source evidence: `plugins/ntfy.py` parse_url 859–1008 and send
435–735; `plugins/gotify.py` parse_url 327 onward and send 197–276;
`plugins/discord.py` parse_url 994–1098 and send/_send 501–930;
`plugins/telegram.py` parse_url 1889–2031, owner discovery 727–858, send
1350–1583; `plugins/base.py` format handling 856 onward, retry/wait 458–516;
`url.py` throttle 328–370 and timeouts/query normalization; `asset.py` static
image/color/application defaults. These line numbers refer to the inspected
Apprise 1.13.1 installation. See the SMTP matrix for its exact source locations.

### Recorded local gate, 2026-10-12

The final source passed 23 native runtime notification tests, 8 actual-CLI
provider/supervision tests, 4 ordinary/mapped webhook wire tests, 17 existing
notification tests using the frozen Python reference harness, and 4 narrow
source-guard tests. The provider fixture counts are 87 matching HTTP captures
and 34 SMTP parser/MIME cases, with the one Reply-To failure explicitly excluded
from parity claims. Changed Rust files pass rustfmt checking; the scoped diff
passes whitespace checks. This evidence does not qualify live providers,
production credentials, Windows, macOS parent-death handling, unsupported options,
release packaging, or the thin Python wrapper.
