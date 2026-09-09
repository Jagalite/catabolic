# Published outputs, Plex, Jellyfin and notifications

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

An agent selects media with queries and explicitly processes it with rules.
Projections own membership, copy/rendition policy, layout and filesystem publication.
Consumer bindings connect those outputs to downstream libraries. Optional Apprise
notifications report what happened to people. None of these integrations choose
media, rerun completed processing, or delete source files.

## Upgrade and connect

Schema 17 is an additive migration. Run `catabolic --db catalog.db db upgrade
--dry-run`, then `catabolic --db catalog.db db upgrade`. Existing catalog IDs,
profiles, projections, jobs, ownership, Jellyfin targets, dirty work and refresh
attempts survive. Upgrade alone enables no new network side effects. Connection
and binding records belong to a local profile; program bundles and manifests do
not transfer credentials, live server identities or notification destinations.

### Sign in with Plex

Authorize Catabolic using Plex's browser PIN flow. These commands need no catalog
and make requests only to Plex's fixed HTTPS authentication service:

```sh
catabolic consumer plex-login
# Open authorization_url in a browser and approve Catabolic on Plex's page.
catabolic consumer plex-login-complete LOGIN_ID
```

Use the returned `login_id` in the second command. You can open the link on another
device while running Catabolic over SSH. Catabolic never asks for your password.
Completion checks once: if still awaiting authorization, repeat after at least one
second. It reports `expired` when the local PIN deadline passes; start a new login.
No background process or callback web server is started. The browser link itself
contains a temporary authorization code; keep it private. `--machine` remains v1;
pending or expired login returns exit code 3 and `complete: false`.

Successful completion returns `credential_file`, never the token. Then explicitly
configure the server you intend to access (replace the example address and path):

```sh
catabolic --db catalog.db consumer connection-put home --application plex \
  --endpoint https://plex.example.test:32400 \
  --credential-file /absolute/path/from/credential_file --apply
catabolic --db catalog.db consumer discover home --type movie
```

Login alone does not connect a server, bind or create a library, or scan anything.
Continue with the existing-library or explicit-creation workflow below. Account
resource discovery is not implemented; choose the server address explicitly.

The default credential directory is `~/.config/catabolic/credentials`; both login
commands accept `--credential-dir` for a different local private directory. Files
are owner-only (0600) inside a private directory (0700), written atomically and
synced to disk. This is permission-protected local storage, not encrypted Keychain
storage. Keep it outside catalogs, exported outputs and shared directories. Run
scheduled workers as the same OS user with access to the credential file. The
catalog's legacy `credential_env` column stores a `file:/absolute/path` reference
for these connections; existing environment references retain their behavior.
Notification credentials remain environment-only. No database migration is needed.

Repeat completion reuses the saved credential without polling Plex again; it does
not revalidate an already-saved token. Server operations check authentication as
usual. If authorization is revoked, sign in again, then use `connection-put` with
`--credential-file NEW_FILE --apply --repair` for the same server identity and retry
blocked deliveries. Network/rate-limit failures do not erase the pending login or
existing credentials. Removing a local credential file does not revoke it at Plex;
manage Catabolic's authorization through Plex's Authorized Devices settings.
Tokens are account credentials, not grants limited to the locally bound library.

### Supply a token manually

Supply a token through the named environment variable using your protected local
secret configuration. Do not put a token in an endpoint or command argument.
HTTPS uses certificate verification; redirects are refused. Use HTTP only on a
trusted local network. Catabolic does not inspect browser/account credentials.

```sh
catabolic --db catalog.db consumer connection-put home --application plex \
  --endpoint https://plex.example.test:32400 --credential-env PLEX_TOKEN
catabolic --db catalog.db consumer connection-put home --application plex \
  --endpoint https://plex.example.test:32400 --credential-env PLEX_TOKEN --apply
catabolic --db catalog.db consumer discover home --type movie
```

Without `--apply`, setup inspects/previews but does not save, create or scan.
Discovery returns machine identity/version, capability and permission evidence,
and library IDs, UUIDs, names, types and configured roots. Mutation permission
may remain unknown until an explicitly authorized operation is attempted. Select
by stable ID, never by title: duplicate library names are supported.

If publication is blocked after a reboot by changed device IDs, use the
[verified remount repair](MIGRATIONS.md#repair-after-a-reboot-or-remount) before
retrying delivery. For intentionally replaced storage, see the
[per-source trust options](MIGRATIONS.md#trust-replacement-sources-explicitly).
Do not recreate output links to bypass identity checks.

## Bind an existing library once

Assume projection/output catalog `cinema` publishes to `/host/published`, with
movie links under `Movies`. Plex reports library `7` rooted at `/media/Movies`.

```sh
catabolic --db catalog.db consumer bind cinema-plex --connection home \
  --catalog cinema --subtree Movies --remote-root /media/Movies \
  --library-id 7 --type movie
catabolic --db catalog.db consumer bind cinema-plex --connection home \
  --catalog cinema --subtree Movies --remote-root /media/Movies \
  --library-id 7 --type movie --automatic --initial-scan --apply
catabolic --db catalog.db projection execute cinema
catabolic --db catalog.db consumer bindings
```

`--initial-scan` explicitly schedules the already-published output. Repeating the
same setup does not request another initial scan. Without it, attachment waits
for a consumer-visible change. `--automatic` enables a bounded drain after the
publication command closes its database; ordinary configured scans need no prompt.
Bindings without this option still record changes for the explicit worker.

A binding maps its subtree, component by component, to its remote root: the
example maps `Movies/Example/Example.mkv` to `/media/Movies/Example/Example.mkv`.
Paths must be POSIX namespaces, without traversal, empty components or backslashes.
Overlapping remote scopes within one library are rejected. Several nonoverlapping
bindings can share one library; one projection can have several consumers.
Movie/show/artist/photo are Plex types; mixed trees need separate appropriate
bindings. An existing output catalog can be bound even without a saved projection.

Equal host/container path strings are declarations, not proof of shared storage.
Plex must access both symlinks and their resolved source/rendition targets, with
compatible mounts and permissions. Local verification checks pinned roots, owned
publication and sources. Server-reported roots validate configuration only.
`consumer verify-indexing cinema-plex --limit 100` separately checks exact remote
file paths. It reports `indexed` or `inconclusive`; an idle server or matching
counts/titles is insufficient. The bounded first-page check can be inconclusive
on large libraries and does not prove playback/decoding or removal completion.

## Explicitly create and bind

Use `consumer discover home --type movie` for this server's scanner and agent
choices. If discovery is unavailable, creation is unavailable; Catabolic never
substitutes old example defaults. Write a local JSON file `plex-library.json`:

```json
{
  "name": "Catabolic cinema",
  "type": "movie",
  "root": "/media/Movies",
  "scanner": "VALUE_FROM_THIS_SERVER",
  "agent": "VALUE_FROM_THIS_SERVER",
  "language": "en-US"
}
```

```sh
catabolic --db catalog.db consumer create cinema-new --connection home \
  --catalog cinema --subtree Movies --remote-root /media/Movies --type movie \
  --spec plex-library.json
catabolic --db catalog.db consumer create cinema-new --connection home \
  --catalog cinema --subtree Movies --remote-root /media/Movies --type movie \
  --spec plex-library.json --automatic --initial-scan --apply
```

Apply persists intent before the creation POST and records the returned/observed
library identity. Repeating this intent reuses that identity. A lost response or
crash leaves an uncertain intent: repeat with `--reconcile` to inspect remote
state without repeating the POST. Exactly one new matching identity can be
reconciled. Zero or multiple matches remain uncertain and require operator review;
Catabolic does not guess or offer a blind POST retry. Library creation can itself
cause Plex to scan, according to server behavior.

Creation intent and local binding are separate durable steps. If remote creation
succeeds but local binding fails, inspect `consumer creations`, repair the local
configuration, and repeat the same intent. Do not invent another intent ID to
bypass an uncertain outcome. No-match discovery never automatically creates a
library; existing same-name libraries are not silently adopted.

## Delivery, retries and supervision

Creation/replacement/removal of owned published media links are visible changes.
A source scan that observes changed size, timestamp or file identity behind an
already-owned link also records durable intent, without rewriting that link.
Unobserved external edits are outside the catalog; source scans remain explicit.
Directory preparation, internal ownership markers, job counters, query/statistics
exports and unchanged repeats do not request scans. Sync, projection execution,
maintenance, rendition/processor receipt refresh and journal recovery share this
boundary. Each affected binding has durable `generation`, `verified` and
`acknowledged` counters; these are change counters, not file counts or Plex jobs.
Whole-catalog verification conservatively gates each binding. Unhealthy unrelated
parts of a shared catalog can defer the whole library request.

The worker coalesces shared-library work, checks local publication again before
sending, and pins the remote server/library identity. New changes during delivery
remain pending. Observed active scans defer another request without acknowledging
new generations. Scan HTTP acceptance is recorded separately from optional
indexing evidence. Ordinary scans never force metadata replacement, empty trash,
remove library roots, delete libraries or change Plex preferences.

```sh
catabolic --db catalog.db consumer run --limit 10
catabolic --db catalog.db consumer attempts
catabolic --db catalog.db consumer events
catabolic --db catalog.db consumer retry cinema-plex
catabolic --db catalog.db consumer watch --limit 10 --interval 30
```

`run` processes due work once. `watch` stays in the foreground; run it under your
service supervisor with the same database/profile and protected environment.
Alternatively schedule `consumer run --limit 10` every minute with cron/launchd.
No background process is secretly started. `--debounce N` on binding setup delays
eligibility by N seconds after the last change (0–3600); a scheduler or foreground
worker is necessary after the short-lived publisher exits. The automatic drain
attempts at most four library deliveries. Each HTTP call has a 15-second deadline
and 8 MiB response limit; a delivery uses a recoverable 180-second lease.

Transient failures retry with exponential backoff (10 seconds initially, capped
at one hour), at most five attempts, respecting bounded `Retry-After` values.
Busy scans defer 30 seconds without consuming a failure attempt. Authentication
and identity changes enter `repair`; exhausted failures require explicit retry.
Processing zero due events does not mean unresolved failures are gone.
A disappeared output/source prevents delivery rather than becoming a healthy
empty library. An outage never retries completed media rendering.

Delivery is **at least once**. A crash after sending but before acknowledgement
can repeat a scan. Leases and revisions prevent stale workers from acknowledging
newer work; already-sent requests cannot be unsent. `consumer disable ID` stops
future delivery and invalidates claims, keeping history and the remote library.
`enable` explicitly schedules reconciliation of changes missed while disabled.
Changing destination identity/root requires `bind ... --rebind`; old pending work
is not redirected to the new server. New server identity requires a new connection.
Credential-reference repair uses `connection-put ... --repair --apply`, followed
by `consumer retry ID`. Deleted/recreated libraries require explicit rebinding.

Plex's own automatic scan/trash settings may remove indexed records during a
storage outage. Catabolic checks what it can locally and never changes these
server preferences. Review them on the server separately.

## Jellyfin compatibility

Use the same connection/binding lifecycle with `--application jellyfin`; discover
library `ItemId`, roots and types such as `movies`. The shared adapter requests
that library's recursive default refresh without replacing existing metadata or
images. Creation, activity and indexed-path verification are explicitly unsupported
for this adapter. The [Jellyfin SDK refresh contract](https://typescript-sdk.jellyfin.org/interfaces/generated-client.LibraryApiRefreshItemRequest.html)
documents the operation's flags.

Legacy `refresh configure/list/run/retry` remains available with its original
captured targets and three-attempt history. Its request is the legacy global
`/Library/Refresh`. Network delivery now runs after releasing the writer lock.
Legacy projection/automatic-refresh suppression is retained. To adopt the new
exact-library lifecycle, explicitly configure a new Jellyfin connection/binding;
finish or inspect old work separately. Upgrade does not reinterpret historical
HTTP success as indexed-media evidence. Avoid enabling both paths for the same
publication if redundant scans are unwanted.

## Optional human notifications

Basic Catabolic and Plex require no Apprise. Install `catabolic[notifications]`,
or use `requirements/notifications.lock` with `--require-hashes --only-binary=:all:`
for the pinned optional dependency set. Configure one protected environment URL
per destination; no URL is stored in the database. Initial supported Apprise
schemes are ntfy/ntfys, discord, mailto/mailtos, tgram and gotify/gotifys. Executable
hooks and remote configuration loaders are not enabled.

```sh
catabolic --db catalog.db notify put home --credential-env APPRISE_HOME \
  --event projection_updated --event scan_requested --event scan_failed \
  --event consumer_needs_repair --severity info --tag home --apply
catabolic --db catalog.db notify status
catabolic --db catalog.db notify run --tag home --limit 10
catabolic --db catalog.db notify retry home
```

These four events are the initial supported subscriptions. `projection_updated`
works without any Plex/Jellyfin connection and emits once per verified catalog
batch, independent of how many consumers are attached. Inspect its durable
generation with `consumer publications`; the notification worker rechecks
publication interrupted between ownership commit and event scheduling.
Severity filters and
tags route generic batch summaries; messages omit media titles and paths and say
“scan request accepted”, not “media indexed”. Rich private content, processing and
curation events are deferred. New destinations do not replay old events.
Notification failures have independent leases/retries, with five attempts and
bounded backoff. Successful destinations are not resent when another fails.
Changing/disabling a destination cancels its unfinished deliveries; already-sent
messages cannot be recalled. Retrying a destination never retries consumer scans.
Missing Apprise or credentials requires repair; no failure-notification recursion
occurs. Lost acknowledgements can still cause duplicate messages.

## Agent interface and evidence

All commands support existing `--json` and version 1 `--machine` envelopes.
Inspect per-component `complete`, publication `healthy`, consumer generations,
`delivery_error`, attempt state, `scan_request_accepted` and `indexing` separately.
A consumer outage does not mark verified local publication corrupt. Machine
errors include stable consumer codes and safe-to-retry guidance. Listings are
bounded; follow their truncation indicators rather than treating a page as all work.
Naming presets remain distinct from API capability and real scanner compatibility.

Normal tests use loopback HTTP fixtures and injected crashes, never live tokens.
Run `scripts/consumer_acceptance.py` against an installed wheel for protocol
acceptance. `scripts/notification_acceptance.py` checks real optional Apprise
against local ntfy fixtures, including partial destination failure. `scripts/plex_acceptance.py --help` describes the explicitly gated
real-server harness. Live Plex indexing remains unverified until that disposable
harness succeeds; protocol responses are not evidence of real Plex behavior.
See [design and migration decisions](CONSUMER_DESIGN.md).
