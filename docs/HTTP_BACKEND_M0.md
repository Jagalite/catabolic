# HTTP backend: M0 repository review and contract decisions

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Status: implementation and local qualification in progress through M7.
The executable interface and deployment contract are documented in [HTTP.md](HTTP.md).
Baseline checked: `main` at `e24a3b3`, database schema 20, clean working tree
before this document. This is a companion to the owner's supplied HTTP backend
implementation plan, not a replacement or a claim that its milestones are done.

## Product contract

An optional interface must let an authenticated application query authorized
catalog state, request an owner-approved durable rendition, and retrieve an
eligible existing result. CLI and HTTP must share catalog, operation, job,
rendition, trust, projection and consumer services. Processing demand is distinct
from a permanent rule and from downstream publication.

Initial exclusions remain adaptive playback transcoding, HLS sessions, uploads,
remote-URL ingestion, raw encoder arguments, public registration and a player UI.
Plex account credentials never authenticate a Catabolic API caller.

## Confirmed repository seams at the baseline

| Area | Current implementation | Required change before exposure |
| --- | --- | --- |
| Database sessions | `store.py`: writable construction acquires a process lock until close; read-only construction begins a transaction | Close every session before streaming/waiting/executing tools; translate contention into a bounded retryable API failure |
| Writer contention | `database_io.py`: nonblocking `flock`; SQLite busy timeout 5 seconds | Preserve immediate CLI contention semantics unless deliberately changed; add an explicit HTTP wait budget rather than busy looping |
| Local render | `artifacts.py`: `_run_one` encodes, validates and hashes while its Store remains open | Split capture/claim, detached execution, fenced publication and completion |
| Artifact recovery | `Artifacts.recover`: resumes publishing records and marks other unfinished artifacts interrupted | Distinguish a live leased attempt from abandoned work before changing lock lifetime |
| Analysis jobs | `Processing.run`: explicitly relies on Store lock; resets running non-render jobs to queued | Apply the same ownership rules to analysis and retry paths, not just FFmpeg |
| Existing external workers | `processors.py`: generation, opaque lease token, expiry, heartbeat and fenced transitions | Reuse the fencing pattern without duplicating external processor jobs or changing their receipt contract |
| Source access | `source_access.py`: validates an inventoried source via an open descriptor before and after use | Introduce a shared content admission service; stream the admitted descriptor without reopening a path |
| SQL | `sql_query.py`: SQLite authorizer currently permits reads from `main` and `temp`; stable-selection restrictions serve a different purpose | Add an HTTP-specific table disclosure policy, including underlying reads through views/aliases; keep arbitrary SQL operator-only |
| GraphQL | `graphql_query.py`: QueryContext and shared field resolver expose paths and JSON evidence | Add resource/field authorization beneath resolution, including nested counts and relationships |
| Invocation paths | CLI, rules and maintenance call existing processing/artifact services | Migrate all local execution entry points together; no HTTP-only runner bypass |
| Dependencies | Only `notifications` and `dev` extras exist | HTTP framework/server must be an optional locked extra; CLI-only installation remains valid |

## Execution contract to implement first

A local claim references an existing job and attempt, plus a monotonically
increasing generation, opaque lease token, worker identity, lease deadline,
source revision, immutable operation/tool configuration, policy revision and
pinned destination. Each attempt retains its own temporary output.

1. Claim eligible work in a short writer transaction and reserve its budget.
2. Close the Store before source analysis, encoding, hashing and output validation.
3. Heartbeat through short independent sessions. Lease loss prevents publication.
4. Reopen a writer session; revalidate the exact claim and captured inputs before
   committing a publication intent.
5. Let the existing recoverable artifact publisher complete that intent, with
   ownership checks at every transition that can otherwise race recovery.
6. Atomically record a valid winning result, fulfillment and durable events.

An expired attempt may have spent CPU or left an owned partial file, but cannot
publish over a successor. A crash after publication intent requires recovery of
that intent before conflicting work can win. Cancellation invalidates authority
to commit; killing a process alone is not the correctness mechanism.

The first acceptance test must hold a real disposable render open while another
process performs a catalog write and CLI recovery. The write must proceed within
its budget; recovery must preserve the live attempt. Then expire/replace the
claim and prove that the stale attempt cannot register or publish a result.

## Authorization and disclosure contract

Every access decision combines principal, action, resource membership, field
policy, conditions and budget. Default denial applies to unspecified fields and
resources. Processing permission can permit server-side use of an original while
forbidding its download.

Dynamic membership pins an owner-approved saved-query revision; snapshot grants
pin an approved bounded ID/revision set. Both remain subject to current revocation.
Caller-created query definitions can only narrow access. Partial authorization
selections fail closed. Saved SQL reports require explicit disclosure approval;
ordinary report execution must not imply a security boundary.

REST and GraphQL must use the same authorized services. Job visibility belongs to
caller demand, not every shared worker job. Events and replay use current access,
not only authorization at subscription time. Security tables, credential hashes,
tickets and private audit state must be inaccessible to HTTP SQL through any
alias or view.

## Content contract

Admission resolves a catalog resource and requested revision into an authorized,
safely opened descriptor. It closes its catalog session before yielding bytes.
A stale revision never selects a newer representation implicitly. HEAD and Range
admission use cheap revision checks, not an unexpected whole-file digest.

Initial streaming supports GET/HEAD and single byte ranges with bounded buffers,
conditionals and an explicit multi-range policy. That policy, validator strength
and cache rules must be finalized in M4 contract tests before implementation.
Metadata fingerprints are not strong byte ETags. In-place source mutation can
only abort future delivery, not retract bytes already sent.

Opaque browser tickets pin one revision, a principal/grant, permitted GET/HEAD,
expiry and transfer budget. They are reusable for seeks and checked on every
request. The policy for already admitted transfers must be stated explicitly.
No path-based route, temporary-file serving, or implicit source exposure is allowed.

## Demand and event contract

A request pins source and operation revisions and retains its principal and
idempotency scope. Reuse of a key with a different normalized body conflicts;
replay still checks current access. Multiple demands can reference normal shared
work. Cancelling one demand must preserve work required by another demand or rule.
Resource reservations must be transactionally enforced rather than inferred from
storage estimates.

Request states are queued, running, validating, ready, blocked, failed, cancelled
and stale. Progress is absent when unknown. A ready rendition, healthy projection,
accepted consumer scan and delivered notification remain independent outcomes.

Polling is authoritative. The later SSE endpoint replays a durable authorized
outbox with bounded retention and an explicit resync-required response. No idle
subscription may retain a database session. CLI-originated transitions must enter
the same outbox in their committing transaction.

## Threats and acceptance evidence

| Threat | Required evidence |
| --- | --- |
| Resource/field disclosure across principals | Adversarial REST, nested GraphQL, counts, raw JSON, copied cursor and event replay tests |
| Broad SQL access to security state | Direct table, aliased table, nested view, subquery and schema-discovery denial tests |
| Stale or revoked authority | Token/ticket expiry, grant replacement, dynamic membership and direct CLI edit tests |
| Path/revision substitution | Parent symlink, remount, rename/replacement, truncation and in-place mutation injection |
| Duplicate or stale execution | Competing claims, quota races, crash windows, active-worker CLI recovery and stale completion tests |
| Shared-work cancellation leakage | Two principals sharing work, one cancelling/revoked, independent result visibility |
| Unbounded resource use | Measured stream buffering, disconnect cleanup, query budgets, request/byte reservations and rate limits |
| Deployment exposure | Authentication on loopback, explicit listeners/proxies/origins, optional-extra clean-install tests |

## Milestone state and ordering

| Milestone | Current state | Gate |
| --- | --- | --- |
| M0 | Repository review, threat model and executable request/response schemas recorded | Authenticated OpenAPI and HTTP guide define the implemented contract |
| M1 | Implemented; local FFmpeg fencing tests pass | Concurrent writer and CLI recovery preserve live work; expired/cancelled claims cannot publish |
| M2 | Implemented; adversarial token, grant, expiry and field tests pass | Resource grants, explicit exposure, private content/SQL denial |
| M3 | Implemented; scoped REST/GraphQL, SQL, saved selections and snapshots tested | Principal-bound cursors and revocation checks |
| M4 | Implemented; exact range, mutation, disconnect and memory tests pass | 32 MiB streamed with at most 64 KiB chunks and traced peak below 4 MiB |
| M5 | Implemented; shared work, restart, cancellation and quota rollback tested | Durable requests use existing jobs and one reservation per job |
| M6 | Implemented; durable polling/SSE and reviewed definition edits | Replay expiry/principal tests and preview rollback/stale-plan tests pass |
| M7 | Optional package, foreground deployment and installed journey implemented | Local macOS journey and full regression passed; Linux/macOS CI qualification pending |
| M8 | Deferred | Sessions/OIDC/delegation without weakening token deployments |

A read-only deployment can be qualified after the applicable M2–M4 gates; request
admission stays unavailable until M1 and M5 pass. Server startup must not migrate,
expose sources, start workers or enable public listening implicitly. Migration
numbers must be allocated against the actual schema at implementation time.

## Baseline evidence and outstanding decisions

The preceding local regression at this development baseline ran 589 tests in
130.050 seconds, with three skips and no failures. Its retained log was inspected
at `/private/tmp/catabolic-plex-import-regression.log`; it was not rerun for this
initial documentation review. This is neither HTTP validation nor Linux/packaged
acceptance evidence.

The initial review required verification of official FastAPI/Uvicorn interfaces
and the deployed SQLite runtime before selecting dependencies or concurrency defaults.
Those choices are now documented in HTTP.md. WAL remains an
unmade deployment decision, not a substitute for claims or single-writer budgeting.
Finalize grant field sets, source exposure defaults, resource reservation accounting,
lease/recovery transitions, range/cache behavior, event retention and Problem
Details codes in executable contract tests as each milestone begins.


## Implementation qualification record

The first full HTTP-enabled regression ran 611 tests in 409.482 seconds with three
skips and one failure: the existing CLI-import interruption startup marker was not
observed. That test passed alone immediately afterward (0.531 seconds). This is
recorded as a failed run, not a clean regression. A final run follows the remaining
security and quota fixes.

The focused suite subsequently passed 31 tests in 7.865 seconds, covering shared
real FFmpeg work, stale claims, scoped metadata, descriptor streaming, disconnected
clients, expired tickets, private SQLite/credential bytes, event cursors, operator
plans, storage rollback and cancellation quota enforcement. Local installed-wheel
acceptance exercised eight external-client checks across 13 commands, including
server/worker restart, exact PNG range bytes and original-download denial.

The dependency lock audit returned no OSV advisories. The local SQLite runtime is
3.53.3; WAL remains disabled. No production media, live Plex/Jellyfin account,
source trust override or public listener was used. Linux/macOS installed CI lanes
are part of release verification; their results must be recorded before calling
M7 release-qualified.

Implemented scope choices: operator HTTP routes edit validated definitions but do
not publish a projection or run a rule backfill; those retain existing CLI plan
workflows. Strong immutable content snapshots, sessions/OIDC and adaptive playback
remain outside this release. Event batches carry current authoritative request
state rather than promising a historical progress log. Cancelled demand retains
quota while its shared job is unfinished; cancellation does not delete results.


The final full local regression passed 620 tests in 261.565 seconds with three
skips. Subsequent response-contract checks passed 28 tests, and the final
body-limit/stream transport check passed all 24 HTTP metadata/content tests.
Distribution validation compared 196 packaged files against source; wheel and
sdist metadata passed strict Twine checks. The installed environment's dependency
check passed. The final transport test adds one test after the full run; CI will
run that exact final revision.
