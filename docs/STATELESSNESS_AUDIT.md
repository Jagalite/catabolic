# Statelessness coverage and architecture audit

Date: 2026-10-29  
Audited revision: `6969f08c5d9b207afe8eec819555759be136644d` (`origin/main` at the time of the audit)

## Scope and evidence

Catabolic uses Statelessness for bounded verification of selected application-owned reducers. This does not make the application stateless: SQLite remains its durable authority, and filesystem and network effects remain adapter responsibilities.

This document records the read-only architectural audit of the revision above. Findings concern model coverage and decision ownership; they are not demonstrated production defects. No builds, model executions, tests, or runtime qualification were performed for this audit. Source references below are relative to this document and describe the audited revision.

Production Rust source matched `codex/ci-follow-up` at audit time; that branch differed in CI and test plumbing. The earlier notification implementation was preserved separately in a stash and is not part of the audited revision or this document.

## Existing coverage and gaps

- Two Statelessness models exist: shared requests and playback. The shared-request production caller is the simple CLI stdio path in [CLI main](../crates/catabolic-cli/src/main.rs). It does not cover durable shared jobs, caller demand, or reservation accounting.
- The [request oracle](../crates/catabolic-verification/src/main.rs) checks emitted effects for stale completion, cancelled delivery, and cancellation stopping a sibling. It does not require necessary start or delivery effects. An implementation that omits required effects can therefore evade these checks. The [playback adapter](../crates/catabolic-verification/src/playback.rs) similarly needs obligations requiring valid starts and completions to progress.
- Existing core reducers for execution, coordination, rendition, and resolution need verification adapters and composition checks. Their existence does not justify relocating them again.
- The [native qualification script](../scripts/check-native-mutations.sh) invokes `catabolic-verification check` and `check-playback`. The inspected gates did not contain automated fault-discovery/replay invocation. Available commands alone are not evidence that a gate executes them.

## Prioritized decision ownership

### 1. Publication and crash recovery — highest safety priority

[Filesystem mutation and journal recovery](../crates/catabolic-store/src/filesystem_mutation.rs) decide cancellation, resumption, retention, and acknowledgment.

Model journal stage, intent identity, observed output, and staged or retained objects. Require interrupted operations to preserve foreign output and retained references, and prohibit acknowledgment before publication is verified. Exercise crashes between effect application and durable acknowledgment, including repeated recovery.

### 2. Scan completeness and missing-file publication — highest safety priority

[Observation handling](../crates/catabolic-store/src/observation.rs) decides when absence is established.

Model scope completeness, seen evidence, root and policy identity, dirty generation, and continuation. Incomplete or stale coverage must never authorize missing-file records. Verify that interrupted or superseded scans cannot publish absence from obsolete evidence.

### 3. Shared-job demand and reservation accounting

[HTTP access SQL triggers](../crates/catabolic-store/resources/migrations/022_http_access.sql) propagate job outcomes and release reservations outside the existing models.

Model one physical job, multiple callers, and one reservation. Preserve trigger atomicity and compare committed database state with model transitions. Check sibling cancellation, duplicate completion, final-demand removal, and exactly-once reservation release.

### 4. Processing acceptance and retry

[Media mutation completion handling](../crates/catabolic-store/src/media_mutation.rs) evaluates source changes, checksum baselines, fact invalidation, and retry policy around the execution reducer.

Make acceptance, invalidation, publication, and retry decisions explicit effects. Verify that stale completion cannot publish facts for a newer source and that retries retain the correct source and attempt identity.

### 5. Playback admission, failure, and cache retirement

The worker model assumes supplied authorization and capacity evidence. Production separately [classifies playback failures](../crates/catabolic-store/src/playback.rs) and [retires caches](../crates/catabolic-store/src/playback_cache.rs).

Model session demand, expiry, reservation ownership, and cache deletion progress. Keep worker and cache lifecycles distinct. Check that active demand prevents premature retirement and that completion or failure releases only the owned reservation.

### 6. Watcher, source-event, and refresh coordination

[Watcher scheduling](../crates/catabolic-store/src/watcher.rs) owns debounce, deadlines, pending reactions, and retry decisions.

Model pending, requested, and completed generations plus reaction identity. Late completion must preserve newer work, and retries must preserve event identity. Verify deadline and debounce behavior using explicit time inputs.

### 7. Notification delivery and mapped-request supersession

[Notification policy](../crates/catabolic-store/src/notifications.rs) owns leases, attempts, exhaustion, and backoff. [Mapped HTTP requests](../crates/catabolic-store/src/http_mappings.rs) add replacement and approval rules.

Use composable machines for pending, dispatched, acknowledged, superseded, and revoked work. Check stale lease completions, bounded attempts, backoff eligibility, and replacement fencing. The previously stashed implementation is not verification evidence for main.

### 8. Fallback health and publication progress

Core already decides failback from supplied values. [Health accumulation and publication acknowledgment](../crates/catabolic-store/src/fallback_resolution.rs) remain outside that decision.

Model candidate and revision identity, accepted health checks, and admitted versus verified generations. Reject stale health evidence and prevent admission from being treated as verified publication.

### 9. Content-ticket allowance and transfer completion

[Ticket and transfer handling](../crates/catabolic-store/src/content_access.rs) combines allowance, expiry, revocation, revision binding, and verification.

Model these decisions explicitly. Allowance must not be overspent, revoked or expired authority must not admit new work, and failed verification must not produce successful completion. Compare model outcomes with committed database effects under competing requests.

## Pure policies suitable for core

These decisions can be pure functions without introducing additional state machines:

- [Rule eligibility and budgets](../crates/catabolic-runtime/src/rules.rs).
- [Estimates](../crates/catabolic-store/src/estimates.rs).
- [Media suitability](../crates/catabolic-runtime/src/rendering.rs).
- [Plan comparison](../crates/catabolic-store/src/plans.rs).
- [HTTP content selection](../crates/catabolic-http/src/routes.rs).

## Implementation and verification boundary

Machines should own domain decisions. Adapters should collect evidence and apply effects with atomic revalidation. SQLite should remain the durable authority; filesystem and network tests remain necessary.

Begin by strengthening existing request and playback oracles with required-effect obligations. Add adapters for existing core reducers before expanding ownership. Prioritize publication recovery and scan completeness, then proceed through the remaining candidates according to dependency and risk.

For each adopted machine:

1. Define state, input evidence, effect identity, and independent safety and progress obligations.
2. Enumerate bounded transitions, documenting bounds and environmental assumptions.
3. Inject a known fault, require discovery, and replay its recorded counterexample in an automated gate.
4. Compare production adapter outcomes and committed database state against the model, including duplicate, stale, interrupted, and competing operations.
5. Retain integration tests for real side effects and crash recovery.

Report source review, compilation, executed tests, model checks, fault discovery/replay, and live qualification separately. A bounded model pass does not establish unrestricted liveness or whole-system correctness.
