# Output consumers: implementation decisions

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Baseline: clean `main` at `ffff429befbf6be7577aac5502ab04fd2ff5e3a8`,
fetched origin on 2026-09-08, database schema 16.

## Existing flow and retained owners

Queries, rules, layouts and projections retain membership and processing ownership.
`Reconciler.apply` publishes normal sync, projection execution and maintenance;
`CatalogRefresh.run` calls it after artifact/receipt/evidence completion. Recovery
replays the same journal executors. Symlink and hardlink executors atomically
commit ownership and remove journal intent. This is the shared change boundary. An observations-version trigger additionally
records size/mtime/device/inode changes to already-published sources: changing
bytes behind an unchanged symlink must not be lost. It uses existing ownership
and active mappings, and the same verification/delivery gate.
Layout application alone changes desired mappings, not consumer-visible files.
Manifest/statistics writes and retained hardlink internal paths are not new media.
Export bundles and calibre/Immich imports have independent, explicitly requested
destinations; they do not represent maintained consumer bindings.

Legacy `Refresh` is Jellyfin-only, captures configured targets in dirty/event
records, and makes HTTP calls while holding the writer lock. Projection execution
and automatic catalog refresh pass `notify_consumers=False`, suppressing that
legacy path. Preserve those legacy semantics; new explicitly enabled consumer
bindings use the shared ownership boundary independently of that flag.

## Ownership and persistence

Schema 17 adds profile-local consumer connections, immutable binding destinations,
catalog publication generations, binding counters, shared-library delivery leases/attempts, explicit
creation intents, and independent human-notification destinations/deliveries.
Existing refresh targets, dirty events and attempts remain intact. No migration
enables new network side effects or guesses server/library identities. An explicit
Jellyfin connection and binding adopt the shared lifecycle; legacy commands remain.
Programming bundles and manifests do not export connections or live bindings.

Each ownership change records the catalog publication generation and increments
affected enabled subtree bindings in that same transaction. Durable journal intent covers interruption before that transaction.
Verified whole-catalog publication advances eligible counters; this conservative
gate can defer a healthy subtree when another part of its catalog is unhealthy.
Before a delayed scan, reverify local roots, source targets, ownership and bindings.
Coalesce by verified server identity and library identity, including separate
connections to the same server. Hold a group while any enabled member has
unverified changes. Acknowledgements apply only to the claimed counters and
binding revisions. Changes during I/O stay pending. Busy remote scans defer work
without acknowledging it. HTTP acceptance is not indexing verification.

SQLite leases serialize library requests. Network work uses short Store sessions,
never an open writer transaction or long-held writer lock. Publication registers
a bounded drain for Store close (the normal CLI command boundary), after closing
the SQLite connection and writer lock. A foreground worker/scheduler processes
delayed retries; no unmanaged daemon is launched. Outcomes are separate from
publication health. A lost response can repeat a scan: delivery is at least once.

## Adapter and setup decisions

Use small direct HTTP adapters over the existing cancellable HTTP subprocess,
not a PlexAPI dependency. This retains bounded DNS/TLS/response deadlines and
header-only credentials without importing account discovery or broad mutation
APIs. A typed capability contract admits unsupported/inconclusive outcomes.
Research: [Plex API](https://developer.plex.tv/pms/),
[PlexAPI source](https://python-plexapi.readthedocs.io/en/latest/_modules/plexapi/library.html),
[server discovery](https://python-plexapi.readthedocs.io/en/latest/_modules/plexapi/server.html),
[Apprise library](https://appriseit.com/library/quick-start/).

Plex scans use the section refresh endpoint without `force`; metadata refresh,
trash emptying, deletion and global preference writes are not exposed. Server
machine ID and library UUID/key are pinned. Creation requires explicit apply,
server-discovered choices, durable intent and an observed pre-create identity set.
An uncertain POST is never automatically replayed: reconcile matching new remote
identity or report ambiguity. No library is auto-created because discovery is empty.

Paths are declared POSIX server namespaces with component-aware subtree mapping.
Binding records pin the local output root identity. Server-reported roots do not
prove shared storage or symlink target access. Optional bounded path-based indexing
verification supplies separate evidence. Whole-section scans only initially.

Apprise is optional and isolated in a bounded subprocess. Local environment
references identify individual destinations; no URLs or remote config are imported
or exported. Publication events do not require a consumer binding, and one catalog batch emits
one publication summary even with several consumers. The notification worker can
recover verified publication after an ownership/scheduling crash. Per-destination
retries avoid replaying successful recipients. Explicit
subscriptions/tags and severity filters route generic summaries. Notification
failures never generate recursive notification events.

## Validation boundary

Core tests use deterministic loopback protocol fixtures and injected failures.
Live Plex acceptance requires an explicitly authorized disposable library/root;
no production tokens or libraries are inferred from this machine. Record live
evidence separately from protocol tests. Preserve existing Jellyfin and media
acceptance lanes, frozen interchange contracts and populated migration fixtures.

## Plex wire-contract correction (2026-09-08)

Reviewed clean checkout `e3081ca` against the current
[Plex reference](https://developer.plex.tv/pms/) and
[Python PlexAPI source](https://python-plexapi.readthedocs.io/en/latest/_modules/plexapi/library.html).
The reference currently describes section refresh as POST and creation `type` as
an integer. Python PlexAPI differs: `LibrarySection.update()` calls the section
refresh endpoint without `force`, using
[`PlexServer.query()`'s default GET](https://python-plexapi.readthedocs.io/en/latest/_modules/plexapi/server.html#PlexServer.query).
`Library.add()` POSTs library-kind strings such as `movie` and `show`, with
`location`, rather than converting the type to a numeric media ID.

Catabolic follows these Python PlexAPI requests: GET for a normal section scan,
POST with a string library kind for creation. Numeric scanner/agent discovery
and media-search parameters retain their separate contracts. This is a client
compatibility choice in the presence of conflicting documentation, not evidence
that all Plex versions reject POST scans or accept numeric creation types.

Both local protocol fixtures enforce the chosen contract. Regression tests reject
POST scans, forced refresh parameters and numeric creation types; outbound tests
check GET scans and creation of movie, show, artist and photo library kinds.
The pre-fix adapter fails those outbound tests. No live Plex server was contacted;
real-server acceptance remains unverified and requires an explicitly authorized
disposable server/library. Creation-intent recovery and delivery state are unchanged.

## Optional Plex account authorization

The explicit `plex-login` / `plex-login-complete` pair implements the
[Plex PIN polling flow](https://forums.plex.tv/t/authenticating-with-plex/609370),
also exposed by Python PlexAPI's `MyPlexPinLogin`. Fixed HTTPS Plex endpoints use
the existing bounded HTTP subprocess with verified TLS and refused redirects.
Login stores a stable client identity and resumable, expiring PIN outside SQLite;
completion validates the account token before atomically replacing PIN state with
an owner-only credential. No password, account profile or token is returned in
machine results. Local credential references extend consumer connection storage
without changing existing environment references, notification credentials,
creation intents, bindings or delivery generations. No schema migration or new
dependency is required. Login is separate from explicit server connection setup.

The initial storage backend uses POSIX file permissions, not encryption or an OS
keychain. Login does not auto-discover account resources or launch a browser.
Tests simulate Plex authorization and fault responses; live account authorization
and real media-server acceptance remain unverified.
