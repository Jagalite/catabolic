# Shared observation and named watchers

Baseline: main 87f4551, schema 23, clean working tree. This is an implementation
ledger; acceptance is recorded separately from intended contracts.

## W0 decisions

Saved queries already own immutable definitions, composition and typed selection
completeness. SQL and GraphQL remain their executors. Resolver already owns ranking,
lineage, isolated live probes and epoch fences. FallbackProjection owns membership,
layout and journaled publication. No new selector or writer is needed.

The missing supported boundary is a profile-bound evaluation session with shared
budgets/cache and explicit access context. Membership and all candidate/sidecar
queries must consume that session. Rows and documents retain structural ordering,
column identity and duplicate multiplicity; only selection IDs are set-valued.

Projection references pin authoritative configuration, not a copied membership or
policy. A policy alone never defines membership. One prepared projection plan must
reach publication; its epoch and configuration are revalidated before applying.

Scanning currently stages on disk but walks under Store's lifetime writer lock.
Authoritative scans need detached traversal and per-source fenced claims before
observation sharing. Only complete full-source coverage (including exclusions) may
mark inventory missing. Offline/failed observations retain inventory. Freshness is
measured from traversal start; dirty watermarks acknowledge only captured events.
Generated locations retain registered-artifact-only scanning.

Coverage defaults conservatively to all locally authorized profile sources,
including for empty selections. Definition dependencies are not filesystem coverage.
Inventory, availability, hash evidence and live candidate checks remain distinct.

Named watchers are opt-in local-owner automation initially. Read-only previews do
not claim, enable, observe, or advance stability. Projection ownership is exclusive
for automatic mutation; legacy refresh paths must skip/delegate owned projections.
Manual execution retains the existing publication lock and recovery semantics.

Schedules coalesce missed runs and triggers; no backlog replay. Cron uses explicit
IANA timezone and five fields. Repeated DST wall minutes run once; nonexistent wall
minutes are skipped. Fixed intervals use persisted wall deadlines and bounded
catch-up. Shared observation completion never advances an unrelated schedule.

Durable run/reaction state is separate from last complete semantic result. Failed
queries never replace it; failed reactions remain retryable without a new change.
SQL rows and GraphQL documents cannot implicitly become operational selections.
Portable definitions exclude credentials, leases, health, winners and activation.

## Validation ledger

Implementation and acceptance pending. No production watchers or media are used.

## Implementation ledger

- W1: `evaluation.py` supplies profile/catalog-bound sessions, cancellation, shared
  composition cache, aggregate ID/byte/deadline budgets and authorization context.
  `plans.py` describes typed query, membership-plus-fallback, and authoritative
  fallback-projection references; conservative coverage and structural comparisons
  stay separate from execution. Resolver's external private `_select` calls are
  removed. `FallbackProjection.apply_prepared` publishes one epoch-fenced plan.
- W2: `observations.py`, migration 024 and the existing scanner share completed and
  in-flight full-source observations. Detached staging retains IDs and complete-scan
  missing semantics. Generation, policy, coverage and PID/lease checks fence stale
  publication. Four live claims and isolated scanner waits bound slow-source work.
  Scan completion and material catalog invalidation share the publishing transaction.
- W3: immutable named definitions, independent manual/interval/cron/events schedules,
  coalesced generations, retry state, one active run and supervised processes.
- W4: explicit projection ownership transfer; fallback/catalog refresh workers skip
  owned outputs, completion events route through the scheduler, and legacy
  maintenance refuses owned output reconciliation. Manual watcher execution uses
  the same projection lock/journal. Legacy `watch` still scans/processes as before.
- W5: typed meaningful baselines, durable reaction checkpoints, per-context fallback
  stability, failed-result preservation and explicit planned/retained/verified
  projection history. No-op reconciliation no longer advances publication counters.
- W6: operator-only typed HTTP 1.2.0 and frozen artifacts, generated watcher client,
  inactive version-3 program imports, optional bounded Watchdog event adapter,
  packaged guide, CI installed acceptance and three-watcher release harness.

Supported reactions retain reports, emit structured notifications, reconcile
fallback projections, or admit approved native analysis/render operations. No
encoder is executed by a watcher reaction. Source monitoring remains conservative;
no dependency inference from previous query results or SQL text was introduced.

## Deliberate initial limits

General plan orchestration executes as explicit local owner. Scoped application
watcher execution is not implemented; HTTP management requires operator authority.
Projection plan references currently require an authoritative fallback binding;
legacy copy/rendition projections retain their existing commands. Exact full-source
coverage sharing is implemented; subsumption, targeted scans and keyed row diffs
are not. Native registrations are established at supervisor startup; periodic
schedules remain necessary for lost events, newly bound roots and network mounts.
Cloud feeds and external processor reactions are not included. Durable semantic
history is retained indefinitely; no automatic destructive retention is enabled.

## Validation

The full regression suite passed **693 tests, three skipped, in 217.255 seconds**.
This includes native disposable filesystem-event checks and the 100-watchers / ten
sources test, which recorded exactly ten authoritative scans. The final maintenance
ownership guard was added afterward and receives a separate focused regression run.

Installed macOS and Debian Bookworm arm64 journeys passed for wheel SHA-256
`de44519e2c9ffe16acc513287d364966763e6692e4a5b45e1a81a8f4f458768d`.
They use real FFmpeg renditions, separate Plex/browser preferences, later audit
reuse, source outage, stable failback, process restarts and generated REST/GraphQL
clients. Existing exact content URL/range tests pass in the same installed journey.
The final maintenance-guard build is qualified separately below.

The optional source trust settings, credentials and media used here are disposable
fixtures. No production source or catalog was changed and nothing was pushed.

### Final qualification

Concurrent supervisor tests exposed probe writer-lock contention after detached
reads. The shared probe now uses bounded writer waits; watcher fallback/projection
evaluation holds publication ownership through its prepared reaction, while scans
remain outside that lock. In-flight observations and catalog contention record a
short waiting retry. Polling `supervise --once` preserves fresh compatible evidence;
continuous/native startup still invalidates coverage to repair monitor gaps.

Final regression: **695 tests, three skipped, 220.501 seconds**, exit 0. The new
concurrent two-projection test and killed-scanner uncertainty test pass. Focused
watcher/fallback gates: 32 tests in 20.198 seconds. Scale remains 100 named watchers,
ten overlapping sources, exactly ten authoritative traversals.

Final wheel SHA-256:
`70a3fa25da6ec759b558f7c983a71eef2907a46097657f8b5b94ae8cb576855f`.
Source archive SHA-256:
`e2dbee732c375f45a17d41837d2efe6dca6fe54853e3bbc1c24b86d0d86369fb`.
Both installed macOS and Debian Bookworm arm64 journeys pass on that wheel, including
real A/B/C/D media, stable failback, exact ranged content, generated clients and
supervisor execution of a durably admitted HTTP watcher. Native-event unit coverage
ran on macOS; Linux installed acceptance used polling.

Ruff lint/format, generated TypeScript clients, HTTP 1.2.0 artifact checks, preservation
of all twelve prior published artifact files, bundled documentation, CI YAML parsing,
and wheel/sdist source-content verification (243 package files) pass. Existing
committed migration checksums and published 1.0.0/1.1.0 artifacts are unchanged.

Evidence logs are `/private/tmp/watcher-final-regression.log`,
`/private/tmp/watcher-macos-qualified.log`,
`/private/tmp/catabolic-watcher-linux-evidence/acceptance-qualified.log`, and
`/private/tmp/watcher-qualified-distribution.json`. These paths contain disposable
qualification evidence, not production catalogs. Work remains uncommitted on main.

## Review fixes

Explicit run admission now has its own durable `requested_generation` in the
uncommitted migration 024. Completion acknowledges only the captured generation,
so a request arriving during execution survives completion and process restart.
Disabling clears automatic retry eligibility and pauses explicit demand while
retaining pending reaction evidence. Scheduled workers recheck eligibility before
claiming, including workers dispatched before a disable. Explicit admission or
re-enabling can resume pending reactions through the existing publication owner.

Saved SQL reports now enforce the evaluation session's HTTP table restrictions
and operator or approved-report authority. GraphQL documents use the authorized
catalog context; HTTP document execution without that context fails closed.
Rows/documents account result bytes inside `Queries.run`, avoiding a second charge
in plan orchestration. Selection references also retain caller authorization.

The review regressions cover overlapping admission/restart, disabled failed
projection retries, scoped document results, SQL authority, referenced selections,
restricted tables and aggregate report bytes. The 26 focused watcher tests pass.
The rebuilt wheel passes the installed macOS watcher/fallback/generated-client
journey; Linux acceptance has not been repeated for this follow-up.

Final follow-up validation: `python -m unittest discover -s tests -q` ran 699 tests
in 213.652 seconds, passing with 3 skips. Ruff, formatting, `git diff --check`,
installed HTTP 1.2.0 artifact verification and dependency checks pass. The wheel
and source archive package-content check verified 243 files. Evidence:
`/private/tmp/watcher-fix-full.log`, `/private/tmp/watcher-fix-tests.log`,
`/private/tmp/watcher-fix-acceptance-absolute.log`, and
`/private/tmp/watcher-fix-distribution.json`. The first acceptance invocation used
a relative interpreter path and failed to launch after the harness changed cwd;
the absolute-path rerun passed. No production sources, commits or pushes were used.
