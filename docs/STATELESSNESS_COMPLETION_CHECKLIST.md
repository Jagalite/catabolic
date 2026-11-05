# Statelessness completion checklist

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

This checklist turns the bounded implementation requirements in
[the historical audit](STATELESSNESS_AUDIT.md) into explicit acceptance items.
It does not expand those requirements into a claim of whole-system correctness.
Existing execution receipts remain in
[implementation status](STATELESSNESS_IMPLEMENTATION_STATUS.md).

## Common acceptance contract

For each adopted machine, completion requires all five audit layers:

1. Production decision ownership and explicit state/effect identity.
2. A finite model with independent safety **and required-progress** obligations,
   documented input bounds and environmental assumptions.
3. Successful exhaustive exploration, deliberate fault discovery, and exact
   fresh-process replay on matching source/build identities.
4. A production-adapter comparison with independently expected durable state,
   including duplicate, stale, interrupted and competing operations where relevant.
5. Real filesystem/network/crash integration tests for effects abstracted by the
   model, executed against the final source.

A source implementation or a passing gate-wiring test does not satisfy layers
3–5. Rollback-isolated SQLite exploration is not independently committed/reopened
exploration. A failpoint returning an error is not a process-kill test.

## Per-area acceptance

| Area | Production ownership already present | Bounded completion obligation |
| --- | --- | --- |
| Request/playback obligations and existing reducers | Core request, playback, execution, coordination, rendition and resolution reducers; all 24 existing model command triplets are wired into qualification. | Preserve required-effect omission controls, run every check/find/replay, and ground composed effects in the corresponding production tests below. |
| Publication/recovery | Core acknowledgement and cancellation eligibility; filesystem adapters retain journal, output, staging and predecessor identity checks. | One create/replace intent through retain/stage/publish/acknowledge, interruption, obsolete intent, cancellation and foreign references; independent progress and early-ack/retention fault. Ground stage traces in committed journal/ownership/retention/generation comparisons, existing process-exit crash matrix and repeated interrupted recovery. |
| Scan completeness | Core ordered publication fences and continuation admission; transaction revalidation and real missing-row SQL. | Two scopes and parent/child lineage, selected/deferred work, seen evidence, cancellation and dirty readmission; stale/copied-evidence fault. Compare committed missing rows, parent lineage and request binding across fresh-process interruption/readmission. |
| Shared jobs | Core admission/cancellation/retry and atomic production fanout/reservation triggers. | Two callers and one physical job through success, failure, retry, duplicate/stale completion and final-demand removal. Compare genuinely committed/reopened production state, exactly-once reservation release and terminal events; retain independent negative control. |
| Processing acceptance | Core source, checksum-baseline and retry decisions around the execution reducer; transaction-fenced publication. | Immutable source identity, two attempts, due boundaries and stale completion, including changed source after retry. Compare committed attempts, queue, pinned snapshot and facts; retain baseline invalidation and source-race tests. |
| Playback admission/retirement | Core worker, admission/capacity and retirement decisions; durable job/session/cache adapters. | Two sessions, expiry/cancellation, worker success/failure, owned reservation release and interrupted deletion. Ground abstract deletion/SQL in persisted lifecycle and cache tests; identify any failure-classification decision remaining adapter-owned. |
| Watcher/refresh | Core due/debounce, completion generation and retry-chain classification; transactional completion owner checks. | Lease replacement/expiry, newer event and retry identity composition; refreshed-event maximum delay independent of quiet debounce. Compare original reaction deduplication and pending generations after interruption/retry. |
| Notifications/mapped requests | Core acknowledgement/backoff and lease fencing; atomic claim/acknowledgement and mapped-request replacement SQL. | Five attempts, retry due/reclaim, expiry, destination revision/revocation and replacement; stale acknowledgement fault. Compare production committed replacement/batch refusal so neither partial cancellation nor partial delivery admission can commit. |
| Fallback/publication progress | Core candidate selection/failback and publication verification; production health and generation adapters. | Two candidates/revisions, health count/age, failed/stale/duplicate evidence and restart; admission must not imply verification. Compare current generation/binding/revision under stale health and verification; retain production projection ledger. |
| Tickets/transfer completion | Core debit and full-response digest/final-chunk decisions; atomic SQL debit and authorization/revision adapter checks. | Two clients sharing allowance, debit rollback/interruption, authority/revision changes and digest failure. Compare committed balance and actual final-chunk release; preserve live HTTP competing-request and blocked-transfer tests. |
| Pure policies | Rule eligibility/budgets, estimates, media suitability, semantic plan comparison and HTTP ordered selection in core. | Preserve production call sites and existing parity/boundary tests; execute final-source checks, including media tools and HTTP selection. No extra state machine is required merely for a pure policy. |

## Resumed-work evidence boundary

The resumed checkout starts from `756d4edde35562bea32360b79dfbc39e6e9b3700`.
Source additions and reviews must not overwrite the historical execution receipts.
The first recovery snapshot was explicitly **uncompiled and unqualified**.
Exact Rust and reference-Python prerequisites have since been provisioned and
focused execution has started; see the dated implementation-status section for
actual results. Source review or environment preparation is not a completion
receipt, and final aggregate qualification remains separate.

The available Python environment executed the eight synthetic SQLite
trigger/retirement checks, 32 qualification-shell/source-manifest checks and two
model-gate wiring checks. Those wiring checks use a recording stand-in, including
72 deliberate command failures; they do not execute Rust models. Bundled-guide
synchronization passed. The exact pinned runtime and locked dependencies were subsequently provisioned;
reference discovery and generated-table checks passed. This does not erase the
historical failed attempts or establish aggregate qualification.

Full qualification requires the repository's pinned Rust toolchain, exact Linux
Python/Expat oracle, locked dependencies and frozen reference. Do not downgrade
pins, substitute generated fixtures for runtime requirements, or treat focused
checks as the full qualification script. Commit/push and remote CI evidence must
identify the final source; merging and deployment are separate authorization.

## Local closure status after resumed implementation

The finite source/model obligations above are implemented and reviewed. All 24
checks and matching-build fault replays passed. Representative durable adapter
and real-effect coverage passed for publication/hardlink recovery, multi-generation
scan continuation, shared-job cancellation/retry, processing acceptance,
playback/retirement, watcher replacement/retry, mapped notifications, fallback
health identity, and content-ticket/transfer completion. These are the bounded
comparisons required by the audit, not exhaustive production refinement proofs.

**Overall qualification is not complete.** The unchanged mandatory aggregate
cannot pass in this workspace without genuine UUID-bearing storage and the
recorded historical capture/runtime/reference-path prerequisites. SQLite function
availability also differs between the source-built oracle and native engine.
The implementation-status record distinguishes the failed broad run, repaired
local prerequisites, scoped passes and remaining limitations. No ignored or
unavailable lane is relabeled as passed, and no remote publication is implied.
