# Processing architecture review

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Implementation follow-up: schema 12 now provides catalog-specific rendition
publication, semantic rule requirements, local receipt imports, optional recipe
acceptance checks and historical usage reports. See
[rendition workflows](RENDITION_WORKFLOWS.md) for current commands and limits.
Schema 13 adds the HTTP receipt-v1 adapter, worker leases, measured estimate
calibration and larger paged rule selections; see [processors](PROCESSORS.md).
The review below preserves the schema-11 baseline as historical research.

Reviewed 2026-09-07 against Catabolic commit `9aa327b` (schema 11), with pending
README documentation edits. This is a source-grounded design proposal, not a
claim that external processors are integrated. Read offline with
`catabolic docs processing-review`. Current commands remain documented in
[processing rules](RULES.md), [artifacts](ARTIFACTS.md), and
[maintenance](MAINTENANCE.md).

## Recommendation

Extend the existing catalog, job, rendition and reconciliation boundaries.
Prioritize publishing selected renditions to individual catalogs and tracking
their completion correctly. Then add a small external-result exchange contract.
Keep advanced encoding and distributed execution available through specialist
tools before considering a new native execution engine.

The useful end-to-end workflow is:

```text
Inventory and current facts → saved selection → immutable recipe → job/attempt
                                                                ↓
External result receipt → validate and register → rendition with source lineage
                                                                ↓
                                explicit catalog publication policy
                                                                ↓
                           layout → mappings → sync → verify → manifest
```

The external receipt and catalog publication policy are proposed extensions.
The other major stages already exist. Processing completion, publication
readiness and entry curation completion must remain separate, inspectable states.

## What the other tools demonstrate

| Project and reviewed scope | Concrete evidence | Application to Catabolic |
| --- | --- | --- |
| Tdarr: public TypeScript plugins and official node/API documentation | Separate size-ratio and duration-ratio checks; a plugin configures live size comparison; FFmpeg execution and file copying are separate flow steps | Extend recipe validation and execution reports; keep physical output publication separate from link layouts |
| Unmanic: Python task, file-test, scanner and postprocessor source, selected integration tests, runner documentation | Selection can return a reason and priority; runners share probe context; task results distinguish processing success from file movement success | Reuse recorded facts and explain deferrals; distinguish encoded, registered and published outcomes |
| FileFlows: official library/builder documentation and public community scripts | Separate destination and source-retention settings; size heuristics, VMAF and job-concurrency examples | Make destination policy explicit; treat estimates and acceptance checks separately; use transactional scheduling if concurrency is added |
| FileBot: public Groovy scripts and CLI documentation | Naming formats and link actions, input exclusions, duplicate ranking and media-server refresh | Preserve identity/selection/naming boundaries and trigger consumer refresh only after successful publication |

This review did not inspect Tdarr's server/node core, FileFlows' core engine, or
FileBot's application core. Public plugins/scripts are not evidence for every
behavior of their host applications. No third-party programs were installed or
executed, so these are source/documentation findings, not performance or crash
qualification results. References below pin the source revisions reviewed.

### Tdarr: checks and execution are composable

The [size-ratio plugin][tdarr-size] routes results into within-range, too-small
and too-large outcomes. [Duration comparison][tdarr-duration] is another explicit
check. The [live-size plugin][tdarr-live] configures either current-size or
estimated-final-size comparison, with a delay before checking. That file sets
flow variables; it does not itself implement the worker's monitoring loop.

Catabolic already checks output bytes, selected codecs/streams and duration in
`rendering.render`; adding another generic validation engine would duplicate it.
The useful extension is a versioned recipe acceptance policy: for example,
required dimensions or an optional maximum output/input size ratio. A smaller
file is not automatically a valid or better rendition. Missing duration must be
explicitly unknown when a policy requires duration evidence.

The [copy plugin][tdarr-copy] handles destination paths independently of
[FFmpeg execution][tdarr-execute]. Catabolic should retain that separation through
`Artifacts` and `Layouts`, with its existing owned-directory reconciler handling
links. Copying a working file into a directory does not itself provide Catabolic's
desired-state, collision and recovery semantics.

Tdarr's [node documentation][tdarr-nodes] distinguishes shared-path workers from
workers that transfer files. It also describes limitations for additional outputs
such as extracted subtitles on unmapped nodes. If Catabolic adds remote execution,
the protocol needs an explicit output list and per-output transfer evidence;
one returned path is insufficient for multi-file output recipes.

### Unmanic: preserve the distinction between work and delivery

The [file-test implementation][unmanic-filetest] records exclusion/failure reasons
and passes shared context between decision plugins. Its
[runner contract][unmanic-runners] supports shared probe information and separates
file testing, worker execution, file movement and final results. This supports
Catabolic's existing choice to scan cheaply and obtain probe facts separately.

The [postprocessor][unmanic-post] stages files using a partial suffix and records
both processing and movement outcomes. In the inspected local copy method, an
existing destination is removed before the final move. This is a concrete reason
to preserve Catabolic's no-overwrite and durable-publication design when adapting
results, rather than importing a file-movement implementation wholesale. This is
a comparison of one method, not a complete recovery audit of Unmanic.

The [task model][unmanic-task] distinguishes task states and task-scoped context.
Catabolic already has durable jobs and attempts; reuse them for execution history.
The reviewed [task-handler tests][unmanic-tests] exercise scheduled and watch-event
queues and bounded thread shutdown. Add equivalent operator-visible interruption
and repeated-maintenance cases for any new integration.

### FileFlows: useful policies, but community scripts are not core guarantees

[Library settings][fileflows-library] explicitly separate a new destination from
whether originals are retained. Its [FFmpeg Builder][fileflows-builder] provides
configurable processing elements. Catabolic's generated locations and immutable
recipes already provide the corresponding ownership boundaries.

The [size heuristic script][fileflows-size] compares file size with a
duration/resolution-based allowance. It is a selection heuristic, not a guarantee
of future encode size. The [VMAF example][fileflows-vmaf] performs a separate
comparison of working and original files. Keep such expensive quality analysis
optional, budgeted, and represented as evidence rather than enabling it on scans.

The [community job-manager script][fileflows-jobs] polls running jobs and adds a
random busy wait before choosing who proceeds. This does not demonstrate an
atomic resource reservation. Catabolic should use durable claims if it later
supports concurrent renders. It is not evidence that FileFlows' core scheduler
has the same limitation.

### FileBot: operational history is not content identity

The [AMC script][filebot-amc] loads a path exclusion list and appends selected
inputs before rename work. That is useful incremental automation, but Catabolic
should continue keying processing reuse to input revisions, recipe and execution
identity, with explicit retry state. A path being seen before is not proof that
the current contents were successfully processed.

FileBot's [duplicate script][filebot-duplicates] and [CLI][filebot-cli] are useful
references for alternate-copy selection and naming actions. Catabolic already
has catalog-specific copy policies, layouts and symlink/hardlink reconciliation.
Extend the candidates those policies can select; do not add a second naming or
link-writing system to the processing runner.

## Where this fits in the current code

| Responsibility | Current owner and storage | Preserve or extend |
| --- | --- | --- |
| Discover files | `app.py`, `filesystem.py`; files, scans, observations and profile bindings | Preserve complete-scan publication and read-only source access |
| Obtain facts | `processing.py`, `process_runner.py`; jobs, attempts, facts and baselines | Reuse current evidence; keep expensive decode/quality checks explicit |
| Select backfill inputs | `selection.py`, `rules.py`; processing_rules and rule_jobs | Preserve bounded queries, immutable rule revisions, generated-input exclusion |
| Define intended output | `rendering.py`, `outputs.py`; recipes and output_definitions | Add typed acceptance/execution options only for demonstrated needs |
| Execute and publish physical files | `artifacts.py`; processing_jobs, processing_attempts and processing_artifacts | Preserve revalidation, owned partial files, checksums and recovery |
| Describe a rendition | `outputs.py`; media_outputs and item_files | Extend external provenance and evidence without fabricating a local render job |
| Choose and name output files | `copy_selection.py`, `selection.py`, `layouts.py`; catalog policies and mappings | Add explicit rendition publication eligibility scoped to a catalog |
| Change output links | `reconcile.py`; owned links and operation journals | Keep one path for preview, collisions, removal budgets, apply and recovery |
| Track entry readiness | `item_workflow.py`; item_requirements and item_worklog | Add semantic rule/rendition requirements without erasing old attempts |
| Run upkeep | `maintenance.py` calling the existing services | Report each stage and backlog; keep rendering opt-in |

## Priority 1: publish renditions to a specific catalog

Give output definitions a versioned semantic purpose, such as transcode, remux,
preview or thumbnail, while retaining file role as a separate concept. Expose it
with producer, recipe/definition revision and eligibility evidence in SQL and
GraphQL. Existing built-in recipes can supply that classification; externally
registered outputs need an explicit declaration. Do not classify ordinary H.264
files as transcodes simply because of their codec, or guess unknown historical
custom outputs. This makes an “all transcoded videos” selection independent of
specific preset names and which processor created the rendition.

Currently `Outputs.record` creates new `item_files` rows with `active=0`.
`selected_associations` and default layout planning require `active=1`.
Activating a generated primary association therefore makes it eligible for other
unfiltered layouts as well. A query can select just transcodes, but global
activation is an awkward prerequisite for publication to only one catalog.

Introduce an explicit publication policy per catalog, defaulting to today's
active-association behavior. A policy may admit specified ready renditions from
a rule revision or output definition into that catalog's candidate set. It must
not modify the global association active flag or silently admit arbitrary
disabled associations. Record catalog-specific exclusions so a rejected rendition
does not return on the next refresh. Define that distinction in the policy API;
`active=0` alone cannot express both “awaiting first review” and a durable rejection.

Feed those candidates through the existing copy selection, layout and reconciler.
Never mark an external registration ready for automatic publication merely because
it exists. Require current output evidence appropriate to the saved policy.

Support two deliberate modes: one preferred rendition per item/part, or all
matching renditions with collision-safe naming. Specify fallback to originals
explicitly. Recipes producing a new logical item use that item's metadata and
relationship, not an implicit copy of the source identity. Old rule revisions and
outputs remain available; following a newly enabled revision must be a documented
policy choice, not an accidental name lookup.

Acceptance: a mobile library gains a completed 720p rendition, the existing Plex
library keeps its original selection, and repeat maintenance changes no healthy
links. Include explicit rejection, two recipe revisions, missing/stale outputs,
sidecars, tied candidates, empty selections and removal-budget blockers.

## Priority 2: completion requirements that survive retries

Today `Rules._attach` adds required job gates only when apply queues or adopts a
job. Deferred/unapplied matches create no gate. Older failed job requirements are
preserved after retries; users must explicitly waive obsolete ones. These are
documented behaviors, but they are cumbersome for “this entry needs a current
mobile rendition.”

Add a semantic requirement for a particular rule revision and input/item pair,
resolved by a current accepted rendition. Retain individual attempts as history;
do not require a previously failed attempt to become successful. Record explicit
supersession instead of deleting or silently waiving old requirements.

Evaluation for item lists, SQL and GraphQL must use recorded evidence, through
the existing `item_workflow.py` evaluator. Read queries must not run arbitrary
saved selections or probe disks to determine completion. Persist a bounded rule
evaluation generation during apply/maintenance; incomplete evaluations cannot
prove that there is no outstanding requirement. Preview remains read-only.

Acceptance: required deferred work blocks completion after evaluation, retries
can satisfy the semantic requirement, invalidated output returns to attention,
and a failed/partial evaluation never clears a prior requirement.

## Priority 3: accept results from external processors

Start with a versioned JSON receipt imported through the CLI. A Tdarr, Unmanic or
FileFlows workflow can write the receipt only after its final outputs have been
delivered. Catabolic then scans the external destination and validates the receipt
using `Outputs.register`. Today `output register` already records user-declared
lineage, but it is not a verified external job protocol.

The receipt should carry an external system/instance identity, stable job and
attempt IDs, Catabolic source file/item IDs and input revision, output-definition
revision, configuration digest, declared tool versions, completion outcome,
output location/relative paths, sizes and optional checksums. Preserve external
claims separately from Catabolic's probe/hash/validation evidence. Define size
and count limits. Never infer lineage solely from matching filenames.

An import needs an idempotency key scoped to the producer instance. Repeating an
identical receipt is a no-op; changing the payload under that key is a conflict.
Use existing explicit binding/rebinding checks for remote path translation.
Reject stale input revisions, partial deliveries, symlinks and paths outside the
bound external destination. A missing source cannot silently pass lineage
validation; an offline-source evidence workflow would need a separate policy.

Treat external output directories as scanned, read-only locations. Do not give
third-party workers write access to Catabolic-owned generated locations or link
catalogs. If managed copying is later needed, route it through owned publication
and account for simultaneous staging and destination bytes.

Add polling/submission adapters only after one real receipt workflow passes.
Tdarr's [API guide][tdarr-api] points to the installed server's API documentation;
do not promise an adapter based on guessed endpoints. A future adapter must
record submission intent before the network call, reconcile uncertain submissions
before retrying, and never rerender because notification delivery failed.

## Priority 4: storage, validation and scale

- Preserve full-selection estimates alongside batch estimates. Add actual output
  bytes and elapsed times by recipe/tool revision, then optionally calibrate
  estimates with sample counts and uncertainty. Size ratios are not quality
  scores. Expose unknowns rather than assuming zero storage or execution time.
- Keep three separate values: expected additional final storage, temporary peak
  storage by filesystem, and runtime output limits/free-space reserves. Current
  local rendering renames a partial file in the destination filesystem; external
  transfer can need an additional full copy. Symlink publication does not add a
  second media copy.
- Extend recipe validation for specific output requirements and optional decode
  evidence. Preserve rejected attempts and report why they failed. Do not make a
  post-encode size heuristic retroactively redefine the preview estimate.
- Share the input/recipe/destination identity calculation between rule matching
  and artifact enqueue. Their checks currently differ: enqueue includes tool
  identity, while rule satisfaction primarily matches source revision, recipe
  and destination. Define explicitly whether a tool upgrade invalidates a ready
  rendition; never rebuild the entire collection solely through an undocumented
  change in cache semantics.
- Add stable pagination for backfill evaluation before claiming million-file
  rules. Current rules cap expanded inputs at 10,000, materialize matches and make
  per-input evidence/history checks. Store evaluation checkpoints and prove full
  totals before applying a page; truncating a selection cannot mean completion.
- Keep serial rendering initially. `Artifacts.run` renders one job at a time and
  maintenance holds the database writer lock for its whole cycle, even though
  encodes run outside SQLite transactions. Short transactions do not make other
  writers concurrent. Measure writer wait time and query latency before changing
  this boundary. Parallel execution needs leases, fencing, per-device admission
  budgets and recovery; a worker-count flag alone is insufficient.

## Database and delivery boundaries

Use additive numbered migrations only when implementing a slice. Proposed
catalog publication decisions, semantic rule evaluations and external receipt
identities need durable storage with foreign keys and appropriate unique keys.
Do not overload free-form tags or worklog text as operational state. The precise
DDL should follow the accepted first workflow; this review creates no migration.

Keep `media_outputs` as the common rendition/lineage record. Local
`processing_artifacts` remain evidence for Catabolic-owned physical publication;
an external registration must not invent that evidence. Local jobs/attempts keep
their current state model until an external execution adapter actually needs to
extend it. In schema 11, one artifact per job attempt is enforced, so multi-output
jobs or HLS/DASH packages require a deliberate extension rather than a disguised
single-file path.

Do not edit frozen recipe definitions, migrations or Open Catalog artifacts.
New machine-local operational tables do not automatically require an interchange
version bump; changes to exported rendition/manifest contracts do. Preserve the
existing migration backup/rehearsal workflow and historical fixtures.

Deliver in this order: catalog-specific rendition publication; semantic completion
requirements; a receipt import with one real processor; additional acceptance
checks and measured reporting. Defer a visual flow editor, arbitrary executable
plugins, automatic source replacement/deletion, general workflow graphs and
distributed encoding until there is a demonstrated need.

Each slice must include CLI help/JSON discovery, offline/wiki documentation, and
real temporary-media acceptance. Test source hashes unchanged, interrupted output
publication, duplicate/conflicting receipts, stale sources, changed bindings,
disk exhaustion, cross-filesystem transfers where supported, Unicode paths,
explicit rejections, cancellation and repeat maintenance. Existing hardlink
final-reference protections continue to apply. No performance gain or complete
application parity is claimed by this review.

## Source snapshots

Source was read for design comparison, not copied into Catabolic or executed.
Documentation links are mutable.

| Repository | Reviewed revision | Scope |
| --- | --- | --- |
| Unmanic/unmanic | `1c324b8fc3974ffce3d7cc945adb938fe7182910` | Selected Python core files and task-handler tests |
| HaveAGitGat/Tdarr_Plugins | `26c97a52f9dcf5fc6faeb751071cb82cdf97ca4e` | Selected public TypeScript flow plugins |
| fileflows/community-repository | `300ee1322934c609a3f226d6778f6175c4ec00a5` | Selected community scripts, not engine source |
| filebot/scripts | `1cd22478a4f2c8f3bb113fcb36309511490397cc` | Public AMC and duplicate scripts, not application core |

[tdarr-size]: https://github.com/HaveAGitGat/Tdarr_Plugins/blob/26c97a52f9dcf5fc6faeb751071cb82cdf97ca4e/FlowPluginsTs/CommunityFlowPlugins/file/compareFileSizeRatio/2.0.0/index.ts
[tdarr-duration]: https://github.com/HaveAGitGat/Tdarr_Plugins/blob/26c97a52f9dcf5fc6faeb751071cb82cdf97ca4e/FlowPluginsTs/CommunityFlowPlugins/file/compareFileDurationRatio/1.0.0/index.ts
[tdarr-live]: https://github.com/HaveAGitGat/Tdarr_Plugins/blob/26c97a52f9dcf5fc6faeb751071cb82cdf97ca4e/FlowPluginsTs/CommunityFlowPlugins/file/compareFileSizeRatioLive/1.0.0/index.ts
[tdarr-copy]: https://github.com/HaveAGitGat/Tdarr_Plugins/blob/26c97a52f9dcf5fc6faeb751071cb82cdf97ca4e/FlowPluginsTs/CommunityFlowPlugins/file/copyToDirectory/1.0.0/index.ts
[tdarr-execute]: https://github.com/HaveAGitGat/Tdarr_Plugins/blob/26c97a52f9dcf5fc6faeb751071cb82cdf97ca4e/FlowPluginsTs/CommunityFlowPlugins/ffmpegCommand/ffmpegCommandExecute/1.0.0/index.ts
[tdarr-nodes]: https://docs.tdarr.io/docs/nodes/nodes/
[tdarr-api]: https://docs.tdarr.io/docs/api/
[unmanic-filetest]: https://github.com/Unmanic/unmanic/blob/1c324b8fc3974ffce3d7cc945adb938fe7182910/unmanic/libs/filetest.py
[unmanic-post]: https://github.com/Unmanic/unmanic/blob/1c324b8fc3974ffce3d7cc945adb938fe7182910/unmanic/libs/postprocessor.py
[unmanic-task]: https://github.com/Unmanic/unmanic/blob/1c324b8fc3974ffce3d7cc945adb938fe7182910/unmanic/libs/task.py
[unmanic-tests]: https://github.com/Unmanic/unmanic/blob/1c324b8fc3974ffce3d7cc945adb938fe7182910/tests/integration/test_taskhandler.py
[unmanic-runners]: https://docs.unmanic.app/docs/development/writing_plugins/plugin_runner_types/
[fileflows-library]: https://fileflows.com/docs/webconsole/libraries/library
[fileflows-builder]: https://fileflows.com/docs/plugins/video-nodes/ffmpeg-builder/
[fileflows-size]: https://github.com/fileflows/community-repository/blob/300ee1322934c609a3f226d6778f6175c4ec00a5/Scripts/Flow/Video/Video%20-%20Filesize%20bigger%20than%20calculated.js
[fileflows-vmaf]: https://github.com/fileflows/community-repository/blob/300ee1322934c609a3f226d6778f6175c4ec00a5/Scripts/Flow/Video/Video%20-%20VMAF.js
[fileflows-jobs]: https://github.com/fileflows/community-repository/blob/300ee1322934c609a3f226d6778f6175c4ec00a5/Scripts/Flow/Utilities/Flow%20-%20Job%20Manager.js
[filebot-amc]: https://github.com/filebot/scripts/blob/1cd22478a4f2c8f3bb113fcb36309511490397cc/amc.groovy
[filebot-duplicates]: https://github.com/filebot/scripts/blob/1cd22478a4f2c8f3bb113fcb36309511490397cc/duplicates.groovy
[filebot-cli]: https://www.filebot.net/cli.html
