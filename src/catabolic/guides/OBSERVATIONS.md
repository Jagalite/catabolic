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
  default). Direct traversal checks guarded budgets between operations; a blocked kernel
  syscall is not forcibly interruptible. Isolated execution bounds the caller's
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
request outcome. Incomplete traversals retain unvisited inventory while publishing validated
discoveries. Missing marking is restricted to validated directory coverage. An available root with
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

Initial coverage remains full-source with exclusions; explicit continuation can
select outstanding directories. Broader-to-narrower reuse, automatic history
retention and remote-worker claims are not implemented.
An uninterruptible live process can retain capacity; cancelling one demand cannot
safely terminate work shared by others. Observation-only watchers use the existing
local-owner authority model. HTTP contract 1.4.0 exposes the additive plan through existing operator watcher
endpoints with the same grants and admission behavior; published 1.0–1.3 artifacts
remain unchanged. No new transport or scheduler is required.

## Guarded discovery and continuation (schema 27)

A default scan publishes confirmed file metadata in short batches while traversal
continues. `complete: false` can therefore accompany `published: 1200`. SQL,
GraphQL and the inbox can read those discoveries immediately. Query execution
completeness describes the recorded result, **not** physical source coverage.
An absent query row during unfinished coverage is not proof of physical absence.
Use `require_complete_inventory` for watcher actions that require complete
membership; maintenance and the legacy settling loop retain their strict scan
completion gates. Processing input revisions and settling checks are unchanged.

The one shared walker uses a disk-backed directory queue and iterative,
no-symlink directory reopening. It holds a bounded number of descriptors rather
than an open ancestor stack. Every committed batch validates the catalog, bound
root identity, policy and exact execution fence, and renews the worker lease.
Positive facts and their existing revision-dependent currentness, batch identity,
directory progress and catalog-change notifications commit atomically.
Filesystem dirty generations are separate from catalog-change notifications.
Batch notifications never invoke processing or publication themselves. Active
watchers observing that job are not dirtied by their own batches; other watchers
retain schedule/debounce controls and durable pending generations.

Coverage is per directory: pending, running, complete, excluded, deferred or
failed. A complete directory is an enumeration interval, not a completed subtree
or point-in-time snapshot. Before finalizing its coverage, the publisher reopens
and checks its identity and mutation timestamps. Only files covered by that
validated enumeration may become missing. A present child directory protects its
unfinished descendants; a child confirmed absent can support their absence.
Excluded historical records remain queryable with their original observations.

### Default allowances

| Control | Default | Extended / `--full` |
| --- | ---: | ---: |
| Entries per directory | 50,000 | 1,000,000 |
| Directory depth | 64 | 4,096 |
| Elapsed time per directory | 15 seconds | 300 seconds |
| Single metadata operation latency | 2 seconds | 10 seconds |
| Errors per directory | 16 | 64 |
| Total elapsed execution | 120 seconds | 1,800 seconds |
| Total entries | 250,000 | 5,000,000 |
| Queued directory records | 100,000 | 100,000 |
| Batch records / encoded bytes / elapsed time | 500 / 1 MiB / 1 second | unchanged |
| Temporary queue size / minimum free storage | 64 MiB / 64 MiB | unchanged |

These are conservative guardrails, not performance guarantees. The repeatable
local benchmark is `python scripts/benchmark_scans.py`; it measures deep trees,
wide directories, many small files and injected slow metadata separately from
publication. Its simulated latency is not a NAS measurement. Counts, elapsed
intervals and reasons are included in directory reports. A local limit defers
that scope while independent queued directories continue. A global time, entry,
queue or storage stop pauses the traversal. Low temporary storage also stops a
multi-source manual invocation before starting further sources.

A syscall already blocked in the kernel cannot be forcibly timed out safely by
this implementation. Limits are checked between operations; the isolated worker
still separates requester wait time from worker lifetime. The per-source guard
and four-worker global cap remain in force. No physical-disk grouping or I/O
priority controller is added. Directory handle use is fixed and bounded, rather
than a configurable number of open ancestors. Scan batch/scope history currently
has no automatic retention policy and grows with continued catalog observation.

### Local benchmark evidence

One macOS disposable-fixture run on 2026-09-13, with Python allocation tracing
and extended allowances, produced the following results. Other regression work
was running on the host; these are observations, not throughput promises.

| Fixture | Traversal + orchestration | Publication | Total |
| --- | ---: | ---: | ---: |
| 80 nested directories | 0.178 s | 6.515 s | 6.693 s |
| 2,000 files in one directory | 0.114 s | 0.762 s | 0.876 s |
| 2,000 files across 40 directories | 0.193 s | 4.063 s | 4.256 s |
| 2,000 files, injected 1 ms metadata delay | 2.640 s | 0.432 s | 3.072 s |

The largest encoded publication buffer was about 52 KB. Peak traced Python
allocation ranged from 0.47 to 8.02 MB, including runtime/import overhead.
Directory opening/completion checkpoints dominate deep-tree publication costs.
The defaults keep batches small and bound expensive directory work independently
by entries and time; these small local fixtures do not qualify a NAS or prove
that a directory below an entry threshold will meet its time allowance.

### Shared owner policy

Preview a JSON policy, then explicitly apply it:

```sh
catabolic location scan-policy media --file observation-policy.json
catabolic location scan-policy media --file observation-policy.json --apply
```

For example:

```json
{"exclusions":["backups","downloads/incomplete"],"budgets":{"directory_entries":25000}}
```

Manual scans, all named watcher plans, maintenance and legacy watch use this
same effective policy. Native event routing filters its excluded paths too.
Caller exclusions add to owner exclusions; an empty caller list cannot remove
them. Broadening requires an explicit policy change. Policy revisions and the
complete effective execution definition are pinned in admission compatibility.
An old request whose observation policy changed must be replaced with a new scan.
Policy preview and inbox reads never change configuration.

### Continue outstanding work

```sh
catabolic --json scan media
catabolic --json scan --request-id REQUEST_ID
catabolic --json scan --continue-request REQUEST_ID --scope expensive --extended
catabolic --json scan --continue-request REQUEST_ID --full
```

`--request-id` resumes the existing immutable demand after interruption. Committed
batches and completed directories survive. An interrupted directory is safely
re-enumerated; iterator positions and last filenames are never persisted.
`--continue-request` creates a linked amended request and retains completed
scope history. `--scope` selects an outstanding source-relative directory;
omitting it continues all outstanding scopes. `--budgets FILE` supplies validated
JSON execution overrides. Full coverage retains configured exclusions, identity
checks, batch limits, concurrency and finite execution budgets. It may still
finish incomplete; permissions and replaced roots require repair.

A deferred job remains blocked under the same policy and allowances rather than
repeating an expensive traversal on each scheduled retry. Continue explicitly,
change policy, or leave it deferred. A successful continuation resolves the old
blocker without relabeling the original incomplete interval as complete.
Completed scopes retain their earlier intervals; refreshing them after filesystem
changes requires a new source observation rather than continuation alone.
Continuation rejects newer recorded dirty events atomically with admission.
Recovery and continuation retain the original root identity, including under
path trust, and stop if the physical root was replaced. SQLite disk-full and
filesystem out-of-space failures stop broader source execution.
Reports provide structured action descriptors
for continuation, exclusion preview and repair; CLI execution never waits for a
prompt, including interactive terminals. Filesystem permission failures are not
fixed by extended budgets.

One source-level scan blocker appears in the existing inbox, backed by the latest
observation job. Directory detail is bounded to 100 entries in the scan report;
query `catalog_observation_progress` for active jobs,
`catalog_observation_scopes` for paginated detail and
`catalog_observation_batches` for committed history. The inbox's work key stays
stable across observation generations. Completed jobs supersede earlier scan
blockers. There is no inbox completion mutation or new task lifecycle.

The existing `scan_id` remains an observation interval reference. Its scan row
stays incomplete during publication; batches have independent sequence numbers.
Continuations retain parent job references, and their inherited directory
intervals may be older than the new job's start. Such jobs are not reused as new
strict-fresh whole-source evidence. Generated artifact sources still inventory
only their registered ready artifacts, never private staging files.
