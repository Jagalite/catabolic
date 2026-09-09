# Consumer integration implementation and validation

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Baseline: clean `main` at `ffff429befbf6be7577aac5502ab04fd2ff5e3a8`, safely
fetched on 2026-09-08; schema 16. Validation used local disposable fixtures.
No production media server was mutated or production media used as a fixture.

## Implemented boundaries

Queries/rules/projections retain their existing owners. Journal executors record
consumer-visible changes atomically with ownership commit; a source-observation
version trigger also covers changed bytes behind unchanged published links.
Catalog publication generations are independent of consumer binding counters,
so human publication summaries do not require a Plex/Jellyfin connection.
Verification makes events eligible; workers recheck pinned roots, sources,
server/library identities and revisions. Shared-library claims coalesce bindings,
and captured generations prevent old acknowledgements from clearing newer work.

Direct bounded HTTP adapters provide Plex server/library discovery, explicit
existing-library adoption, server-discovered scanner/agent choices, explicit
create-and-bind with durable uncertain-outcome reconciliation, section scans,
activity observation, and optional exact-path indexing evidence with available
Plex identifiers. Unsupported operations are explicit. No targeted scan, forced
metadata refresh, trash emptying, remote deletion or global preference mutation
is implemented.

Jellyfin has the shared connection/binding/section-scan lifecycle. Legacy global
refresh commands and captured targets/attempts remain available; network work now
runs outside the writer lock. Migration does not infer bindings or enable new
network policies. Apprise is optional, has per-destination independent retries,
and receives generic, explicitly subscribed summaries rather than media details.

## Changed files and migration

| Area | Principal files |
| --- | --- |
| Additive schema 17 | `src/catabolic/migrations/017_consumers.sql`, its license sidecar, `migration.py`, `store.py` |
| Publication and source versions | `reconcile.py`, `hardlinks.py`, `consumers.py`; `publication_generations`, `publication_source_paths`, `publication_source_version` in migration 17 |
| Adapter/setup/CLI | `consumer_adapters.py`, `consumer_setup.py`, `consumer_cli.py`, `cli.py` |
| Bounded HTTP and legacy Jellyfin | `http_worker.py`, `network_adapters.py`, `legacy_refresh_delivery.py` |
| Optional notifications | `notifications.py`, `notification_worker.py`, `pyproject.toml`, `requirements/notifications.lock` |
| Discovery/docs | `targets.py`, `catalog_refresh_cli.py`, `documentation.py`, README, catalog-refresh guide, consumer guides and bundled copies |
| Tests | `tests/test_consumers.py`, `test_consumer_publication.py`, `test_notifications.py`; adapted legacy refresh and schema-version assertions |
| Acceptance/CI | `scripts/consumer_acceptance.py`, `notification_acceptance.py`, `plex_acceptance.py`, `.github/workflows/ci.yml` |

Schema 17 preserves existing profiles/catalog IDs, projections, jobs, ownership,
refresh dirty records, captured targets and attempts. New consumer/creation and
notification records are profile-local. Migration does not export credentials or
live bindings. The existing 14 frozen interchange artifacts remain unchanged.

## Executed checks

Environment: macOS 26.5.2 ARM64, Python 3.14.6, FFmpeg/ffprobe 8.1.2. HTTP fixtures
bind only loopback. Full media tests run with host permissions because macOS
sandbox restrictions prevent the existing FFmpeg descriptor-reopen fixtures.
Logs and disposable evidence are retained under `.local-tests/consumers/`.

The final full regression run executed **539 tests: 536 passed, three skipped**
(zscale/HDR conversion, Poppler and Tesseract), in 146.979 seconds. It includes
38 new consumer/publication/notification tests. See `verified-regression.log`.

Executed commands (paths below are repository-relative):

```sh
.venv/bin/python -m unittest discover -s tests -q
.venv/bin/ruff check src tests scripts
.venv/bin/ruff format --check src tests scripts
.venv/bin/python scripts/sync_docs.py --check
.venv/bin/catabolic spec check
.venv/bin/python scripts/check_spec_releases.py HEAD
.venv/bin/python scripts/audit_dependencies.py --report .local-tests/consumers/dependency-audit.json
.local-tests/release-ci/env/bin/python -m build --no-isolation --outdir .local-tests/consumers/dist
.local-tests/release-ci/env/bin/python -m twine check --strict .local-tests/consumers/dist/*
.venv/bin/python scripts/check_distribution.py --dist .local-tests/consumers/dist --report .local-tests/consumers/distribution-report.json
.local-tests/consumers/installed/bin/python -m pip check
.local-tests/consumers/apprise/bin/python -m pip check
git diff --check
```

The installed environments use hash-locked runtime requirements; the second adds
the optional notification lock. Wheel installation uses `--no-deps`. Packaging,
formatting/lint, guide synchronization, frozen contracts, dependency consistency
and the OSV audit passed. The audit found no advisories for the pinned packages.

Acceptance commands use the installed executable/interpreter, not `PYTHONPATH`:

```sh
.venv/bin/python scripts/consumer_acceptance.py --cli .local-tests/consumers/installed/bin/catabolic --root .local-tests/consumers/verified-wheel
.venv/bin/python scripts/notification_acceptance.py --cli .local-tests/consumers/apprise/bin/catabolic --root .local-tests/consumers/verified-notifications
.venv/bin/python scripts/acceptance.py --cli /Users/jagatranvo/Projects/Catabolic/.local-tests/consumers/installed/bin/catabolic --root .local-tests/consumers/verified-media
.venv/bin/python scripts/experience_acceptance.py --python /Users/jagatranvo/Projects/Catabolic/.local-tests/consumers/installed/bin/python --root .local-tests/consumers/verified-experience
.venv/bin/python scripts/experience_evaluator.py --root .local-tests/consumers/verified-experience
.venv/bin/python scripts/storage_acceptance.py --cli /Users/jagatranvo/Projects/Catabolic/.local-tests/consumers/installed/bin/catabolic --report .local-tests/consumers/verified-storage.json
```

The existing media journey passed 198 CLI commands, including real FFmpeg,
symlink/hardlink publication, processing/renditions, receipt completion, source
preservation, recovery and unchanged repeats. The collection journey passed 84
commands and its independent evaluator: correct publication, stable repeat,
recovery and preserved sources. Actual cross-filesystem rejection, unmount
preservation and explicit remount rebinding passed with no cleanup errors.

Consumer acceptance passed 28 CLI commands through installed machine output, discovery, binding,
publication, repeat stability and explicit creation/reuse. Two changed batches
produce two section scans, with no unchanged-link rewrite. Real Apprise acceptance
passes 18 commands against local ntfy protocol endpoints: three total requests
cover one successful destination and a failed destination's retry; the successful
destination and accepted scan are not repeated.

Deterministic tests cover duplicate library names/identity matching, preview and
creation uncertainty, namespace traversal/Unicode, multiple consumers/shared
libraries, busy scans, updates during discovery and request execution, interrupted
filesystem/ownership/scheduling, lease expiry/stale acknowledgements, source and
destination outages, retry/rate-limit/authentication/exhaustion states, binding
changes and replacement identities, independent notifications and their recovery,
secret-free transfer, populated legacy migration, and actual writer-lock release
at protocol requests. Observed source-version changes do not rewrite symlinks.

## Real-server evidence and limits

**Real Plex is unverified.** No authorized disposable Plex server was supplied.
The runnable `scripts/plex_acceptance.py` requires an explicit disposable-create
flag, expected server identity, NEW fixture root, server-visible mount, library
name, scanner/agent choices and a credential reference. It generates actual video,
creates one scoped library, checks exact symlink-backed paths, publishes a second
item, and verifies safe repeat/resume. The harness itself was executed twice
against a protocol fixture, with two scans and one creation; this is protocol
validation, not real Plex evidence. It retains the remote library for inspection.

Live Jellyfin acceptance could not run: `docker info` reports no Docker socket.
The existing Docker acceptance lane remains in CI; legacy and shared Jellyfin
protocol tests pass locally. Local Apprise used its real pinned package but no
external notification accounts. Existing optional-tool skips remain explicit.

Deliberate limits: whole-section scans; conservative whole-catalog health gating;
bounded first-page path indexing (which can be inconclusive); no remote playback
proof or generation-causal indexing claim; no account/browser discovery; no
Jellyfin creation/indexing/activity capability; no rich/private notification bodies
or processing/curation subscriptions yet. Scanner/agent choices must be discoverable
from the configured Plex server. Delayed retries require the documented foreground
worker or scheduler. Delivery is at least once, including possible duplicates
after a lost response. Unobserved external source edits are not filesystem-watched.

See [setup and recurring-operation examples](CONSUMERS.md) and
[design/migration decisions](CONSUMER_DESIGN.md).

## Plex metadata import validation (2026-09-09)

`consumer import` adds bounded Plex-to-Catabolic metadata intake through
`plex_import.py`, the existing consumer connection, and curation proposals and
decisions. It requires explicit path mappings to scanned source files and a
reviewed plan ID for apply. It adds no database migration or Plex write operations.

The full local regression run executed **589 tests: 586 passed, three skipped**
in 130.050 seconds. The final focused import run passed all **19 tests** after
correcting the outbound sort to `id:asc` and adding per-proposal budget validation.
Lint, formatting, bundled-doc synchronization, interchange specification and frozen
release-artifact checks passed. Regression output is in
`/private/tmp/catabolic-plex-import-regression.log` on the development machine;
that temporary log is not a packaged artifact.

Import fixtures check read-only preview, CLI preview/apply, mandatory plan IDs,
local and remote plan invalidation, server/library replacement, GUID changes,
metadata preservation, exact source mapping, unavailable/changed files, conflicting
associations, duplicate paths, multipart files, edition separation, pagination,
repeated imports, interrupted-batch recovery and source preservation. Every mocked
remote request verifies that the local database writer lock is available.

The fixture supplies synthetic XML at the HTTP adapter boundary; it is not live
Plex evidence. Real account/library import, server-specific pagination behavior,
and real Plex metadata variations remain unverified. Imported TV/music parent
labels are metadata only: parent items and structural relationships, playlists,
watched state, artwork and remote-only files remain outside this import scope.
