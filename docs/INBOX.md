# Work inbox

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

The inbox answers what needs attention, why, and which existing operation owns
the next decision. It is a read model over existing records: no task table,
generic completion mutation, claim, or agent runner. Reads never scan, probe,
hash, contact servers, advance failback checks, or append worklogs.

```sh
catabolic --db /path/to/catalog.sqlite3 --profile default --json inbox list --limit 100
catabolic --db /path/to/catalog.sqlite3 --profile default --json inbox show WORK_KEY
catabolic --db /path/to/catalog.sqlite3 --profile default --json inbox summary
catabolic docs inbox
```

Follow `list --after CURSOR` until `next_cursor` is null. `complete` and
`evaluation_complete` describe evaluation of that page; `result_complete` is false
when another page remains. `--include-inactive` also shows waiting, deferred and
historical records. Timeout and query errors fail rather than returning an empty
inbox. SQL retains its `complete`/`truncated` reporting; GraphQL clients must check
both errors and `pageInfo.hasNextPage`.

An empty actionable inbox does not prove complete inventory or finished curation.
Work may be waiting, deferred or ignored, and physical health may be unknown.
`summary.curation_finished` is therefore unknown. Summary counts cover the whole
selected profile independently of pagination, under existing execution budgets.
A large summary may time out; it must not be treated as a zero count.

## Contract and scope

`catalog_work_inbox` is a connection-local SQL view with these columns:

| Fields | Meaning |
| --- | --- |
| `work_key`, `profile` | Together form a stable reference. Keys namespace and hex-encode owner IDs; scans and evaluations do not change them. Treat them as opaque. |
| `subject_kind`, `subject_id` | Item, file, proposal, job, projection catalog, or watcher. Artifact recovery uses its owning job as subject. |
| `source_kind`, `source_id` | Authoritative record or derived workflow check. |
| `category`, `label`, `reason` | Category, display text, and machine-readable reason. Labels are untrusted catalog text. |
| `source_status`, `actionability` | Original status and derived ready, waiting, blocked, needs_decision, deferred, or historical. |
| `required` | 1 for completion obligations, 0 for known optional processing/proposals, NULL when not applicable. |
| `countable` | Whether the entry contributes to task counts; summaries and linked owner details can have 0. |
| `related_work` | JSON work keys backed by actual record relationships. Follow target references in `evidence_profile` where different. |
| `evidence_profile`, `evidence_at`, `evidence_complete`, `current`, `evidence` | Recorded evidence, with NULL for unknown. Timestamps retain the owner's SQL-text or Unix-seconds format. Readiness is separate from technical currentness. |
| `preconditions` | Owner-specific workflow revisions, observed file identities, job snapshots/attempts, watcher definitions/generations, or publication generations. |
| `suggested_actions` | JSON operation descriptors with structured arguments and reload/note requirements. These are incomplete plans, never executable shell commands. |

Coverage includes unidentified occurrences without placeholder items, curation
summaries, explicit and automatic workflow checks, proposals, native and external
jobs, artifact recovery, watcher attempts/reactions, fallback projection entries,
link journals and the legacy catalog-refresh queue. Owners retain all decisions
and histories. SQL includes every profile: always filter `profile=:profile`.
Item status is shared, readiness is profile-dependent, and explicit requirements
retain their evidence profile.

Default results contain ready, blocked and needs_decision entries. Queued,
running, submitted and leased jobs are normally waiting. Completed/cancelled jobs
are historical. A native failure is historical after a current successful analysis or generated
representation with the same file, operation/options and recipe. A historical
success without current evidence does not hide a new failure.
An exact-job requirement still retains its original semantics; a different job
does not silently satisfy it. Semantic rendition requirements continue to use
their existing replacement-job evidence. Old failed artifact attempts become
historical after a newer ready artifact from the same job. Pending publication
states suggest `artifact recover`; terminal failures suggest an eligible
`process retry` or `artifact show` for inspection and fresh admission planning.
These descriptors do not authorize processing. Empty action arguments are JSON
objects. A failed pending watcher reaction remains blocked even while a later
attempt waits for observations; its original error remains available in evidence.

Deferred items and checks remain deferred; ignored ones are historical. Failed
required job/proposal details inherit deferral when all explicit owners are
deferred/ignored. Independent optional work retains its own lifecycle and never
becomes a completion blocker. `in_progress` is not an exclusive lease.

An item summary with outstanding checks has `countable=0`; its checks carry the
count. A job/proposal referenced by a non-waived requirement is a linked detail
row with `countable=0`. An unidentified file awaiting a proposal is a waiting
detail, not an additional identification task. Use `sum(countable)` for work
counts and `count(*)` for displayed rows. Separate explicit requirements remain
distinct owner records even when they reference the same resource. Shared file
references permit grouping real blockers; similar timestamps imply no causality.
An automatic primary-file gate covered by an explicit file requirement in the
same evidence profile is a detail row and does not add another work count.
Failed external jobs retire when explicit rule provenance leads to a passing
replacement requirement; similar requests without that relationship are not merged.

Curation readiness and projection health remain separate. An unavailable required
original can cause needs_attention while its published fallback remains usable.
Unmapped or intentionally excluded files are not projection errors. Unrecorded
physical drift is unknown; run existing preview/verification operations explicitly.

## Queries and watchers

Local GraphQL uses the same view and existing pagination/budgets:

```graphql
query Inbox($after: String) {
  workInbox(first: 100, after: $after) {
    nodes
    pageInfo { endCursor hasNextPage }
  }
}
```

Nodes use evidence JSON, like existing worklog/check pages. This is a local
operator schema extension. Published HTTP GraphQL schemas remain unchanged;
the aggregate contains private watcher/recovery data and is unavailable through
HTTP SQL or authorized GraphQL. Existing HTTP interfaces retain their meanings.

Save ordinary SQL in rows mode or GraphQL in document mode. For example, use
`query save identification --definition FILE` with:

```json
{
  "version": 1,
  "mode": "rows",
  "selection": {
    "language": "sql",
    "query": "SELECT work_key,subject_id,label,reason,preconditions FROM catalog_work_inbox WHERE profile=:profile AND category='identification' AND actionability='ready' ORDER BY work_key"
  }
}
```

Other useful bodies, each suitable for an existing saved rows query:

```sql
-- Review requirements: labels explain edition/component compatibility work.
SELECT work_key,label,subject_id,suggested_actions FROM catalog_work_inbox
WHERE profile=:profile AND category='requirement'
  AND json_extract(evidence,'$.kind')='review' AND actionability='needs_decision'
ORDER BY work_key;

-- Required processing intervention, including semantic rendition obligations.
SELECT work_key,label,related_work,evidence FROM catalog_work_inbox
WHERE profile=:profile AND required=1 AND actionability='blocked'
  AND (category IN ('processing','artifact_recovery') OR
       (category='requirement' AND json_extract(evidence,'$.kind') IN ('job','artifact','rule')))
ORDER BY work_key;

-- Unresolved proposal decisions, including optional work.
SELECT work_key,subject_id,preconditions FROM catalog_work_inbox
WHERE profile=:profile AND category='proposal' AND actionability='needs_decision'
ORDER BY work_key;

-- Blocked projection reactions.
SELECT work_key,subject_id,evidence,suggested_actions FROM catalog_work_inbox
WHERE profile=:profile AND category='projection_reaction' AND actionability='blocked'
ORDER BY work_key;
```

Use normal query joins for media, tags, components and compatibility. No inbox
media-filter language is added. Ordinary rows/documents cannot become typed
work-key selections or mutation commands.

Report/event watchers monitor these saved queries using existing plans. Their
conservative coverage includes all bound sources even when no rows previously
matched. Failed/truncated queries do not replace complete baselines. Failed
reactions remain visible until addressed. Polling and periodic reevaluation are
still needed after crashes and for work already outstanding; notifications alone
are not durable agent dispatch. Reading consumes no entry or pending work.
This feature enables no watcher or processing automatically.

## Acting and preserving history

`show` explains owner, evidence, actionability and suggested operations. Its
`evidence_token` can be passed to `show WORK_KEY --expected-evidence TOKEN` to
reject changed planning evidence. This is a read-side comparison, **not an atomic
mutation fence or claim**. It covers the exposed row, not every upstream fact,
lease or authority change. Before acting, reload the owner, check eligibility
and permissions, then use the existing transaction and preconditions:

- Identification/association: proposal put, inspect, accept/reject. Acceptance
  rechecks the proposal's inventory and curation snapshot.
- Manual review: item resolve with a note and expected workflow revision.
  Automatic checks cannot be manually completed; explicit waiver semantics
  remain available. Item completion reevaluates evidence in its transaction.
- Processing: inspect jobs/attempts and use existing retry/admission/recovery.
  Changed revisions need fresh admission. Existing execution claims and fenced
  publication remain authoritative.
- Deferral: item status and append-only worklog operations.
- Projection: existing planning, watcher ownership, reconciliation, journals and
  verification. Review a fresh plan before publication.

A workflow revision is not a revision of files, metadata or processing facts.
There is no generic inbox action adapter. Actor strings are attribution, not
authenticated authority. Claims/agent invocation remain separate future work.

No database migration or export format changes. Backups preserve owner records;
SQL/GraphQL export the derived view. `--include-inactive` exposes resolved explicit
work. Resolved unidentified-file, automatic-check and journal rows may disappear:
inspect associations, scans, worklogs and publication/watcher history instead.
There is no new historical snapshot store. Lease expiry is not independently
declared a failure by this read model: use existing worker recovery and inspect
waiting work after a crash.

`tests.test_work_inbox` demonstrates disposable new-file identification, proposal
acceptance, explicit edition/component review, review resolution and completion.
Invalidating a required original returns the same requirement and item references;
restored evidence clears attention without new worklog events. It also exercises
mixed pagination/counts, unknown evidence, profiles, stale preconditions, owner
concurrency, saved queries, report/event retries and a 12,000-file backlog.

Guarded scans contribute one source-level observation blocker backed by the latest
observation job. Confirmed discoveries may appear while that job remains
incomplete. Consult its directory coverage before inferring absence; see
[guarded observations](OBSERVATIONS.md).
