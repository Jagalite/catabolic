# Entry status and worklog

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Each catalog item has an overall curation status and an append-only worklog.
Use the worklog for notes, decisions, progress, and reasons for deferring work.
It applies to the whole item, across its associated files and renditions, and
supports every built-in and custom media kind. These CLI commands require schema 10.

## Status and completion checks

| Status | Meaning |
| --- | --- |
| `pending` | Not yet reviewed; default for existing and new items |
| `in_progress` | Curation is underway |
| `complete` | Explicitly marked complete and all required checks currently pass |
| `deferred` | Work is postponed; record why in the worklog |
| `ignored` | Intentionally outside the current curation work |
| `needs_attention` | Previously marked complete, but a required check no longer passes |

`needs_attention` is computed, not manually assigned. The saved `requested_status`
retains the last explicit status decision. If the evidence becomes valid again,
the effective status returns to `complete` without inventing another completion
event. Reads never append history. A successful status request, even if it repeats
the existing status, records a new explicit decision in the worklog.

Before accepting `complete`, Catabolic checks:

- The item has a nonempty title.
- All active `primary` file associations have recorded `present` availability
  and a source binding in the selected profile.
- All explicitly required files, jobs, artifacts, proposals, and review tasks pass.

An item without files can be complete: books, collections, and other abstract
items need not represent a physical file. Optional files and jobs do not block
completion. A pending optional proposal does not block it either. Add explicit
requirements for additional metadata, edition identification, or other review work.
There is no automatic parent/child completeness roll-up; use a review task or the
expected-set workflow when a whole collection needs an explicit completeness check.

```sh
catabolic --json item status ITEM_ID
catabolic --json item status ITEM_ID --set in_progress --note 'Reviewing editions'
catabolic --json item requirements ITEM_ID
catabolic --json item status ITEM_ID --set complete --note 'Curation finished'
catabolic --json item list --curation-status needs_attention
```

A blocked completion returns an error naming the blockers. It changes neither
the saved status nor the worklog. `item show` and `item list` include a `workflow`
summary with effective status, requested status, revision, timestamp, and author.

Status decisions and worklog entries are shared across profiles. Effective readiness
is profile-dependent because active primary file availability is profile-dependent.
Explicit requirements retain the profile that supplies their evidence. SQL summaries
include one row per item/profile; always filter the intended profile.

## Write and read the journal

```sh
catabolic --json item note ITEM_ID --text 'Waiting for evidence of the edition.'
catabolic --json item note ITEM_ID --kind decision --text 'Retain both cuts.' \
  --actor agent:cataloger
catabolic --json item note ITEM_ID --kind progress --file notes.txt
catabolic --json item note ITEM_ID --file - < notes.txt
catabolic --json item worklog ITEM_ID --limit 50
catabolic --json item worklog ITEM_ID --limit 50 --after EVENT_ID
```

Notes support multiline UTF-8 text, up to 64 KiB per entry. User-authored kinds are
`note`, `decision`, and `progress`. Status and requirement changes append structured
events automatically. Each event has an ID, entry revision, body, actor, profile,
timestamp, and structured change data. `actor` is caller-supplied attribution,
not an authenticated identity; its default is `user`.

No CLI operation edits or deletes a worklog event. Add a correction referencing the
earlier event when necessary. Entry metadata updates, rescans, and processing runs
do not replace notes. Processing attempt details remain in `process attempts`;
the worklog records workflow actions and references required job IDs rather than
copying every processing event.

Workflow mutations increment the item's shared workflow revision and commit their event atomically.
Agents and interfaces can supply `--expected-revision N` to reject an edit based
on an outdated entry. After a conflict, reread `item status` and the worklog before
retrying. A repeated note without a revision precondition creates another event.
This revision covers status, requirements, and worklog changes; it is not a version
of all item metadata or processing evidence. Completion always reevaluates the
current recorded checks within its transaction.

## Required work

```sh
# Manual checks can cover metadata, edition review, or an explicitly verified publication.
catabolic --json item require ITEM_ID --kind review --label 'Confirm edition'
catabolic --json item resolve REQUIREMENT_ID --state complete \
  --note 'Checked the publisher record' --actor reviewer

# Automatic checks use actual catalog evidence, not a manually assigned success flag.
catabolic --json item require ITEM_ID --kind file --target FILE_ID --label 'Source present'
catabolic --json item require ITEM_ID --kind job --target JOB_ID --label 'Required remux'
catabolic --json item require ITEM_ID --kind artifact --target ARTIFACT_ID --label 'Output ready'
catabolic --json item require ITEM_ID --kind proposal --target PROPOSAL_ID --label 'Accept identification'

# Waive or reopen a requirement with an explanation; its earlier history stays intact.
catabolic --json item resolve REQUIREMENT_ID --state waived --note 'This output is optional now'
catabolic --json item resolve REQUIREMENT_ID --state pending --note 'Reopened after new evidence'
```

| Kind | Passing evidence |
| --- | --- |
| `review` | Explicitly resolved as complete with a note |
| `file` | Recorded present and bound in the requirement's evidence profile |
| `job` | Complete, current recorded input and successful analysis facts; render jobs also require a ready, current output |
| `artifact` | Ready, matching recorded file identity and publication snapshot, or current checksum verification against the artifact's original SHA-256 |
| `proposal` | Accepted; rejection does not satisfy a requirement to accept identification |

Automatic requirements cannot be manually marked complete. They may be waived
with a reason. A required job references an exact job ID; successful retries of
that job satisfy it despite earlier failed attempts. A different replacement job
must be explicitly required and the superseded requirement waived. Targets must
belong to the item, and jobs/artifacts/proposals must belong to the selected profile
when the requirement is added. Each item supports up to 1,000 explicit requirements.

These checks use recorded evidence and never secretly rescan, hash, decode, or
contact a server. Use `scan`, `process enqueue verify`, `process run`, and normal
link `verify` commands when fresh physical evidence is needed. A manual publication
review records the reviewer's assertion; there is no automatic remote-app verification
gate. No link output or source file is changed by status/worklog operations.

Generated files now retain their publication snapshot. A metadata probe of a
changed output does not establish that it still matches the original bytes.
Older artifacts with no publication snapshot need a successful current checksum
verification before satisfying a newly added artifact/render-job requirement.

## SQL, GraphQL, and export

```sh
catabolic query "SELECT item_id,status,requested_status,revision FROM catalog_item_workflow WHERE profile=:profile AND status='needs_attention'"
catabolic query "SELECT check_id,label,state,target_id,evidence_profile FROM catalog_workflow_checks WHERE profile=:profile AND item_id=:item AND coalesce(satisfied,0)=0" --params '{"item":"ITEM_ID"}'
catabolic query "SELECT id,revision,kind,body,actor,data,created_at FROM catalog_item_worklog WHERE item_id=:item ORDER BY id" --params '{"item":"ITEM_ID"}'
catabolic graphql '{ items(curationStatus:DEFERRED,first:20) { nodes { id title workflow { status requestedStatus revision } } pageInfo { endCursor hasNextPage } } }'
catabolic graphql 'query($id:ID!){item(id:$id){workflowChecks{nodes} worklog(first:20){nodes pageInfo{endCursor hasNextPage}}}}' --variables '{"id":"ITEM_ID"}'
```

SQL also exposes the stored definitions through `catalog_item_requirements`.
GraphQL has root `worklog(item:ID)` and `workflowChecks(item:ID)` fields as well as
the nested item fields. GraphQL uses opaque cursors; CLI worklogs use numeric event
IDs. Keep following the returned cursor when exporting a full journal.

Open Catalog v3 manifests include each selected item's current `workflow` summary
as an additional field, using the format's existing extension support. The released
schemas remain unchanged. Manifests do not contain the full worklog or requirement
definitions; use paginated CLI/SQL exports for inspection and database backups to
preserve all history and requirements.

## Upgrade safety

Run `db upgrade --dry-run`, then `db upgrade` on the intended database. Schema 10
adds workflow tables and a nullable artifact publication snapshot without rewriting
existing items, recipes, or artifact rows. Existing items read as `pending` with
revision 0 and an empty worklog; the migration does not invent reviews or history.
