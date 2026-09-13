# One-shot source observations

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

A scan executes a source observation once. An observation-only watcher schedules
that same operation. A query/fallback watcher obtains compatible observations,
evaluates its existing plan, and performs its configured reaction.

## Commands

Select a database and profile explicitly, or set `CATABOLIC_DB` and
`CATABOLIC_PROFILE` before running these examples:

```sh
catabolic --json scan media
catabolic --json scan media --exclude downloading
catabolic --json scan --request-id REQUEST_ID
catabolic watcher run inventory
catabolic supervise --once
```

`scan` requests fresh evidence, with a barrier captured once for the invocation.
It needs no saved query, watcher, report or projection. `--request-id` resumes one
source demand with its recorded guarantees; do not combine it with a source or
exclusions. A later plain `scan` is a new demand. Scan JSON retains `complete` and
`scans` and adds `observations` containing request/job references. Pending work is
incomplete, with a null scan ID and an explicit blocker, not an offline source.

`watcher run NAME` executes that watcher's configured plan. `supervise --once`
processes eligible watcher work; it does not scan every source once. A continuous
supervisor runs in the foreground. Nothing here automatically enables a watcher.

Save this as `inventory.json` to schedule inventory without querying or reacting:

```json
{
  "plan": {"kind": "observation", "sources": ["media"], "exclusions": ["downloading"]},
  "schedule": {"kind": "interval", "seconds": 3600},
  "max_age_seconds": 300
}
```

```sh
catabolic watcher put inventory --file inventory.json
catabolic watcher preview inventory
catabolic watcher enable inventory
catabolic supervise
```

An observation plan has explicit bound source IDs and optional source-relative
exclusions. These control traversal coverage, not media selection. Its reaction
is null; supplying a report, event, processing or projection reaction is rejected.
An incomplete observation leaves the watcher outstanding for retry. For query,
fallback and projection plans, existing conservative coverage of all bound
profile sources remains, including sources with no previous query matches.

After either manual or scheduled observation, use the same [inbox](INBOX.md)
flow: summary → list → show → act through the owning service → check again.
Scanning itself never accepts proposals, invokes an agent, renders media or
publishes projections. Existing catalog-change notifications can become pending.

## Shared service and guarantees

All inventory entry points use `catabolic.observations.observe(app, source, ...)`.
`request` admits durable demand; `resume` reloads a request; `execute` consumes
admitted work; `cancel` cancels only one request. `Application.scan()` is the
legacy-shaped one-shot facade. Its private `_execute_observation(job)` performs
the single physical traversal and staged publication, without calling admission.
The isolated worker invokes that executor directly.

- `max_age` permits completed evidence whose traversal started within the captured
  freshness window. Completion time alone is insufficient.
- `after` requires traversal to start at or after that barrier. Plain scan sets a
  new barrier and disallows older completed reuse.
- `request_id`, or a stable `requester` admission key within profile/source,
  resumes the same demand. Retries preserve the original minimum start time,
  exclusions and completeness policy. Changed scope/policy arguments require a
  new demand; changing freshness timestamps cannot silently move its barrier.
- Compatible queued work is joined. Compatible running work is joined only when
  its start satisfies the demand. An older running scan leaves a queued follow-up;
  there is never a second concurrent traversal of the same source/profile.
- `wait=True` waits outside the writer lock, bounded by `timeout` (30 seconds by
  default). Direct in-process traversal retains its existing unbounded wall time;
  staging has existing size/entry limits. Isolated execution bounds the caller's
  wait, not the shared child's lifetime. A live child retains its capacity slot.
- `require_complete` participates in compatibility. Strict consumers do not reuse
  confirmed-unavailable evidence. Fallback consumers may use available alternatives
  under their existing plan policy; pending work never counts as availability.

Compatibility includes profile/source, full binding and volume identity, effective
trust policy revision/settings, normalized exclusions, exact full-source coverage,
method, completeness policy, dirty generation and minimum traversal start time.
Retries may reuse a successful compatible observation satisfying the original
guarantees; unavailable evidence is excluded from retry reuse. A changed
binding/policy or new dirty event requires new compatible work while
preserving the demand's original timing guarantee. Completed scans predating the
new compatibility convention are retained in history but not reused after upgrade.

Result references include `request_id`, job `id`, immutable `guarantees`, `reuse`
(created, inflight, completed or resumed), source/exclusions, start/completion
times, covered dirty generation and the scan report. Job states distinguish
complete, unavailable, failed, stale and queued/running work; cancellation is a
request outcome. Failed or incomplete traversals retain prior inventory. Missing
marking is restricted to completely traversed coverage. An available root with
incomplete traversal is not a complete inventory.

Claims serialize source traversal and cap live scans at four. New claims hold an
advisory lock beside the database for the executor's lifetime; isolated children
inherit it. Recovery can therefore retire an abandoned claim even if its process
is still alive after writer reconnection fails. Lock files are coordination
metadata and must not be removed while work is active. Confirmed dead
workers become stale immediately, without waiting for their lease. Resuming the
request replaces stale work. Claims and binding/trust identity are validated
before staged publication. Events during traversal remain above its captured
dirty generation. Writer reconnection and worker startup use existing bounded
contention retries; source-event hints remain queued until their transaction
persists. Cancelling a requester does not kill a shared worker or another demand.

## Caller audit and preserved policies

The consolidation started from one physical scanner but two admission paths:
direct scan claimed new work itself, while named watchers used observation reuse
and then called back into scan. The following callers now share admission and
execution without merging their policies:

| Caller | Prior policy, preserved unless noted | Processing/completion/retry |
| --- | --- | --- |
| `Application.scan()` / CLI `scan` | All configured locations or one explicit location; normalized exclusions; fresh observation after invocation | No processing. Incomplete scans retain inventory. New: joins compatible admitted work and reports/resumes pending demand instead of raising capacity as an availability failure. |
| `watching.cycle()` / legacy `watch` | Calls fresh scan each polling cycle, then tracks unchanged file revisions across the settling interval | Still admits analysis for stable revisions from completed covered scans and runs the configured bounded processing batch, including existing retry options. No named watcher is required. |
| `maintenance.run()` / maintenance CLI | Validates requested source/output roots first; scans configured sources with exclusions | Strict incomplete-scan gate, settling, optional analysis/rules, removal budgets and output-ownership checks are unchanged. Ordinary maintenance does not automatically process media. |
| `Watchers.run()` query/fallback/projection plans | Freshness allowance or strict observe-after; all bound profile sources regardless of previous matches | Existing plan evaluation/reaction. Processing reactions admit jobs; workers execute them separately. New: immutable per-generation observation requests fix moving retry barriers; contention/timeouts remain pending. |
| Observation-only named watcher | New typed source-observation plan using the same service | No query, semantic baseline, processing or reaction. Existing schedules, run histories, claims and retries own orchestration. |
| Supervisor / native source events | Schedule eligible watchers; conservatively invalidate full-source evidence | No direct traversal. Dirty events and watcher hints persist atomically; continuous startup repairs missed-event gaps. |

Shared inventory does not prove downloading has finished: legacy settling and
processing admission revalidation remain necessary. Reusing an observation does
not create a scan record or advance an unrelated watcher's schedule/reaction.

## Storage and limits

Schema 26 adds `observation_requests`, referencing existing observation jobs, and
a `guarded` marker on jobs to distinguish these claims from legacy PID-only claims. It
stores requester identity, immutable guarantees and cancellation; scan history,
watcher runs and projection generations remain separate. Existing schema rows,
IDs and migration files are unchanged. Use the normal `db upgrade` backup and
recovery workflow. Schema 25 publication recovery remains supported before upgrade.
SQLite database backup/export includes requests; portable media manifests retain
their existing scope and do not transport live worker demand.

Coverage remains exact full-source with exclusions. Targeted scans, broader to
narrower reuse, automatic retention and remote-worker claims are not implemented.
An uninterruptible live process can retain capacity; cancelling one demand cannot
safely terminate work shared by others. Observation-only watchers use the existing
local-owner authority model. HTTP contract 1.4.0 exposes the additive plan through existing operator watcher
endpoints with the same grants and admission behavior; published 1.0–1.3 artifacts
remain unchanged. No new transport or scheduler is required.
