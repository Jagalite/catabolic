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

Select the same database and profile used for your published outputs. Examples
below use `--db catalog.db` and the default profile; add the global
`--profile NAME` option when needed. Check and upgrade an existing database before
configuring consumers:

```sh
catabolic --db catalog.db db status
catabolic --db catalog.db db upgrade --dry-run
catabolic --db catalog.db db upgrade
```

The current schema is 27. Consumer delivery was introduced in schema 17;
schemas 18–20 add volume identity, source trust policies and validation evidence.
See [database migrations](MIGRATIONS.md) for backup and repair behavior. Existing
catalog IDs, profiles, projections, jobs, ownership, Jellyfin targets, dirty work
and refresh attempts survive upgrade. Upgrade alone enables no new network actions.
Connection and binding records belong to a local profile; program bundles and
manifests do not transfer credentials, live server identities or notification
destinations.

For an existing Plex library, follow **sign in → connect → discover → bind →
publish → run delivery → verify indexing**. Create the output catalog/layout or
projection first using the [getting started guide](GETTING_STARTED.md) or
[projection guide](PROGRAMMABLE_CATALOG.md). Library creation is a separate,
explicit option below.

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
Notification credentials remain environment-only. Browser sign-in itself requires
no database; credential-file connections need no additional credential migration.

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

## Import an existing Plex library into Catabolic

`consumer import` reads Plex metadata and associates it with files already scanned
into Catabolic. It uses the saved Plex connection, including browser-login
credentials. No output binding or new Plex library is required. It reads the
remote server and writes only local catalog decisions when you apply a preview;
it does not download media, publish links, request scans, or modify Plex.

First bind and scan the source locations using the [setup guide](GETTING_STARTED.md).
Then discover the library ID with `consumer discover home`. Create a local JSON
mapping file, `plex-import-map.json`, translating Plex's file paths into Catabolic
source locations and optional relative subtrees:

```json
[
  {"remote_root": "/media/Movies", "location": "seed1", "subtree": "Movies"},
  {"remote_root": "/archive/Movies", "location": "seed2", "subtree": "Films"}
]
```

If `seed1` is bound to `/Volumes/seed1`, the first entry maps
`/media/Movies/Example.mkv` to the inventoried `Movies/Example.mkv` at that source.
Omit `subtree` when the remote root corresponds to the source root. Mappings use
POSIX paths, must not overlap on the remote side, and never infer a match from a
filename or title. Map original media roots to scanned sources, not an output
symlink tree; aliases and symlink resolution are not inferred.

```sh
catabolic --db catalog.db consumer import home --library-id 7 \
  --map plex-import-map.json --limit 100
# Review candidates, preserved_fields, deferred entries, and plan_id.
catabolic --db catalog.db consumer import home --library-id 7 \
  --map plex-import-map.json --limit 100 --apply --expected-plan PLAN_ID
```

Preview does not write the database. Apply refetches the page and checks its plan
against the database/profile, connection, library identity, mappings, remote
metadata and local curation snapshots. If anything relevant changed, preview again.
Files must still match their scanned revisions. Network requests run outside the
local database writer lock. Keep the library stable while paging: offset paging
is not a remote snapshot, and concurrent library edits can move page boundaries.
After such changes, restart at offset zero; unchanged imports are safe to repeat.

Each command reads one page of at most 100 Plex items. If `next_offset` is non-null,
repeat preview and apply with `--offset NEXT_OFFSET`; each page has its own plan ID.
`page_complete` means the page has no deferred entries or apply errors; `complete`
also requires the final page. Partial results return exit code 3. Apply imports
eligible candidates even when other entries are deferred, and reports each
accepted proposal in `imported`. An unchanged repeat creates no new decision.

### Imported metadata and conflict handling

Supported library types are movies, TV (episode files), music (track files), and
photos. Imports include titles, available year/summary/release-date/duration fields,
movie edition labels, and episode or track numbering and parent labels. Multipart
files retain part numbers. Different Plex items remain distinct even when they
share an IMDb or other provider GUID; automatic edition merging is not attempted.

Identity is scoped by Plex server identity, library UUID and item rating key.
Curation proposals and decision history retain the remote path and Plex/provider
GUID evidence. Existing metadata fields are preserved; differing values appear in
`preserved_fields`, and missing fields may be filled. A changed Plex GUID, a file
already associated with another item, conflicting parts, duplicate file paths,
unmapped paths, missing inventory or changed sources are deferred for review.
Fix mappings or curation explicitly, rescan changed sources, and preview again.

This imports file-backed items and their descriptive metadata. Series/season and
artist/album labels are metadata; parent items and structural relationships are
not synthesized. Collections, playlists, watched state, ratings, artwork downloads,
standalone parent records and remote-only files are not imported. Existing TV/music
layouts that require parent relationships still need those relationships curated.
Unsupported or incomplete remote entries are reported rather than silently counted
as imported. Library reads use the [Plex library API](https://developer.plex.tv/pms/) and the
`id:asc` rating-key sort documented in [PlexAPI source](https://python-plexapi.readthedocs.io/en/stable/_modules/plexapi/library.html);
live-server import acceptance remains unverified.

Decisions are durable per file, not atomic across a page. If interrupted, rerun the
preview: accepted files become unchanged, and pending decisions can be resumed.
The import does not mark curation complete, infer output membership, remove local
items absent from Plex, or overwrite manually edited fields. No schema migration
is added for this feature.

## Match existing published files to Plex

Existing catalogs do not need to be reimported. `consumer metadata-match` uses a
consumer binding and an active output mapping to find the exact published file
in Plex. It verifies the owned symlink, selected source revision, server, library,
GUID and remote paths. It reads the library in bounded pages and refuses missing
or ambiguous matches; the default and maximum library bound is 10,000 items.
An incomplete search never becomes a match.

Select a mapping from `mapping list --catalog plex`, then preview and save it:

```sh
catabolic --db catalog.db --json consumer metadata-match plex-home \
  --mapping MAPPING_ID
catabolic --db catalog.db --json consumer metadata-match plex-home \
  --mapping MAPPING_ID --apply --expected-plan PLAN_ID
catabolic --db catalog.db --json consumer metadata-matches --binding plex-home
```

Apply returns `match_id`. Matches are accepted association-evidence proposals,
viewable through `proposal show MATCH_ID`, with their decisions retained in the
existing curation history. Matching does not overwrite item metadata, replace
identities, or rewrite associations or links. Repeating an unchanged match reuses
its record. The listing includes historical matches; changed bindings, mappings,
associations, source revisions, or Plex identities require a new reviewed match.
Use `--limit` and the returned `next_after` with `--after` to page the listing.

The default `--metadata-source association` reads fields from the exact primary
file association. This supports a catalog that groups episode files under one
series item even when Plex's generic video library represents each file as a
movie. It never substitutes the series title or description for missing episode
metadata. Curate the intended per-file fields through `association put` first;
its `--metadata` replaces the association metadata object, so preserve existing
season/episode, provenance, and other fields when editing it.

For a standalone item whose kind agrees with Plex, explicitly choose
`--metadata-source item` during both preview and apply. A mismatched kind rejects
that option. This keeps catalog semantics separate from Plex's representation
without changing either catalog's media types.

## Publish catalog metadata to Plex

`consumer metadata` explicitly writes selected fields to a saved match:

```sh
catabolic --db catalog.db --json consumer metadata home --library-id 7 \
  --match MATCH_ID --field title --field summary --lock-fields
catabolic --db catalog.db --json consumer metadata home --library-id 7 \
  --match MATCH_ID --field title --field summary --lock-fields \
  --apply --expected-plan PLAN_ID
catabolic --db catalog.db --json consumer events --limit 100
```

Review the preview's `plan_id` and each field's before/after value and lock state,
then repeat the same options with `--apply --expected-plan`. Repeat `--match` for
up to 100 files. Results identify the match, association and Plex rating key even
when several files belong to the same catalog item. A partial batch identifies
remaining matches separately.

Previously imported Plex items can instead use `--item ITEM_ID` (repeat for up to
100 items). This mode requires the imported Plex identity and accepted import
provenance for an active primary-file association, and reads item metadata.
It cannot be combined with `--match`. Both modes require the appropriate
connection, library and profile.

Repeat `--field` for each selected metadata key. Supported fields depend on the
**Plex** item kind:

| Plex item kind | Metadata keys |
| --- | --- |
| Movie | `title`, `sort_title`, `original_title`, `summary`, `year`, `release_date`, `tagline`, `studio`, `content_rating`, `edition` |
| Episode | `title`, `sort_title`, `summary`, `release_date`, `content_rating` |
| Track | `title` |
| Photo | `title`, `sort_title`, `summary` |

`release_date` must be `YYYY-MM-DD`; `year` must be an integer from 1 to 9999.
Plex may derive year from the release date. Edition editing requires the Plex
server's applicable entitlement. Selected missing/null values normally fail
validation. Explicit `--clear-field FIELD` clears a selected field, including an
absent date/year; it cannot clear the title. An explicit empty string also clears
other text fields. Text values are limited to 8,192 characters, each encoded
update to 16 KiB, and each plan to 4 MiB.

Existing locks are preserved by default. `--lock-fields` locks every selected
field, even when its value already matches. `--unlock-fields` explicitly unlocks
selected fields; the options are mutually exclusive. Values, lock changes and
clears are part of the reviewed plan. Unselected fields are not sent. Tags,
genres, collections, artwork, ratings and watched state are outside this command.
Source files and local metadata are not rewritten by publication.

Apply rebuilds the plan and rechecks selected local/remote values, locks,
identities and publication evidence before each write. Catabolic metadata writes
from the same database are serialized with a separate advisory lock, without
holding the database writer lock during HTTP requests. Each item is updated
with one request and read back to verify selected values and locks. Batches stop
at the first failure; verified writes are not rolled back. Ordinary scans,
publication, maintenance, and consumer delivery never publish metadata implicitly.

Attempts appear in `consumer events` as `metadata_publish`; the JSON `subject`
contains the plan ID, target, changes, and state. Intent is recorded before the
request. `verified` means read-back matched; `uncertain` means a write may have
occurred without verified completion. An interruption can leave `started`.
Run a fresh preview before deciding what remains to apply. There is no automatic
write retry. Matching values and locks are no-ops. CLI exit codes are 0 for a
completed preview/apply, 2 for invalid/stale plans, and 3 for incomplete application.

Plex does not provide conditional compare-and-set through this integration.
Independent Plex editors or other catalogs can still edit a field between the
final read and PUT. Avoid simultaneous editing of the same fields. Verification
establishes the observed result, not a lasting lock against other editors.

The wire format follows upstream [PlexAPI field editing](https://python-plexapi.readthedocs.io/en/latest/_modules/plexapi/mixins/edit.html)
and [library editing](https://python-plexapi.readthedocs.io/en/latest/_modules/plexapi/library.html).
Live testing on Plex Media Server 1.41.9.9961 verified matching an existing
series-associated file in a generic movie library; title, summary, date and year
publication; locks/unlocks; no-op repeat; stale-plan rejection; and restoration
of original Plex and catalog metadata. Other type/field combinations and failure
cases remain covered by fixtures; see the
[validation record](CONSUMER_VALIDATION.md#metadata-publication).

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
catabolic --db catalog.db consumer run --limit 10
catabolic --db catalog.db consumer bindings
catabolic --db catalog.db consumer verify-indexing cinema-plex --limit 100
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
bindings. An existing output catalog can be bound even without a saved projection. After
applying its layout, use these commands in place of `projection execute`:

```sh
catabolic --db catalog.db sync --catalog cinema --dry-run
catabolic --db catalog.db sync --catalog cinema
catabolic --db catalog.db verify --catalog cinema
catabolic --db catalog.db consumer run --limit 10
```

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
catabolic --db catalog.db inbox list --json
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

## Webhooks and optional human notifications

HTTP webhooks use the standard library and require no Apprise. Configure a
trusted receiver URL in the worker environment, then subscribe a destination:

```sh
export CAT_WEBHOOK_URL='https://your-service.example/catabolic-hook'
catabolic --db catalog.db notify put backend --credential-env CAT_WEBHOOK_URL \
  --event job_completed --event job_failed --apply
catabolic --db catalog.db notify run --limit 100
```

Existing databases need `catabolic --db catalog.db db upgrade` to install
job-event triggers (introduced in schema 30).

Run `notify run` periodically to drain committed events and due retries, or run
the supervised `api worker`, which also drains this queue. Job
transitions enqueue deliveries transactionally; they do not start a background
notification worker. Subscriptions apply to future events in the selected profile.
`job_completed` and `job_failed` (including timeouts) cover analysis and rendition
processing jobs, with a `job_id` for correlation. Cancellation is not a failure event.

HTTP/HTTPS destinations receive a JSON POST with `version: 1`, `id`, `event`,
`profile`, `severity`, `created_at` (UTC database timestamp), and, for job events,
`job_id`. Media paths, titles, results and errors are omitted. `Idempotency-Key`
contains the stable event ID; receivers should deduplicate it because delivery
is at least once. Any 2xx response acknowledges delivery; other responses and
network errors use the existing five-attempt retry policy. Redirects are not
followed. Requests time out after 20 seconds, inside a 30-second worker deadline.
Local HTTP receivers are supported. HTTPS uses normal certificate verification.
CLI-configured URLs remain environment-only and may contain a receiver's secret
query token;
URL userinfo and fragments are rejected. Custom headers and signature-based
authentication are not currently supported. Only operators should configure
these destinations, which can reach the worker's local network.

HTTP registration and per-rendition-request `callback_url` support are described
in [HTTP.md](HTTP.md#http-webhooks-and-request-callbacks-api-160-schema-31).
API-provided URLs are stored privately in the catalog and are not returned.

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

The four consumer events above, `fallback_selected`, `fallback_unresolved`,
`job_completed`, and `job_failed` are supported subscriptions. `projection_updated`
works without any Plex/Jellyfin connection and emits once per verified catalog
batch, independent of how many consumers are attached. Inspect its durable
generation with `consumer publications`; the notification worker rechecks
publication interrupted between ownership commit and event scheduling.
Severity filters and
tags route generic batch summaries; messages omit media titles and paths and say
“scan request accepted”, not “media indexed”. Rich private content and curation events are deferred. New destinations do not replay old events.
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

Consumer delivery is also exposed in the [work inbox](INBOX.md) as one
`consumer_refresh` entry per delivery group. Repair and exhausted retries appear
in the default actionable list; pending delivery and automatic retries are
waiting work. Disabled work is deferred, and acknowledged generations are
historical. Inbox reads neither refresh a server nor verify indexing.

## Query-driven destination mappings

Version 3 [HTTP destinations](HTTP_QUERY_MAPPINGS.md) map complete SQL rows or
GraphQL connections to pinned OpenAPI operations and the durable delivery queue.

`projection mapping-*` covers remote metadata, all 17 folder targets, the three
CLI import targets, and NFO/OPDS/XSPF export bundles. `mapping-capabilities`
declares the supported operations and query contract for each adapter. Version 1
uses **rows** queries for remote metadata; [version 2](#folder-export-and-import-mappings)
uses complete **selection** queries for publication and imports.

For version 1, the shared engine evaluates columns, constants, JSON arrays and explicit lookup tables;
groups rows by a stable key; and plans, journals and verifies the changes.
Adapters declare which operations they support. Queries remain read-only.
Existing filesystem `projection put/preview/execute` commands retain their
selection/layout contract.

```sh
catabolic projection mapping-capabilities
catabolic --db catalog.sqlite3 query save curated-metadata --definition query.json
catabolic --db catalog.sqlite3 projection mapping-preview --definition mapping.json
catabolic --db catalog.sqlite3 projection mapping-apply --definition mapping.json --expected-plan PLAN_ID
catabolic --db catalog.sqlite3 projection mapping-events
catabolic --db catalog.sqlite3 projection mapping-events --event EVENT_ID
catabolic --db catalog.sqlite3 projection mapping-recover EVENT_ID
catabolic --db catalog.sqlite3 projection mapping-recover EVENT_ID --apply
```

Use the immutable query revision returned by `query save` in `mapping.json`.
For example, a query returning `catalog_key`, `remote_id`, `remote_path`, `title`
and `genre` columns can drive this definition:

```json
{
  "version": 1,
  "id": "curated-movie-metadata",
  "query": "QUERY_REVISION_ID",
  "destination": {"connection": "home", "library": "2"},
  "key": {"column": "catalog_key"},
  "target": {
    "id": {"column": "remote_id"},
    "path": {"column": "remote_path"}
  },
  "fields": {
    "title": {"column": "title"},
    "genres": {
      "column": "genre",
      "lookup": {"genre:anime": "Anime", "genre:documentary": "Documentary"}
    }
  }
}
```

Return the destination's actual item ID and absolute media path, not its display
name. Both are checked remotely, along with server/library identity; subsequent
runs also check the recorded item identity. Imported provider identities or saved
file matches can supply these columns. Mapping definitions do not automatically
resolve titles or convert a series-level identity into episode identities.
Choose an item, association or mapping ID as `catalog_key` at the intended scope.
Include the profile and active-state filters appropriate to the query.

Multiple rows with the same key and target union set values. Conflicting scalar
values, duplicate target IDs under different keys, missing columns and unmapped
lookup values fail before writes. `{"constant":["Curated"]}` supplies a fixed set;
`{"column":"genres_json","json":true}` decodes an SQL JSON-array column. A string
becomes one set member; null is not an empty set. More complex joins, relationship
selection, filtering and transformations belong in the saved SQL query.

| Destination | Scalar fields | Set fields |
| --- | --- | --- |
| Plex | Existing metadata publisher fields, subject to media-kind restrictions | `genres`, `labels` on movies/episodes |
| Jellyfin | `title`, `sort_title`, `original_title`, `summary`, `year`, `content_rating` on movies/episodes | `genres`, `tags` |
| Both | Existing collection membership through a separate definition | `collection_ids` with explicit collection scope |

Tags have no implicit destination meaning. Map only the namespaces you intend to
publish. Plex labels can affect sharing restrictions; they are not interchangeable
with Jellyfin tags. Existing field locks are preserved. Jellyfin metadata updates
round-trip editable metadata and verify unselected fields as well.

For collection membership, set `destination.collections` to the existing remote
collection IDs being explicitly adopted, and use **only** `collection_ids` in
`fields`, for example `{"collection_ids":{"constant":["123"]}}`. On Jellyfin use
its collection UUIDs. Supply the same item ID/path target as a metadata mapping.
A query can select by series relationship, taxonomy, or any other catalog evidence.
Collections are identified by ID, never looked up or created by name. Plex smart
collections and incompatible media types are rejected.

Set reconciliation preserves preexisting/manual members. Catabolic owns only
members it added, and removes only those contributions. To release contributions,
keep the target in the query and explicitly map an empty array. Review the preview's
`removals` and pass `--max-removals N` when applying. Missing query rows or fields
retain previous contributions; empty/offline selections never clear a destination.
Scalar fields use reviewed replacement and reject subsequent remote edits rather
than overwriting them. A different mapping cannot take over an owned field.
Keep the definition's `id` stable across query revisions.

Every apply requires the exact preview `plan_id`. Stale query results, connection
changes and remote changes invalidate it. Writes are serialized with the existing
metadata publisher on the same database, and checked again before each target.
Remote APIs do not provide atomic compare-and-swap; external edits can still race
a write. Readback detects mismatches but does not automatically overwrite or undo
them. Failed or interrupted attempts block further mappings in that library until
`mapping-recover` can establish the complete before or after state. Recovery only
reads the destination; `--apply` records its finding locally. A mixed/changed remote
state remains uncertain and requires inspection and manual reconciliation against
the recorded before/after state (`mapping-events --event EVENT_ID`) before recovery
can finish. Earlier verified targets in a
batch remain applied.

Initial bounds: 1,000 complete query rows, 100 targets, 100 requested members per
set, 20 explicitly adopted collections, fewer than 1,000 members per collection,
and a 4 MiB plan. Truncation fails before writes. Definitions are JSON files with
pinned saved query revisions; automatic scheduling, portable program-bundle
inclusion, collection creation/deletion, and remote playlists are not supported
by these commands. Additional destination types can implement the adapter
snapshot/validate/write/verify contract without adding mutation to the query layer.


## Folder, export and import mappings

Version 2 definitions connect the existing publication/import owners to the same
`mapping-preview`, `mapping-apply`, `mapping-events` and `mapping-recover` commands.
They consume an immutable saved **selection** query (`item_id`, `file_id`, or
`association_id`, using SQL or supported GraphQL selections). The catalog provides
metadata; the destination's existing preset or format defines supported fields.
Arbitrary remote metadata edits are still available only through adapters that
declare that operation. `target list` also reports each target's mapping operation.

| Adapters | Operation | Result and verification |
| --- | --- | --- |
| All 17 folder presets, including Emby, Kodi, Navidrome, Audiobookshelf and Komga | `folder` | Managed symlinks; source/output identity checks and filesystem readback |
| calibre, Calibre-Web, Immich | `import` | Staged copies sent through the official CLI; successful submission recorded, remote metadata not verified |
| `nfo`, `opds`, `xspf` | `export` | New immutable bundle containing format artifacts, selected media links and a manifest; exact file/link readback |

For example, save this as `selection.json`, then use the returned query revision
ID in a mapping definition:

```json
{
  "version": 1,
  "mode": "selection",
  "entity": "item_id",
  "selection": {
    "language": "sql",
    "query": "SELECT item_id FROM catalog_items WHERE kind='movie'"
  }
}
```

```sh
catabolic --db catalog.sqlite3 query save movies-for-emby --definition selection.json
catabolic --db catalog.sqlite3 catalog bind emby --root /existing/empty/output
catabolic --db catalog.sqlite3 projection mapping-preview --definition emby.json
catabolic --db catalog.sqlite3 projection mapping-apply --definition emby.json --expected-plan PLAN_ID
```

`emby.json`:

```json
{
  "version": 2,
  "id": "movies-for-emby",
  "query": "QUERY_REVISION_ID",
  "destination": {"adapter": "emby", "operation": "folder", "catalog": "emby"}
}
```

The folder must already have an output binding. The engine owns an internal saved
layout and projection binding, and refuses to take over another projection or a
fallback-managed catalog. Preview stages database changes inside a rolled-back
transaction and does not publish files. Apply pins the complete layout and
reconciliation plan, then delegates mutations to the existing filesystem journal.
Unsupported associations skipped by a preset are reported in the layout preview;
missing required naming metadata blocks publication. Existing manual files are
preserved. Folder mappings currently require symlink catalogs.

Folder membership follows the complete query. Removals require
`--max-removals N`; clearing a previously populated selection also requires
`"allow_empty":true` in `destination`. Neither option bypasses source checks or
incomplete-selection rejection. Recovery with `--apply` can finish authorized
filesystem operations while refusing changed configuration or unreviewed actions.

For an export, use an existing source catalog and a **new** bundle path:

```json
{
  "version": 2,
  "id": "kodi-movie-snapshot",
  "query": "QUERY_REVISION_ID",
  "destination": {
    "adapter": "nfo",
    "operation": "export",
    "catalog": "movies",
    "path": "/existing/parent/kodi-snapshot"
  }
}
```

`opds` additionally requires `"base_url":"https://books.example/"`; `xspf` accepts
an optional base URL. Selected associations must already appear in the source
catalog. Format exporters retain their existing kind/field rules. Exports never
overwrite an existing unowned directory. Identical repeats verify the recorded
bundle and do no writes; changed inputs require a new output path. Recovery can
create missing files only inside the directory whose identity was recorded at
creation. Changed files, unexpected entries, or replaced directories block it.
Source revisions must still match. No existing file is overwritten or deleted.

For imports, change the adapter and destination accordingly:

```json
{
  "version": 2,
  "id": "selected-books-to-calibre",
  "query": "BOOK_QUERY_REVISION_ID",
  "destination": {
    "adapter": "calibre",
    "operation": "import",
    "catalog": "books",
    "path": "/existing/parent/calibre-library"
  }
}
```

Use `calibre-web` for the calibre library consumed by Calibre-Web. For `immich`,
`path` is the server URL, and the existing `IMMICH_API_KEY` environment credential
is required for preview, apply, and import recovery. Catabolic reads `/api/users/me`
(the key needs permission to read the current user) and scopes submission history
to the server URL and user ID. Rotating a key for the same user preserves history;
changing users requires a new reviewed plan. Older URL-only submission records
block publication because their account cannot be safely inferred. Select only
supported primary associations; unsupported
kinds/roles fail before launch. Books group all selected formats in one CLI call;
photos/videos use one asset per call. Private staging copies protect source files.

A successful CLI run records **submitted**, not remotely verified. Repeating a
submitted source revision skips it, including completed groups from a partial
batch. Changed revisions of already imported groups are rejected because these
adapters do not implement updates. Missing executables or credentials fail before
launch. An uncertain external outcome blocks further imports to that destination.
Inspect the destination before explicitly recording either `submitted` or
`not_applied`:

```sh
catabolic --db catalog.sqlite3 projection mapping-events --event EVENT_ID
catabolic --db catalog.sqlite3 projection mapping-recover EVENT_ID --resolution submitted
catabolic --db catalog.sqlite3 projection mapping-recover EVENT_ID --resolution submitted --apply --expected-plan RECOVERY_PLAN_ID
```

The resolution is recorded as operator-attested evidence. `not_applied` permits a
new reviewed attempt; it must not be used when the destination might already have
received the media. Recovery never automatically repeats an uncertain import.

Version 2 plans are bounded to 4 MiB; folder plans allow at most 1,000 detailed
changes/actions and import plans at most 1,000 selected files. Selection-query
limits apply independently. Existing `layout`, `projection`, `target import` and
`export` commands retain their contracts. Mapping definitions remain explicit JSON
files; automatic scheduling and inclusion in portable program bundles are not
implemented by these commands.
