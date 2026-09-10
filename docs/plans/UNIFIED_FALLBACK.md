# Unified fallback resolution: repository baseline and implementation contracts

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Scope: user authorized implementation through F6. The baseline below records the
starting point; implementation uses migration 023 and additive API 1.1.0.
See [the implemented interface](../FALLBACK.md) and the validation record below.

## Verified baseline

Reviewed checkout: `560e37d`, following `0c5ac38`. Database schema: **22**.
The new public API contract is **1.0.0**. Its OpenAPI/GraphQL release directory
must remain unchanged when fallback fields are introduced. The existing full
suite passed 642 tests with 3 skips during API-contract qualification; fallback
qualification needs its own regression and installed acceptance evidence.

| Boundary | Current implementation | Consequence |
| --- | --- | --- |
| Saved selections | `Queries.select` returns complete typed IDs; SQL/GraphQL execution is bounded; HTTP uses `_http=True` | Reuse execution and completeness errors, including composed queries. Candidate ordering is not query row order. |
| Copy preferences | `CopySelection.filter` groups primary associations by item and part, ranks recorded observations/facts | Extract reusable scoring. Fallback-enabled projections must bypass the old final copy-selection pass. |
| Membership/layout | `Layouts._plan` expands associations, applies publication and copy policy, then names selected files | Materialize logical slots before physical selection. Exact-file selections stay exact unless explicitly converted. |
| Mapping identity | `Application.mapping_id` includes catalog, file, item and destination, but not profile | Do not repurpose or rewrite old mapping IDs. Add profile-local effective entries with separate logical identity. |
| Ownership | Layout ownership lives in `meta` at `layout-catalog:<catalog>`; link ownership is profile/catalog/path scoped | New fallback ownership must be profile-local and must not overwrite shared layout intent. |
| Publication | `Publication.candidates` invokes `ready`, which validates the original and recursively validates ancestry | Split accepted output revision evidence from live/current source evidence. Existing callers retain strict behavior. |
| Reconciliation | `_catalog_delta` omits blocked files from desired paths and can emit `remove` for their owned links | Unresolved logical members must be explicitly retained. Do not pass an empty candidate set as desired membership. |
| Projection gate | `Projections.run` rolls back mappings if any source is blocked | Retain this conservative whole-projection gate initially. |
| Filesystem recovery | `_execute` journals target replacement, checks owned link text, uses a temporary symlink and `os.replace` | Extend this path; do not build another publisher. Preserve ownership checks without reopening a dead previous source. |
| Slow work | Writable `Store` owns the writer lock; `Store.detached` refuses an active transaction | Candidate capture must finish before live probes. Revalidate catalog fingerprints after detaching. |
| Maintenance | Preflight opens every source; an incomplete scan blocks processing/output work | Existing maintenance cannot detect an outage and then switch projections. A bounded fallback pass must be independently reachable under explicit opt-in. |
| HTTP content | `opened` authorizes and opens one requested file/revision; content tickets pin both | A resolution changes the next concrete reference, never an admitted stream or existing ticket. |
| Processing | Jobs and execution claims pin source snapshots and fence publication | Resolve before admission. A failed attempt does not substitute another source. |

## Proposed persistent boundaries

Implementation allocates migration **023** against the reviewed schema 22 checkout. Keep all existing records and migrations unchanged.

- Immutable fallback policies and their saved-query dependencies. A policy
  revision belongs to a profile and validates every referenced query as a complete
  selection contract. Imported definitions remain unbound.
- Explicit profile/catalog policy bindings and membership-query revisions. A
  file-ID membership selection cannot acquire substitution semantics implicitly.
- Logical entries keyed by profile, catalog, item, role, component/part and selected
  variant. Store chosen file/revision/tier separately from desired membership.
- Resolution generations and bounded history. Distinguish proposed selection,
  journaled publication and verified publication.
- Health observations and supervised scheduling state. Preview reads these records
  but cannot advance counters or timers.

Profile-local effective mappings must feed the existing reconciler without being
written over shared `mappings`. Recovery must use the same effective-mapping
adapter and generation fence as ordinary application. Explicit historical mappings
continue through the existing adapter.

## Resolver contract

The reusable batch service accepts logical slots, one pinned policy revision,
consumer constraints and a captured previous-resolution state. Exact file requests
bypass fallback. It returns complete typed decisions and a revalidation digest.

Policy tiers are ordered, bounded to 32, and reference immutable query IDs. Query
results are cached once per batch, including reused IDs in composed selections;
lower tiers are evaluated only while entries need them. A failed or incomplete
query blocks resolution instead of becoming an empty tier.

Candidate capture records association identity, item, role, part, variant,
representation coverage, file revision, source binding/trust revision, applicable
output evidence and policy exclusions. Eligibility never follows titles or arbitrary
item relationships. Multipart or cross-edition ambiguity blocks substitution.

Scoring uses extracted copy preference rules within a tier. A tier outranks all
later tiers. Without explicit deterministic tie-breaking, usable equal-ranked
candidates produce an ambiguity blocker.

Live probes consume immutable snapshots with no catalog session or writer lock.
Use isolated, bounded helpers with a global/source cap and suppression after a
failure. A timed-out helper is not assumed to have stopped: retain its slot until
it is reaped; never queue an unlimited backlog behind a stuck mount. Normal probes
must not hash or invoke FFmpeg.

Output validity, recorded lineage, known source currentness and live ancestry are
separate evidence fields. `accepted_source_revision` permits an explicitly accepted
historical output when its own live revision and evidence match; it never reports
that an offline original was verified. `current_source_revision` requires current
source evidence and fails on unknown currentness. Legacy readiness stays strict.

Before commit, revalidate membership, policy/query revisions, binding/trust,
association/evidence snapshots, prior generation and relevant output ownership.
Reject stale manual plans. Automated retries create a fresh bounded generation.

## Publication and automation contract

An unresolved member stays desired. Retain its owned output and block the initial
whole-projection publication pass. Intentional membership removal remains separate
and requires the existing removal authorization.

A same-path switch is a journaled symlink replacement. A format change creates and
verifies the correctly named new path before retiring the old owned path; each step
is recoverable but the group is not claimed to be atomically visible. Reject
unsupported hardlink switches. Dependent sidecars require selected-representation
compatibility; ambiguous groups block publication.

Failover escapes an unusable current candidate immediately. Failback is immediate,
stable or manual as explicitly configured. Stable counters survive restarts and
advance only on committed maintenance checks. An unchanged successful pass performs
no filesystem mutation and sends no repeated transition notification.

Conservative dirty invalidation and debouncing are preferable to inferring SQL
dependencies. The supervised fallback pass may run despite unrelated source
outages, but must honor its retarget/removal/probe budgets and valid output root.
Only verified healthy publication changes feed existing consumer delivery. Consumer
failure is independent of selection and publication success.

## Public API and portability contract

Introduce additive fallback API/GraphQL fields in a new **1.1.0** artifact directory,
retaining all 1.0.0 artifacts and operation IDs. Reuse the typed models, exporter,
release guard and generated-client acceptance tooling from `560e37d`.

A policy-aware logical resolve returns a concrete authorized file/revision and
sanitized explanation. Policy approval is separate from file access. HTTP candidate
queries use restricted SQL execution and current grants; copied cursors or tickets
must not widen scope. Content routes keep their exact-byte behavior.

Processing admission invokes the same resolver, then uses ordinary job/cache
identity and exact claims. Portable definitions include query/policy dependencies,
not credentials, health state, active bindings or machine-local winners.

## Required milestone evidence

| Gate | Evidence required before claiming completion |
| --- | --- |
| F0 | Current selection, profile, blocked-source and recovery invariants captured in regression tests; proposed contracts reviewed |
| F1 | Ordered complete tier execution, deterministic ties, bounded queries/candidates, explicit identity/coverage blockers |
| F2 | Live checks outside sessions; slow-source containment; historical output eligibility without false source verification |
| F3 | Profile isolation, stable membership, ownership-preserving retarget, offline retention and crash recovery |
| F4 | Restart-persistent failback, flapping and budget tests, supervised scheduling, independent consumer delivery |
| F5 | Scoped HTTP and generated clients; concrete source admission; no in-stream or mid-attempt substitution |
| F6 | Supported multipart/sidecar and format transitions; final installed A→B→C→unresolved→B→stable A journey on Linux/macOS |

No fallback policy, source exposure or automatic retargeting is enabled by this
review or by a database upgrade.

## F0 validation record

On this checkout, the existing query-layout, programmable-catalog, rendition,
maintenance and HTTP-contract groups passed **66 tests**. Four new tests in
`tests/test_fallback_baseline.py` also pass and capture:

- Exact file membership does not expand to an available backup.
- Legacy mapping identity changes with the physical file and is not profile-local.
- A blocked source appears as a removal candidate in the raw reconciler delta, but
  the projection gate retains mappings and the previously owned link.
- An active transaction cannot detach; a detached session releases the writer lock
  for an independent writer before reacquiring it.

These were baseline compatibility proofs before implementation. Current fallback
regressions cover resolver ordering, evidence, scoped HTTP, failback persistence,
retention and publication recovery. Installed release evidence is produced by
`scripts/fallback_acceptance.py`; CI repeats it on Linux and macOS. No production
catalogs or source media are used by these tests.

## Implementation and qualification record (2026-09-10)

The implementation covers F1–F6 using migration 023 and HTTP contract 1.1.0.
The supported release boundary is symlink publication, same-item curated
roles/variants, explicitly coherent multipart groups and compatible sidecars.
Cross-edition substitution and adopting existing exact mappings remain explicit
migration work: use a separately configured logical projection. No upgrade
silently changes an exact selection into fallback intent.

| Gate | Implementation and proof |
| --- | --- |
| F1 | Immutable policies, lazy complete selections and shared copy scoring; ordering, ambiguity, candidate budgets and query-count regressions |
| F2 | Isolated bounded helpers; current output evidence and separately reported source currentness; real FFmpeg offline-original and historical-input tests |
| F3 | Profile-local logical entries/effective mappings, retained unresolved output, journaled retargets and verified publication generations; profile, ownership, stale-preview and crash tests |
| F4 | Explicit supervised worker and catalog-refresh integration, persistent failback health and transition notifications; restart/flapping/manual-failback regressions |
| F5 | Scoped REST/GraphQL resolution and pre-admission processing; policy approval does not grant content access; exact/logical demands share ordinary cache identity; generated clients |
| F6 | Container-extension transitions, supported role/part coverage, inactive portable imports, packaged documentation and installed Linux/macOS journeys |

Validation:

- Full repository suite: **667 tests, 3 skips**, passed on macOS with host access
  for disposable socket and FFmpeg descriptor fixtures. Final targeted evidence,
  processing, HTTP, identity and lifecycle group: **16 passed**.
- Installed fallback journey: passed on macOS and Linux (Debian Bookworm arm64
  container), Python 3.14.6. Both used the wheel with SHA-256
  `74d7e995288462fc78a65636644f6129877b9c7dd61248663c93c71eab22cab8`.
- A → B → C → unresolved → B → briefly healthy A (retain B) → stable A passed.
  Unresolved generation 4 retained publication generation 3; recovery advanced
  publication only after verified completion. Old file/revision URLs stayed exact.
- Existing installed HTTP request/restart/ticket journey passed on both platforms;
  generated REST and GraphQL clients compiled and completed real HTTP requests.
- Ruff checks/format, distribution inspection (222 package files), bundled-guide
  synchronization, current schema export verification and preservation of all
  prior HTTP/interchange release artifacts passed.
- Linux/macOS CI now includes the installed fallback journey. These local results
  do not claim that an unpublished commit has passed remote CI.

Disposable logs/reports are under `/private/tmp/catabolic-fallback-*`; no real
catalog, media collection, credential or downstream library was used. The
reproduction entrypoints are `scripts/fallback_acceptance.py`,
`scripts/http_acceptance.py`, and the `tests/test_fallback_*.py` groups.

## Review corrections

The subsequent review identified and fixed three issues:

- Recovery distinguishes a completely evaluated obsolete fallback intent from an
  incomplete or failed query. It cancels unapplied work only after checking output
  ownership, the unchanged original destination, and any temporary replacement link.
  The retained owned link remains subject to the normal removal allowance.
- An unchanged API worker's heartbeat timestamp no longer invalidates resolution.
  Worker identity/capability changes and ordinary catalog changes remain fenced.
- The change limit is also enforced against actual reconciliation actions before
  filesystem publication, so repairs cannot bypass it when selections stay unchanged.

Six targeted regressions in `tests/test_fallback_review.py` pass, including
incomplete-query and foreign-ownership refusal. The full corrected suite passes
673 tests with three skips. The corrected wheel also passes the installed macOS
fallback journey, including generated REST/GraphQL clients, exact content URLs,
ranges, restart recovery, and persistent stable failback. Its SHA-256 is
`fdcfce76be19ab3c74bbee7212dd90a850c6c75850fcce1cb15299bf6c42ed13`.
The Linux installed-wheel qualification above predates these review corrections.
