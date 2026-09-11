# Named query watchers

A watcher maintains a query-defined result or projection on its own schedule,
using sufficiently fresh shared observations. It does not introduce a selector,
fallback engine, projection writer, or implicit processing rule.

## Configure and run

Watchers are disabled when created. Commands use the selected database/profile:

```sh
catabolic watcher put audit --file audit.json
catabolic watcher preview audit
catabolic watcher enable audit
catabolic watcher run audit
catabolic watcher list
catabolic watcher pending
catabolic watcher history audit
catabolic supervise --once
catabolic supervise
```

`watcher schema` describes all configuration fields. A saved report example:

```json
{
  "version": 1,
  "authority": "local_owner",
  "plan": {"kind": "query", "query_id": "IMMUTABLE_QUERY_REVISION"},
  "schedule": {"kind": "interval", "seconds": 3600},
  "max_age_seconds": 300,
  "reaction": "report"
}
```

Queries retain their selection, rows, document, and composition contracts. Logical
fallback plans require `kind: fallback`, `membership_query_id`, `policy_id`, and a
role/part/variant slot. A policy alone never defines membership. Projection plans
use `kind: projection`, `catalog`, and `binding_digest`, obtained from the shared
`plans.projection_digest` service. The digest pins the authoritative binding and
layout; configuration changes require an explicit disabled-watcher revision.

Projection reactions use `reaction: projection`. One evaluated plan reaches the
existing epoch-fenced, locked and journaled publication path. Evaluation through publication shares its lock; source scanning stays outside it. Scheduled executions
also repair drift when the semantic result is unchanged. Unresolved entries retain
membership and owned links, with the reaction pending for retry. Exact HTTP content
references and already-admitted job inputs never substitute another file.

`reaction: event` emits a durable structured notification transition. Processing is
explicit: `reaction: processing`, immutable `operation_id`, `max_jobs`, and an
approved `destination` for render operations. Only complete file-ID selections or
resolved logical fallback inputs can drive admission. Existing analysis/render
services pin inputs, deduplicate jobs and own execution. Watchers enqueue work;
they do not execute encoders or activate persistent rules. External operations are
not admitted by this initial reaction adapter.

## Schedules and ownership

Schedules are `manual`, `interval`, or `cron`. Cron has five numeric fields:
minute, hour, day of month, month, weekday (Sunday=0). Fields support `*`, lists,
inclusive ranges and positive steps. Both restricted day fields must match.
Timezone is an explicit IANA name (default UTC). Repeated DST wall minutes run once;
nonexistent minutes are skipped. Searches are bounded to two years, so schedules
without an occurrence in that window are rejected. Missed runs coalesce into one
attempt, and interval deadlines resume from an admitted execution time.

`events: true` enables event-triggered runs. `debounce_seconds` and
`maximum_delay_seconds` coalesce hints; resource contention can delay admission.
Triggers during a run leave a newer pending generation. Explicit run admissions
persist independently of retry timing, including admissions during an active run.
Disabling pauses pending work and retries and releases automatic projection ownership;
it preserves the pending reaction for inspection. Re-enable or explicitly run/admit
work to resume it. Disabling an active run is rejected. Sharing a completed scan
never advances another watcher's cron/interval deadline. Manual-only watchers do
not run because another watcher observed their sources.

Only one automatic mutation owner is allowed per projection/profile. Migrating
existing enabled fallback/catalog-refresh automation requires
`watcher enable NAME --transfer`. This disables those legacy schedules and assigns
the owner atomically. Legacy workers skip owned projections even if their old
settings are subsequently enabled. Processing completion dirties event-enabled
owners; it cannot bypass their schedules. Disabling a watcher does not reactivate
legacy automation. Legacy `maintenance run` rejects watcher-owned output reconciliation; use `watcher run` or inventory-only maintenance. The old top-level `watch` command remains available.

## Shared evidence and limits

Coverage is conservatively **all bound sources in the profile**, including when a
query currently returns no IDs. Dependencies between query/policy definitions are
not interpreted as complete filesystem dependencies. Unbound sources cannot be
observed. Arbitrary SQL/GraphQL and time-dependent queries need periodic execution.

Initial sharing requires exact full-source scope, exclusions, binding/volume and
trust policy identity. Compatible completed and in-flight jobs are reused. Broader
to narrower coverage reuse and targeted scans are not implemented. Inventory,
availability, hashes, verification and fallback live probes remain distinct.
Freshness is measured from traversal start, not its completion. Set
`observe_after_request: true` for a new observation barrier;
`require_complete_inventory: true` blocks reports when any inventory is incomplete.
Otherwise unknown/unavailable sources are reported honestly and fallback can use
available alternatives while retaining historical inventory.

Scans stage on disk outside the writer lock. Short admission/publication sessions
capture and revalidate a source generation, binding, trust, coverage and lease.
Failed scans never mark the library missing. Events acknowledge only their captured
dirty generation. Scanner processes have bounded waits and at most four live claims;
a still-live uninterruptible process retains capacity. A supervisor runs at most
four watcher children and terminates attempts exceeding 120 seconds. Isolated scan
waits are 30 seconds. Direct CLI scans release the writer but have no automatic wall
timeout. Staging is capped at 1 GiB/ten million entries with a 2 MiB SQLite cache.

There are at most 1,000 definitions per profile, 100 admitted runs per supervisor
pass, 100 jobs per processing reaction and existing query/fallback/publication
budgets. A source cannot have two live claimed traversals in one profile. Failed
reactions stay pending; every retry is a new recorded evaluation attempt. Dead
local worker PIDs are recovered on supervisor startup. PID reuse conservatively
retains a lease until expiry. Long or uninterruptible operations can reduce capacity;
timeouts cannot promise termination of kernel filesystem I/O.

## Events, traceability and HTTP

Polling is always supported. Optional `catabolic[watch]` adds
`supervise --native`, using one shared Watchdog observer over source roots. The
adapter follows the [Watchdog observer API](https://python-watchdog.readthedocs.io/en/latest/quickstart.html).
Its event queue is bounded; hints dirty full-source coverage, never edit inventory.
Generated sources and managed outputs are excluded. Continuous/native startup invalidates evidence to
repair crash gaps; polling `--once` retains compatible fresh observations. Native events are acceleration, not durable delivery: periodic
schedules remain necessary for missed events, network mounts and time-relative queries.
Cloud feeds are not implemented.

Run history links definition, trigger, observations, semantic comparison, prepared
plan and reaction. Selections compare complete ID sets. Rows preserve column order,
duplicate columns, row order and duplicate multiplicity; keyed diffs are not yet
supported. GraphQL arrays retain ordering. Failed/truncated evaluation never replaces
the last complete baseline. Projection check generations remain compatible; no-op
checks no longer advance verified publication generations. Planned/retained/verified
history records include path, revision, policy and removal changes. Durable semantic
history is retained indefinitely; automatic destructive retention is not enabled.

HTTP 1.2.0 adds typed operator endpoints at `/v1/operator/watchers`: list, PUT/GET
name, POST name/enabled, POST name/runs, GET name/history. Run admission returns 202;
only the supervised worker executes it. These endpoints require `operator:write` and
honor read-only server mode. Execution explicitly uses local-owner authority;
scoped application-owned automation is not implemented. Query evaluation sessions
carry authorization for direct authorized selections, but general plan orchestration
fails closed for scoped callers. Watcher internals are excluded from HTTP SQL.

Program bundle version 3 adds `program export --watcher NAME`, with pinned query,
policy and operation dependencies. Import creates disabled local definitions and
never carries credentials, leases, winners or health. Projection references require
an explicit local catalog mapping and validate/pin that existing local binding.
Old version 1/2 bundles and published HTTP artifacts remain unchanged.
