# Catabolic: Python-to-Rust Migration Plan

**Decision:** Proceed with a staged, compatibility-first rewrite of standalone Catabolic. Use Statelessness to explore, check, and replay Catabolic-owned decision logic. Do not replace Catabolic's persistence, filesystem journal, execution claims, or API contracts with a generic workflow engine.

**Prepared:** October 9, 2026  
**Catabolic baseline:** `453fca983222c6665a48775eef67c489c96b527e`  
**Statelessness baseline:** `cb7f256e4d7cf20cfab6018b9bbba391ec2dd9de`  
**Assessment level:** Repository inventory plus selected implementation, test, contract, packaging, and CI review. This is not an exhaustive code audit or an executed migration. Neither repository's test suite was run for this assessment. A local checkout attempt failed because the container could not resolve its configured network proxy; connected GitHub reads supplied the source evidence.

All new crate names, harness commands, budgets, milestones, and architecture below are proposed. Existing paths are identified separately. A current-source reference identifies observed behavior, not proof that the behavior is correct under every environment.

## 1. Executive decision

A Rust rewrite makes sense as an investment in a reusable native catalog engine, predictable resource ownership, native distribution, and explicitly testable lifecycle decisions. It does **not** make sense as a promise that merely changing languages will dramatically accelerate scanning or media processing.

Catabolic already delegates important work to SQLite and external media tools. Its disk-backed scan staging already bounds memory. Much of the benefit must therefore come from better application structure, lower per-record overhead where profiling demonstrates it, and a reusable Rust library—not assumed improvements to FFmpeg or filesystem latency. [S3, S5, S6]

The recommended end state preserves the standalone product, including Python installation and documented Python access. It adds a native Rust library and executable; it does not turn Catabolic into a Motion-only component. Motion could consume the resulting library later, but that is a separate integration project.

Three rules govern the migration:

1. **Freeze behavior before replacing implementations.** Create a complete parity ledger and an executable comparison harness against the pinned Python release.
2. **Migrate complete ownership boundaries.** One implementation owns each mutation and its transaction, journal, claim, and recovery path. Do not divide a live operation across competing Python and Rust stores.
3. **Separate migration from redesign.** Preserve the current database format, external contracts, output ownership, and security semantics first. Optimize and redesign only after equivalent behavior is demonstrated.

A full native rewrite is approved in principle, but each subsystem receives a go/no-go gate. Missing optional features remain visible blockers to a full-parity claim; they do not disappear because the core scanner has been rewritten.

## 2. What the repository actually requires

### 2.1 Baseline facts

The implementation declares **database schema 32**. The HTTP implementation declares **contract 1.7.0**. The Python package declares version **0.2.0**, Python **3.11 or newer**, and optional watch, HTTP, notifications, and OpenAPI dependencies. Existing CI runs unit tests on Linux and macOS with Python 3.11 and 3.14, and includes installed-wheel, external-media-tool, consumer, notification, filesystem, and HTTP acceptance work. [S2, S3, S4, S6]

Some architecture and migration prose describes older schemas. In particular, the architecture overview describes schema 16. Resolve conflicts using implementation, frozen contracts, and executable tests; do not use a stale architecture overview to reduce the migration scope. [S1, S2]

The migration inventory includes features added late in the schema history: guarded observations, playback sessions, media-header checks, job notifications, HTTP webhooks, and HTTP query mappings. The source tree also contains fallback resolution, watchers, media components and packages, document extraction, Plex integrations, and consumer publication. [S4, S7]

### 2.2 Feature parity ledger

M0 must turn this initial inventory into a machine-readable ledger. Each externally observable operation needs an identifier, owner, current entry point, public inputs and outputs, mutations, failure behavior, reference tests, Rust tests, differential fixtures, and completion status.

| Area | Required migration scope | Existing anchors |
|---|---|---|
| Product and administration | CLI commands, flags, defaults, JSON and text modes, exit behavior, docs/spec access, initialization, status, maintenance, backups, diagnostics, capability detection | `cli.py`, `maintenance.py`, `documentation.py`, distribution and release tests |
| Storage and upgrades | Schema 32, all original migration bytes, migration hooks, database identity, advisory locks, read snapshots, transaction/savepoint semantics, recovery eligibility, backup rehearsal | `store.py`, `database_io.py`, `migration.py`, `migrations/`, `test_migrations.py` |
| Source identity and inventory | Profiles, locations, bindings, remounts, strict/path trust, source exposure, guarded scan budgets, partial coverage, continuation, freshness barriers | `app.py`, `source_trust.py`, `remount.py`, `observations.py`, `scan_*`, guarded-scan and observation tests |
| Catalog and curation | Files versus logical items, associations, revisions, targets, tags, metadata, proposals, decisions, work inbox, import/export and interchange | media model, item workflow, tagging, targets, manifest and interchange tests |
| Query and definition engine | Read-only SQL, SQLite views/UDFs, GraphQL, saved immutable queries, rules, projections, selection, layouts, program bundles, estimates and calibration | query, SQL, GraphQL, programmable catalog, program bundle, rule and projection tests |
| Filesystem publication | Managed symlinks/hardlinks, output definitions, ownership markers, reconciliation, ordering, durable mutation journal, recovery, retained outputs | `filesystem.py`, `reconcile.py`, `hardlinks.py`, `outputs.py`, storage acceptance |
| Media analysis | Probing, decode/header health checks, metadata extraction, text/document extraction, capability failures and bounded subprocess handling | enrichment, media-health, probe-health and document-text tests |
| Processing and artifacts | Processors, workers, attempts, receipts, artifacts, render validation, images and other existing presets, cancellation, retries and revision evidence | processor, artifact, output and native-processing tests |
| Renditions and components | Caller demand, shared processing, independent publication, component inventory/selection/lineage, package construction, source pinning | rendition workflows, component lifecycle/media/migration/package tests |
| Automation | Durable execution claims, supervisors, watcher policies/schedules, recovery, coalescing, refresh generations, external work | execution-claim, watcher, watcher-recovery and catalog-refresh tests |
| External publication and consumers | Existing consumer adapters, Plex login/import/matching/metadata, Jellyfin behavior, destination/publication mappings | consumer and Plex tests; real consumer acceptance scripts |
| Fallback resolution | Policy revisions, candidate eligibility and evidence, logical-to-concrete choice, retries, publication generations, identity and authorization | all `test_fallback_*` suites |
| Playback | Session admission and cancellation, reuse/sharing, worker lifecycle, HLS cache/segments/playlists, expiry, source/auth pinning | `playback*.py`, `test_http_playback.py` |
| Notifications and outbound HTTP | Job notifications, optional Apprise behavior, partial delivery, webhooks, rendition callbacks, approved OpenAPI operations and mapped deliveries | notifications, HTTP webhooks/query mappings, destination mapping and notification acceptance tests |
| HTTP and clients | Contract 1.7.0, authentication/grants, tickets, snapshots/cursors, bounded queries, events/SSE, requests, operators, transport errors and generated clients | `http/contract.py`, HTTP tests, `tests/clients/`, HTTP acceptance |
| Distribution and embedding | `pip install catabolic`, optional extras, documented Python contracts, package data, native executable/library, release provenance and exact-artifact acceptance | `pyproject.toml`, distribution/release tests, CI |

Do not treat the table as an exhaustive command inventory. The ledger is complete only after every CLI parser branch, HTTP operation, exported schema, documented Python entry point, integration adapter, and migration hook is accounted for.

## 3. Compatibility contract

### 3.1 Required compatibility

**Data compatibility:** The first compatible Rust release reads and writes schema 32 with the same supported data meanings. Preserve database IDs, item/file IDs, revision identities, canonical hashes, metadata types, migration checksums, journal records, claims, and historical relationships. Keeping a schema number alone is insufficient if values or recovery semantics change.

**CLI compatibility:** Existing scripts retain command names, arguments, defaults, environment/configuration discovery, JSON structures, exit statuses, and stdout/stderr separation. Freeze documented text and diagnostics that tests or scripts depend on. Intentional changes require a separately reviewed compatibility decision.

**HTTP compatibility:** Preserve paths, methods, operation IDs, status codes, headers, envelopes, nullability, pagination, principal visibility, idempotency, content delivery, and authorization rules. Continue shipping the frozen historical contract artifacts; do not regenerate old releases using a new framework.

**Filesystem compatibility:** Existing output roots and ownership markers remain valid. Rust must recognize and recover Python-created pending operations, and the rollback qualification must establish which Rust-created states Python can safely recover.

**Python compatibility:** Preserve installation, the CLI entry point, and the documented/imported public surface classified in M0. Python tests that access internal SQLite connections do not automatically establish that those internals are public APIs. Existing public access must not be dropped simply because the CLI works.

**Operational compatibility:** Preserve cancellation, retries, resource limits, partial results, missing-tool behavior, secret redaction, lock ownership, and upgrade/recovery refusal conditions.

### 3.2 Non-goals for the parity release

Do not introduce a new database, an event-sourced replacement schema, a plugin platform, a remote executor, a new HTTP API version, a different media pipeline, new mandatory runtime services, or a merged Motion product. Do not promise Windows support without qualification. Do not replace FFmpeg, Poppler, or Tesseract simply because the coordinator is being rewritten.

A safety defect discovered in the Python oracle is not a behavior to reproduce unquestioningly. Record it as an explicit exception, add a regression, and preferably fix the reference implementation as well. No silent changes under the label of parity.

## 4. Proposed Rust architecture

```text
Native CLI             Python package                 HTTP server
    |                 thin PyO3 facade                     |
    +------------------------+-----------------------------+
                             |
                     Catabolic runtime
          admission, transactions, executors, recovery
                    /                    \
           Catabolic core             Boundary adapters
      domain + pure decisions       filesystem / tools /
                    |               network / watchers
                    +---------+-------------+
                              |
                       Catabolic store
             existing SQLite schema and contracts

Verification adapter -> same production decision functions
                     -> Statelessness checks/search/replay
```

### 4.1 Workspace boundaries

| Proposed crate | Responsibility | Boundary rule |
|---|---|---|
| `catabolic-core` | Domain types, validated values, pure selection/planning/lifecycle decisions, application errors | No Python objects, SQL connections, filesystem operations, sockets, or implicit clock reads |
| `catabolic-store` | Existing schema, migrations and hooks, logical persistence, canonical encodings, SQL sandbox, transaction/claim primitives | Only owner of database lifecycle and transaction policy |
| `catabolic-runtime` | Use cases, admission, execution, recovery, safe filesystem/process/network adapters, worker/watch coordination | Executes explicit effects and turns results into inputs |
| `catabolic-cli` | Compatible command interface and native binary | Delegates business decisions rather than implementing a second engine |
| `catabolic-http` | Compatible transport, authentication integration and client contracts | Uses the same authorization and domain use cases as other surfaces |
| `catabolic-python` | PyO3 extension and public Python compatibility | Thin boundary; no permanent second catalog implementation |
| `catabolic-verification` | Statelessness models, independent oracles, bounded exploration, codecs and replay executable | Calls production reducers; cannot replace them with easier toy replicas |

Keep filesystem/process/network adapters as modules initially. Split further crates only when dependencies or ownership justify them. Avoid a public trait and registry for every private helper.

### 4.2 Initial dependency choices

Use `rusqlite` as the first storage candidate because direct SQLite control is central to existing behavior. Its documented hooks expose authorizers and progress handlers needed for the SQL boundary. Qualify user-defined functions, backup, limits, statement completion, and connection configuration before approving the final feature set. Do not assume an ORM reproduces these controls. [S12]

Use a small, conventional Rust CLI and HTTP stack, but select and pin exact versions in M1. The frozen Catabolic contracts—not generated defaults of a chosen framework—remain authoritative. An async HTTP runtime must not hold SQLite write transactions across network or process waits. Run blocking storage/filesystem work through a bounded execution boundary.

Use ordinary Rust values and typed identifiers internally. Do not pass arbitrary JSON through every internal API merely to make cross-language tests easy. JSON belongs at existing contracts, frozen codec boundaries, and comparison fixtures.

### 4.3 Runtime state ownership

SQLite remains the durable authority. Do not introduce a second persisted aggregate that can disagree with the existing normalized tables.

For a typical durable operation:

1. Read the relevant persisted state and current evidence into a bounded decision snapshot.
2. Run the pure decision function and produce an explicit plan.
3. In the required write transaction, revalidate expected state, revisions, authorization, source evidence, and claim fence; commit allowed state changes with required journal/outbox records.
4. Release the transaction and execute the external effect.
5. Reacquire the store, validate completion against the current fence and revision, and atomically record accepted results and follow-up work.

The concrete filesystem journal may require multiple durable stages. Preserve that protocol rather than force all effects into a generic two-step abstraction. An external request can be delivered even when its acknowledgment is lost; retryable external effects require explicit idempotency policy, not a claim of exactly-once execution.

Maintain one implementation's ownership for a mutation from admission through recovery. Do not share an active Python `sqlite3.Connection` with `rusqlite`, or use two connection owners inside one logical transaction. Cross-language boundaries exchange values or opaque service handles, not transaction internals.

## 5. How to leverage Statelessness correctly

### 5.1 Its actual role

Statelessness lets applications retain their own state, transitions, and effects while the library checks invariants, explores supplied input schedules, records failures, shrinks them, and replays observations. It supports independent oracles and observation of already-executed transitions. It does not supply Catabolic's durable storage, job delivery, filesystem recovery, or production scheduling. The project explicitly describes itself as experimental and bounds its guarantees to the supplied model and search. [S8, S9]

At the reviewed commit, `Cargo.toml` declares version 0.2.0 and Rust 1.90, while the README's install example still uses a 0.1 version constraint. Pin the reviewed commit or a separately qualified published release rather than copy that example blindly. Do not infer that the reviewed 0.2.0 source is already the released crate. [S8, S10]

A proposed pinned dependency for the verification crate is:

```toml
[dependencies.stateless]
package = "statelessness"
git = "https://github.com/Jagalite/statelessness"
rev = "cb7f256e4d7cf20cfab6018b9bbba391ec2dd9de"
```

For a distributable Cargo package, replace Git-only dependencies with a qualified registry release or an appropriately packaged vendored arrangement before publication. Do not make the initial application rewrite depend on a simultaneous Statelessness API redesign.

### 5.2 Shared production decision functions

Use the same reducer in production and in the `Model` adapter. Conceptually:

```text
decide(snapshot, input) -> next decision state + ordered requested effects

runtime:      load -> decide -> validate/commit -> execute -> ingest result
verification: snapshot -> decide -> independent checks -> explore/replay
```

The domain core can return its own `Decision` type; the verification adapter converts it to Statelessness's `Transition`. This keeps framework-specific types out of most application APIs without duplicating policy.

Inputs must carry anything behavior-relevant: logical time, deadlines, attempt IDs, generations, resource identities, authorization changes, cancellation requests, and observed effect outcomes. A guard must not secretly read SQLite or the clock during exploration.

Use actual commit/post-state comparisons to verify that persistence adapters implement the reducer's plan. Otherwise, a perfect pure model could coexist with a broken SQL executor.

### 5.3 Start with bounded lifecycle models

Do not model every catalog row in one enormous global state. Start with small workflow slices, then compose only the interactions whose correctness depends on interleaving. Statelessness observes one state and transition; Catabolic owns any multi-machine composition. The reviewed library does not implement partial-order reduction or automatic state normalization. [S9]

| Model | Relevant inputs | Initial properties |
|---|---|---|
| Observation and guarded scan | Directory batches, completed scopes, resource stops, dirty-generation changes, root replacement, continuation, cancellation | Positive evidence can publish safely; absence requires current complete coverage of the relevant scope; stale continuation cannot publish against a new root/generation |
| Shared processing and caller demand | Request, deduplication, claim, completion, retry, one caller cancels, all callers expire | One caller cannot cancel another's demand; stale attempt completion cannot publish; a process exit is not independent artifact validation |
| Managed filesystem publication | Plan, durable intent, create/replace/remove result, root change, crash/restart | No unowned replacement/deletion; source bytes remain untouched; recovery converges to an allowed state or explicit blocker |
| Watcher and refresh coordination | Dirty events, policy revision, scheduling, lease expiry, partial completion, acknowledgment | Requests are coalesced without losing newer generations; old acknowledgment cannot clear newer pending work |
| Notifications and mapped HTTP | Authorization change, approved operation revision, payload replacement, dispatch, retry, lost acknowledgment | Disabled/unapproved work cannot newly dispatch; unsent supersession works; acknowledged delivery is not claimed as remote processing completion |
| Playback and shared encoding | Admission, authorization/expiry, completed segment, cancellation, encoder exit, cache retirement | No temporary segment is exposed; one session cannot retire another authorized session's needed encoding; stale work cannot serve a changed source revision |

Initial composition tests should combine demand + processing + publication, and observation coverage + watcher scheduling. Separate source-trust and authorization data can be abstracted to a few explicit states without removing the distinctions under test.

### 5.4 Independent oracles and fault controls

Use two independent reference paths:

**Pinned Python oracle:** Execute equivalent external operations against isolated database/filesystem copies and compare observable behavior. This catches accidental migration changes.

**Small independent event ledger:** Advance solely from input, emitted output, and disposition—not the implementation's next state. Check expected results, including required outputs that were omitted. This catches bugs shared by Python and Rust or introduced into a reducer that otherwise agrees with itself.

The framework's own conformance testing does not establish Catabolic parity. Catabolic needs its own versioned scenario corpus and application properties.

Every initial model must detect deliberately injected faults: accepting an old lease, clearing a newer generation, exposing an incomplete segment, accepting a stale reviewed plan, canceling shared demand, or deleting an unowned output. The gate is a detected property failure, a shrunk trace, and reproduction in a fresh process. Passing without fault detection is weak evidence.

### 5.5 Model bounds and trace policy

Start with explicit small domains, such as two callers, two attempts, two generations, one or two source scopes, bounded pending effects, and a finite logical clock. Publish the exact depth, state, edge, seed, byte, and time budgets with every result.

Run breadth-first exploration for small domains, seeded fuzzing for longer sequences, and targeted real-system race tests. A budget stop is not an exhaustive pass. Any progress property must state its scheduler fairness and completion assumptions; do not claim general liveness.

Cheap invariants can be checked in production. Full-state encoding and detailed capture should be optional and bounded. Observe a transition that already occurred using `check_observed`/`Recorder` rather than running the reducer again. An observer failure must not cause a duplicate application operation. [S8, S9]

Trace snapshots must not contain credentials, bearer tickets, sensitive receiver URLs, or unneeded raw media paths. Prefer opaque identities and explicit authorization facts in the model. If redaction removes behavior needed for replay, label the result as incomplete diagnostic evidence rather than exact replay. Trace/checksum files are not authentication mechanisms. [S9]

## 6. High-risk compatibility work

### 6.1 Database migration is not just copying SQL

Preserve all 001–032 migration files byte-for-byte. Their names and SHA-256 checksums are part of validation. Retain explicit migration entry points, the original refusal conditions, consistent backups, rehearsal, integrity/FK validation, and the exact set of recoverable older schemas. Do not automatically migrate when opening for a read or preview. [S2, S3, S11]

Port implementation hooks as well:

| Existing behavior | Required Rust qualification |
|---|---|
| Migration 15's Python UDFs rewrite query definitions and calculate digests | Golden tests for JSON merge precedence, exact encoding and hash bytes |
| Migration 16 uses foreign-key deferral and special validation handling | Rehearse upgrades with real constraints; preserve rollback/refusal outcomes |
| Migration 25 calls component backfill after SQL | Port backfill semantics and compare all resulting relationships and IDs |
| Schema verification constructs an expected schema in memory | Preserve validation of tables, indexes and triggers, not just `user_version` |
| Backup/table digests encode typed Python values deterministically | Reproduce type tags, bytes encoding, nulls, escaping and numeric formatting |
| Detached external work releases then reacquires store ownership | Preserve identity/fence revalidation and bounded reentry behavior |

Ordinary stored JSON uses Python's sorted-key, non-ASCII-preserving, non-NaN encoding. Some digest paths use different compact and ASCII-escaping rules. A default `serde_json` dump is not an approved replacement for either without byte-level fixtures. Preserve old encodings where they define identity; future formats require an explicit version. [S3, S11]

Test every supported prior schema, not an invented assumption that every number permits the same recovery path. Independent legacy SQL fixtures are better than generating all historical databases with the new migrator itself.

### 6.2 Guarded scans and filesystem safety

The current tests demonstrate that readers can see valid batches before overall scan completion; a completed directory may establish absence even while another directory is deferred. Unvisited inventory is retained. Continuation rejects changed dirty generations and root replacement, including under path trust. These are required behaviors, not optional optimizations. [S5]

Preserve coverage granularity, resource stops, source-wide vs directory-scoped failures, freshness barriers, request cancellation, root identity evidence, and continuation semantics. Do not simplify to either “incomplete scans publish nothing” or “anything not seen is missing.”

Use descriptor-relative operations and explicit ownership checks for mutable output roots. Validate root/ancestor identity, symlink behavior, hardlink identity, rename/replace rules, mount changes, and platform case behavior. Keep source roots read-only from Catabolic's perspective; filesystem atime changes caused by reads are not application writes.

Do not loosen the journal protocol because Rust makes resource lifetimes explicit. Safe memory ownership does not establish safe path resolution, correct fsync ordering, or crash recovery.

### 6.3 Query and validation semantics

Freeze SQL view definitions, UDF behavior, profile binding, result columns including duplicate aliases, row ordering where specified, typed values, completeness/truncation, and cursor binding. Retain separate authorization boundaries for local SQL versus HTTP-visible SQL.

Qualify GraphQL parsing/execution behavior, variables, fragments, aliases, nullability, partial errors, bounded connections, and visibility rules. A schema match alone is insufficient.

Inventory all uses of Python `regex`, Pydantic validation, JSON Schema, Unicode case folding and sorting, datetime/number conversions, and Python dictionary merge order. Do not substitute a Rust dependency on the assumption that its name or feature category implies semantic equivalence. An incompatible pattern/validator is a release blocker or a reviewed compatibility exception.

### 6.4 HTTP has existing non-obvious constraints

For contract 1.7.0, retain these especially important behaviors: reviewed-plan digests for definition/mapping application; approval of a pinned OpenAPI operation without network dispatch; private receiver URLs and environment-based credential references; caller-owned cancellation; separate admitted and verified publication generations; authorization-aware replay/cursors; and authorization rechecks. [S4]

Playback is not generic unrestricted streaming: the reviewed contract serves a growing HLS EVENT playlist containing completed segments, with bearer authorization on segment requests. Whole playback segments reject ranges with 416. Do not accidentally add generic range behavior there while correctly retaining existing byte-range behavior for other content endpoints. [S4]

Freeze these details in black-box tests before replacing FastAPI or GraphQL machinery.

### 6.5 External tools and optional Python ecosystems

Keep tool subprocesses as bounded, supervised effects unless a later independent project justifies embedding them. Preserve argument construction, executable discovery, protocol restrictions, environment policy, timeouts, bounded stdout/stderr, cancellation, child-process cleanup, exit classification, temporary-file cleanup, and post-execution validation. Use explicit argument vectors rather than a shell.

A Rust wrapper does not sandbox FFmpeg or a malicious media parser. Maintain current restrictions, then separately evaluate stronger platform isolation where appropriate.

Apprise is a particular parity decision. Existing acceptance work tests optional partial delivery. Preserve the existing optional Python adapter during transition if a native replacement is not equivalent; document that dependency honestly. A native core with a Python-only optional adapter is not a fully Python-free feature set. The final native-parity gate must either replace the supported adapter behavior, ship a deliberately supported compatibility helper, or obtain explicit approval for a narrower product. Quietly deleting providers is not parity. [S6]

## 7. Python and native distribution

A thin mixed Python/Rust package is a viable target: Maturin supports mixed packages and Rust binaries, and PyO3 supplies the native extension boundary. [S13, S14]

Proposed structure:

```text
python/catabolic/
    __init__.py
    cli.py                 # compatibility entry point -> _native
    public_wrappers.py     # actual public modules determined in M0
    py.typed
    ... preserved public modules and package resources ...
crates/catabolic-python/
    Cargo.toml
    src/lib.rs             # exports catabolic._native
```

Keep `pip install catabolic` and existing extra names meaningful. A wheel-installed CLI can call the extension in-process; a separately shipped native binary calls the same runtime. Do not require an HTTP server or IPC hop merely to access the local library.

Classify the Python surface before implementation: documented interfaces must work, de facto entry points get an explicit compatibility decision, and internal test helpers can be replaced by native tests. Do not export a fake `sqlite3.Connection` over a different storage owner to satisfy an internal assertion.

Release the interpreter for long-running Rust-only work using the qualified PyO3 mechanism. Avoid holding a Rust lock while calling Python callbacks that can reenter Catabolic. Define cancellation and exception mappings at the boundary. [S15]

The current pure-Python distribution's portability does not automatically survive conversion to native wheels. M0 must identify supported OS/architecture/interpreter combinations. Build and install-test a wheel for each promised combination; provide documented source-build requirements for anything supported only through an sdist. At minimum preserve the existing Linux/macOS and Python 3.11/3.14 qualification, then cover the architectures promised in the release policy.

Evaluate `abi3` only after proving its API/performance fit. Do not assume ordinary `abi3` covers free-threaded Python; qualify those builds separately if supported. Ensure package resources include all original migration files, guides, interchange releases, HTTP releases, schemas and licenses. Verify native library dependencies, CPU baseline, deployment target and platform tags on the installed artifact. [S14, S16]

## 8. Milestones and acceptance gates

Milestones are dependency-gated, not calendar promises. Re-estimate staffing and elapsed time after M0 counts public operations, tests, platform combinations, integrations, and unresolved semantic gaps. This is a multi-subsystem product migration; counting only scanner modules would materially understate it.

| Milestone | Deliverables | Tests and hard exit gate | Dependencies |
|---|---|---|---|
| **M0 — Freeze reference and scope** | Pinned Python reference artifact/environment; complete parity ledger; supported-platform/public-Python inventory; sanitized legacy DB fixtures; reproducible media/integration fixtures; baseline benchmarks; known-defect register | Run all existing applicable CI/acceptance lanes; retain failures and skip reasons; record CLI/HTTP/resource hashes; every inventoried feature has an owner and executable proof plan | None |
| **M1 — Rust, packaging and verification skeleton** | Workspace, error/value conventions, dependency/toolchain pins, basic native executable, installable Python wheel, Statelessness adapter, one shared-request lifecycle model | Both surfaces execute the same harmless use case; minimum-version wheel imports outside source tree; injected stale-completion/cancel fault produces a shrunk, fresh-process replayable failure; no hidden I/O in reducer | M0 |
| **M2 — Storage and migration compatibility** | Store/locks/snapshots/savepoints, schema validation, migration bytes/hooks, backups/rehearsal, canonical encodings, exact recovery eligibility | Every supported historical fixture upgrades identically; full integrity/FK checks; invalid ledger refusals; byte-level hashes; rollback during every migration stage; Python↔Rust read compatibility; no read-triggered writes | M0, M1 |
| **M3 — Read-only catalog and query surfaces** | Catalog types/views; SQL sandbox/UDFs; GraphQL; read-only selection and query/layout planning; inspection/status APIs | Differential rows/types/order/cursors/errors; read-only and HTTP disclosure attacks rejected; SQLite limits/deadlines; immutable DB/source checks; existing query and media-model tests mapped | M2 |
| **M4 — Definitions, curation and interchange** | Imports/exports, manifests and released schemas, tags/targets/associations, saved query/rule/projection revisions, program bundles, estimates, reviewed plans | Identical revisions/digests and logical DB state; unknown-field behavior preserved; stale-plan refusal; rule completeness and publication eligibility; catalog workflow and interchange parity | M2, M3 |
| **M5 — Observation, source trust and watchers** | Binding/remount/trust; guarded traversal/publication; continuations and fresh barriers; watcher policies, schedules, claims and recovery | Partial-scope absence tests; replaced root and dirty-generation tests; unmount/disk-full/permission/budget cases; watcher state-machine exploration; real Linux/macOS filesystem acceptance | M2, M3; definition dependencies from M4 as needed |
| **M6 — Safe output reconciliation and recovery** | Symlink/hardlink outputs, ownership markers, retained output behavior, publish journal, apply/recover | Crash failpoints around every durable mutation boundary; no unowned overwrite; creates-before-removals where required; source immutability; resume Python journals in Rust and qualified Rust journals in Python | M4, M5 |
| **M7 — Media analysis and processing** | Tool runner, probing/header health, document extraction, processors/attempts/receipts/artifacts, render/image presets, validation and shared demand | Real media plus missing/broken tools; kill/cancel/late completion; receipt/byte-digest checks; lease takeover; preserved sources/prior outputs; production reducer and independent-ledger agreement | M2, M4; M5/M6 for end-to-end publication |
| **M8 — Renditions, components and fallback** | Revision-pinned requests, component inventory/selections/packages, publication, fallback policy/evidence/resolution | Component migration and actual mux/media fixtures; chosen-source retry stability; multiple callers; active/inactive lineage; admitted vs verified generation; all fallback families and rendition workflows covered | M4–M7 |
| **M9 — Integrations, notifications and playback** | Consumer/Plex/Jellyfin adapters, mappings, refresh delivery, optional notification parity, callbacks/webhooks/approved HTTP mappings, playback worker/session/cache | Real consumer acceptance; partial delivery/retry; approved-origin and credential rules; disabled/superseded deliveries; growing playlist/closed segments; shared-session cancellation/expiry; no Python-free claim with unported required adapters | M6–M8 |
| **M10 — HTTP and full Python/native qualification** | Complete contract 1.7.0, auth/tickets/cursors/SSE, all routes, generated clients, stable Python surface, complete wheels/native artifacts | Frozen contract and black-box parity; unauthenticated/cross-principal adversarial cases; TypeScript client compile/run; installed artifacts on every promised platform; complete public Python facade | HTTP work starts after M3; final gate requires M4–M9 |
| **M11 — Shadow, canary, release and retirement** | Evidence bundle, documented recovery/rollback, Rust-default opt-in then stable release, removal of redundant Python core only after gates | No unresolved safety/contract discrepancies; all ledger rows qualified; performance reviewed; crash-recovery and upgrade/downgrade procedure rehearsed; exactly tested artifacts released | M0–M10 |

### 8.1 Required artifacts from every milestone

Each milestone produces a reviewable implementation, updated parity ledger, scenario IDs, passing/failing/skipped test report, benchmark deltas where relevant, and reproducible failure evidence. A fixture changed to make the Rust output pass requires an explanation tied to an approved compatibility decision.

No milestone is complete because `cargo test` passes while its existing installed-wheel, external-tool, or platform acceptance lane remains unexecuted. “Blocked,” “skipped,” “budget-limited,” and “not tested” are distinct outcomes.

### 8.2 Parallel ownership

| Workstream | Main ownership | May begin | Shared interface dependency |
|---|---|---|---|
| A: Storage and compatibility | M2, canonical encodings, migration fixtures, locking | M1 | Owns transaction and storage contracts |
| B: Domain/query/definitions | M3–M4 | M1 with frozen snapshots | Domain values and repository interfaces |
| C: Filesystem/observation | M5–M6 | M1 with a fake store | Filesystem capability boundary; A's journal/claims |
| D: Media/workflows | M7–M8 | M1 with fake tools/effects | Shared lifecycle snapshots; A's fencing; C's publication |
| E: Integrations/transport | M9–M10 | M1 contract fixtures; runtime integration after M3 | Authorization/use-case interfaces, no independent policy copies |
| F: Verification/release | Comparison harness, models, CI, wheels, final evidence | M0 onward | Versioned contracts and independent reference environment |

The integrator owns public protocol changes, shared error types, migration ledger changes, and release gates. Agents should not independently redesign the schema, add incompatible state names, or alter shared canonicalization. Merge vertical slices with integration evidence rather than leave all subsystem integration until M10.

## 9. Detailed test strategy

### 9.1 Differential harness

Run the Python and Rust implementations against **separate** copies of the same logical fixtures. Never let shadow mode mutate the same outputs, database, consumer, or notification destination twice.

The harness has four adapters: CLI process, Python public API, HTTP black box, and deterministic domain scenarios. Seed clocks and identifiers where supported; otherwise map explicitly non-contractual identities consistently. Compare:

- Returned values, errors, exit statuses, HTTP status/headers and emitted events.
- Logical database rows, relationships, revisions, journals, ownership and pending work.
- Filesystem names, contents, link targets, ownership markers and hardlink relationships.
- External effect intents and observed calls to controlled fake services.

Normalize only explicitly nondeterministic values, such as approved timestamps or generated fixture roots. Do not normalize away state transitions, completeness flags, array order, canonical IDs/hashes, authorization decisions, cancellation ownership, or stale-fence rejection. Compare hardlink equivalence relationships rather than requiring inode numbers to match across independent fixture copies.

Direct-import Python unit tests cannot all be pointed at a Rust binary unchanged. Keep public-facade tests, parameterize black-box tests, and port internal tests to Rust with a ledger mapping to their original purpose. A removed Python test must have a traceable replacement or a documented reason.

### 9.2 Contract and correctness matrix

| Test family | Required scenarios | Evidence/gate |
|---|---|---|
| CLI | Every parser branch; flags/defaults; bad input; JSON/text; interruption; output separation; docs/spec commands | Baseline/Rust comparison and command coverage ledger |
| Storage | Every supported old schema; migration hooks; bad checksums; foreign-key failures; backup with active WAL; disk full; interrupted upgrade; database identity changes | Expected schema and typed row digests agree; refusals preserve source DB |
| Serialization | Unicode, escaping, key order, duplicate alias columns, bytes, null, booleans, large IDs, floats, NaN rejection, paths and revision hashes | Exact golden bytes wherever identity or frozen output depends on encoding |
| SQL | Read-only connection; DML/DDL/ATTACH/unsafe PRAGMA/extension attempts; disallowed functions; recursive work; caps; duplicate column names; partial output | No unauthorized mutation/disclosure; correct timeout and incomplete-result contract |
| GraphQL | Variables/fragments/aliases; visibility; null/error envelopes; bounded queries and connections; malformed documents; pagination context | Existing schema and execution semantics; caller isolation |
| Scanning | Empty vs unreachable; readable batches during incomplete scan; completed-scope absence; unvisited retention; continuation; replaced roots; generation changes; temporary storage failures | `test_guarded_scans.py` behavior retained, not approximated |
| Paths/trust | Symlinks, ancestor replacement, case aliases, Unicode, hardlinks, remount, wrong volume, strict/path trust, root overlap, permissions | No unintended source writes or output-root escape |
| Curation/rules | Ambiguous associations; active/inactive relationships; pinned revisions; incomplete selection; immutable definitions; stale preview; generation coalescing | Same accepted decisions and blockers; no hidden rule recursion |
| Publication | Existing unknown files; externally changed owned target; safe create/replace/remove; retained outputs; journal recovery | No overwrite/delete outside ownership; every crash state either recovers safely or blocks explicitly |
| Processing | Success, nonzero exit, timeout, cancel, lost worker, lease takeover, stale receipt, output corruption, duplicate completion, prerequisite changes | Valid bytes and current evidence required; stale effects cannot publish |
| Components/renditions/fallback | Multi-stream sources; package selection; all documented presets; multiple demand owners; chosen-source pinning; missing candidates; stale publication generations | Existing component/fallback/rendition suites mapped; real-media output inspected |
| HTTP security | No token, expiry, revoke, cross-profile/principal, disallowed exposure, shared work, tickets, cursor reuse, event replay and stale auth | No caller data leakage; effect dispatch/serve boundaries revalidate as required |
| Playback | Pending/first segment; playlist HEAD; segment HEAD; range rejection; unlisted/temp segment; session sharing/expiry/cancel; source changes | Preserve whole-segment 416 behavior and authenticated growing HLS semantics |
| Outbound HTTP | Approval without dispatch; approved origins; credential redaction; superseded unsent payload; disabled work; retry after lost ACK; bounded response; reauthorization | No silent duplicate-policy change; delivery completion not confused with remote job completion |
| Consumers/notifications | Plex matching/login/import, consumer refresh, Jellyfin scan/playback, optional Apprise partial success/retry | Fake-service tests plus existing real acceptance paths |
| Packaging | Clean wheel install, no source-tree imports, missing optional deps, min/current qualified Python, architecture/OS, package resources, native binary dependencies | Exact shipped artifacts work without a Rust compiler on wheel-supported targets |

### 9.3 Crash and fault-injection campaign

Add named failpoints around durable boundaries: before/after intent commit; before/after file creation; before/after fsync; before/after rename; before/after ownership update; before/after outbox insertion and acknowledgment; claim acquisition/replacement; migration steps; and recovery reconciliation.

For each failpoint, crash the actual subprocess where feasible and restart recovery against the resulting fixture. Test lost acknowledgments, partial writes, disk full, permission denial, unmount, worker termination and bounded shutdown. Failure injection should exercise real store and filesystem adapters, not only a pure model.

A process kill is not a complete simulation of power loss. Use real filesystem/remount acceptance where available and clearly separate evidence for process-crash recovery from evidence for storage durability under power interruption.

Release invariants include: no source media content modified; no unowned output removed; no invalid database identity adopted; no incomplete scope interpreted as complete; no stale claim commits a successful result; and no caller loses demand because another caller canceled.

### 9.4 Stateful search and replay campaign

Each workflow model must supply deterministic state/input/output encodings, named properties, explicit legal input schedules, an independent oracle where applicable, and fault controls. Save original and minimized traces with model/property/codec/build identities.

Fast CI runs small exhaustive spaces and fixed-seed regressions. Nightly CI runs a documented larger seed campaign and bounded compositions. Preserve failure traces as artifacts and permanent regression fixtures after diagnosis. Do not change bounds silently or equate a callback error with a successful model check.

Complement model exploration with real concurrent calls, clock/lease boundary tests, and persistence post-state checks. State exploration cannot prove that an OS lock, network implementation, or FFmpeg process behaves as modeled.

### 9.5 Performance qualification

Use existing synthetic and scale benchmark entry points as the initial reference. Retain 100/1,000/5,000 synthetic sizes, then use the existing high-cardinality profiles and recorded real filesystem workloads. Add larger sizes only with explicit fixture/resource definitions. [S6, S17]

Measure cold CLI start, idle RSS, scan throughput, peak RSS, temporary disk use, descriptors, bytes read, query p50/p95, plan/apply time, writer-lock hold time, job admission, HTTP latency, cancellation delay, and verification overhead. Separate warm/cold filesystem cache, storage medium, SQLite build, external tool versions, concurrency, and enabled observers.

Compare release builds on the same machine and dataset with repeated interleaved runs. Report distributions rather than a single best measurement. Proposed initial regression review threshold: a repeatable deterioration above 10% in a critical measured path triggers investigation; M0 must ratify or revise thresholds before treating them as release gates.

Success is demonstrated improvement or acceptable parity with justified tradeoffs. Do not trade away safety checks, resource bounds, coverage semantics, or media validation for a benchmark win.

## 10. CI and release evidence

Preserve the current Python unit, formatting, documentation/spec, installed-wheel, media-tool, storage, consumer, notification, Jellyfin, HTTP, and generated-client checks that apply to the baseline. Existing CI explicitly installs FFmpeg, Poppler and Tesseract in its unit lane. Missing tools must not quietly convert required acceptance to passing skips. [S6]

Add Rust formatting, linting, unit/integration tests, pure-reducer fixtures, differential suites, migration rehearsal, model exploration/replay, and native artifact checks. Keep source CI and installed-artifact CI separate.

Example **proposed** harness commands after the corresponding tools exist:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
python scripts/parity.py --reference reference/python --candidate target/release/catabolic --suite all
cargo run -p catabolic-verification -- explore --suite release --report artifacts/models.json
cargo run -p catabolic-verification -- replay tests/parity/traces/stale-completion.sttrace
python scripts/qualify_wheels.py --dist dist --matrix tests/parity/platforms.json
```

These names are planned deliverables, not commands currently present in the repository. Keep existing applicable commands working, including `catabolic spec check`, frozen-release checks, installed CLI acceptance, and TypeScript client acceptance.

Each candidate release retains: commit/toolchain/dependency identities; checksums of the Python oracle and candidate artifacts; parity ledger; per-platform reports; schema/contract checks; model bounds and outcomes; crash/recovery evidence; performance deltas; known limitations; and rollback instructions. Publish the exact qualified artifacts rather than rebuilding different binaries after testing.

## 11. Rollout and rollback

### 11.1 Stage order

**Read-only comparison:** Start Rust against snapshots and isolated fixtures. No effects, migrations, or hidden writes.

**Explicit operation ownership:** Add developer/experimental selection before command admission. A chosen engine owns the entire operation, including its durable state and recovery behavior. Avoid per-helper fallback.

**Disposable mutation qualification:** Exercise full write workflows on cloned catalogs with isolated output roots and controlled integrations. Confirm both logical data and filesystem effects.

**Opt-in real catalogs:** Require the documented backup/recovery procedure and record the supported rollback envelope. Do not shadow mutations against real consumer APIs or notification receivers.

**Rust default:** Only after every required ledger row has passing evidence. Keep a clearly identified Python reference release for the compatibility window; retire the duplicate implementation after rollback requirements and operational experience permit it.

### 11.2 Rollback constraints

Do not automatically retry a failed Rust mutation using Python. The first attempt might already have changed files or sent an external request.

Before changing engine ownership, stop admission, settle or explicitly recover in-flight work, inspect journals and claims, and apply the tested handover protocol. Schema 32 compatibility is necessary but not sufficient: old Python must understand every state and journal record Rust wrote.

Restoring a database backup is **not** a general rollback for filesystem or network side effects. A snapshot can lose later metadata, conflict with current outputs, or requeue an external effect. Prefer forward recovery, then a qualified engine handover. When restoration is required, follow a tested incident procedure that reconciles database identity, outputs, claims, and external consequences; never run two catalog copies against the same managed outputs.

Test both directions explicitly: Python-created unfinished operations recovered by Rust, and Rust-created unfinished operations recovered by the pinned Python version within the advertised rollback envelope. Unsupported states require an explicit blocker, not best-effort mutation.

## 12. Risks and decision checkpoints

| Risk | Consequence | Required mitigation/checkpoint |
|---|---|---|
| Incomplete feature inventory | A scanner rewrite is presented as a complete product rewrite | M0 ledger covers commands, API operations, extras and late schema features |
| Stale design documentation | Missing schema changes and incorrect scan semantics | Pin source/tests/contracts; maintain discrepancy register |
| Canonicalization mismatch | Existing revision identities and backup evidence become invalid | Golden byte encodings before data mutation |
| Migration hooks omitted | Apparently valid upgraded DB loses relationships or fails checks | Port UDFs, FK handling and backfill; independent historical fixtures |
| Filesystem race/recovery regression | Source/output loss or unsafe ownership actions | Descriptor-relative implementation, real fault injection, M6 gate |
| Unqualified Rust dependency semantics | Regex, validation, GraphQL or error behavior changes | Compatibility spikes and black-box fixtures in M1–M3 |
| Statelessness API churn or over-modeling | Rewrite becomes a framework redesign or state explosion | Pin dependency, isolate adapter, small bounded models, no global catalog state |
| Disconnected model and production | Model passes while SQL/runtime violates policy | Shared reducers plus real post-commit comparison and boundary tests |
| Apprise or other optional adapter gap | Native feature set silently loses integrations | Explicit bridge/native decision and full-parity release blocker |
| Wheel portability regression | Previously installable environments require a compiler or fail | Supported-matrix decision, clean-platform wheel checks and documented sdist path |
| Dual engine ownership | Duplicate side effects or incompatible recovery | One operation owner, no automatic mutation fallback, tested handover |
| Benchmark-driven scope reduction | Safety or completeness removed for speed | Correctness gates precede performance goals |

**Checkpoint after M1:** Proceed only if distribution and semantic dependency spikes are credible and the first real lifecycle fault can be found/replayed.

**Checkpoint after M2:** Do not permit real Rust mutations until migration and serialization parity are demonstrated.

**Checkpoint after M6:** Do not allow managed-output writes outside disposable fixtures until filesystem recovery qualification passes.

**Checkpoint after M10:** Do not call the rewrite feature-complete with unresolved HTTP, optional integration, Python API, or platform gaps.

## 13. Definition of done

The migration is complete when every baseline feature has a qualified owner and executable evidence; schema/history and existing catalogs remain usable; CLI, HTTP and required Python interfaces retain their approved contracts; filesystem safety and source immutability hold under the tested failures; all required external tools/integrations behave equivalently; and the native/Python artifacts work on their promised platforms.

Statelessness adoption is complete when critical production decision functions are exercised through its adapters, independent oracles and injected faults demonstrate meaningful detection, bounded search outcomes are reported honestly, and fresh-process replay reproduces retained regressions. It is not complete merely because the dependency appears in `Cargo.toml`.

Release requires a rehearsed upgrade/recovery/handover procedure, no unresolved safety discrepancy, approved performance results, intact packaged resources, and publication of the exact tested artifacts. The application core should no longer depend on the legacy Python business logic; any intentionally retained optional Python adapter must be named and supported explicitly.

**Recommended first implementation slice:** M0 → M1 → M2, then one end-to-end read-only catalog operation and one shared-request/cancellation decision model. This tests the three largest uncertainties—compatibility, packaging, and verification value—before broad mutation work begins.

## 14. Source register

Repository sources are pinned to the reviewed commits. External tooling documentation was consulted on October 9, 2026; exact dependency versions still require qualification during implementation.

- **S1:** [Catabolic design overview](https://github.com/Jagalite/catabolic/blob/453fca983222c6665a48775eef67c489c96b527e/docs/DESIGN.md)
- **S2:** [Catabolic migration implementation](https://github.com/Jagalite/catabolic/blob/453fca983222c6665a48775eef67c489c96b527e/src/catabolic/migration.py)
- **S3:** [Catabolic store](https://github.com/Jagalite/catabolic/blob/453fca983222c6665a48775eef67c489c96b527e/src/catabolic/store.py) and [Python package metadata](https://github.com/Jagalite/catabolic/blob/453fca983222c6665a48775eef67c489c96b527e/pyproject.toml)
- **S4:** [HTTP contract implementation](https://github.com/Jagalite/catabolic/blob/453fca983222c6665a48775eef67c489c96b527e/src/catabolic/http/contract.py)
- **S5:** [Guarded-scan tests](https://github.com/Jagalite/catabolic/blob/453fca983222c6665a48775eef67c489c96b527e/tests/test_guarded_scans.py) and [scan staging](https://github.com/Jagalite/catabolic/blob/453fca983222c6665a48775eef67c489c96b527e/src/catabolic/scan_staging.py)
- **S6:** [CI workflow](https://github.com/Jagalite/catabolic/blob/453fca983222c6665a48775eef67c489c96b527e/.github/workflows/ci.yml)
- **S7:** [Pinned source tree](https://github.com/Jagalite/catabolic/tree/453fca983222c6665a48775eef67c489c96b527e/src/catabolic) and [tests](https://github.com/Jagalite/catabolic/tree/453fca983222c6665a48775eef67c489c96b527e/tests)
- **S8:** [Statelessness README: model interface, oracles and runtime observation](https://github.com/Jagalite/statelessness/blob/cb7f256e4d7cf20cfab6018b9bbba391ec2dd9de/README.md)
- **S9:** [Statelessness README: model responsibilities, search limits, trace/replay and runtime capture](https://github.com/Jagalite/statelessness/blob/cb7f256e4d7cf20cfab6018b9bbba391ec2dd9de/README.md#model-responsibilities)
- **S10:** [Statelessness Cargo metadata](https://github.com/Jagalite/statelessness/blob/cb7f256e4d7cf20cfab6018b9bbba391ec2dd9de/Cargo.toml)
- **S11:** [Migration and recovery documentation](https://github.com/Jagalite/catabolic/blob/453fca983222c6665a48775eef67c489c96b527e/docs/MIGRATIONS.md); implementation S2 takes precedence when version descriptions differ
- **S12:** [rusqlite connection API](https://docs.rs/rusqlite/latest/rusqlite/struct.Connection.html)
- **S13:** [Maturin official repository and mixed-package guidance](https://github.com/PyO3/maturin)
- **S14:** [PyO3 official repository](https://github.com/PyO3/pyo3)
- **S15:** [PyO3 parallelism guidance](https://pyo3.rs/main/parallelism)
- **S16:** [PyO3 building and distribution](https://pyo3.rs/v0.29.3/building-and-distribution)
- **S17:** [Catabolic development/testing guide](https://github.com/Jagalite/catabolic/blob/453fca983222c6665a48775eef67c489c96b527e/docs/DEVELOPMENT.md)
