# Programmable catalog redesign: implementation review

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Review baseline: `5d90764`, schema 14. This is an implementation review and an
incremental design record, not a claim that every proposed operation is available.

## Repository findings

The repository inventory covers 58 application modules, migrations 1–14, the
interchange packages, CLI scripts, tests, scale benchmarks and documentation.
Detailed changes follow the owning implementation paths below. The generated
review inventory includes definitions, imports and content hashes; it is retained
with local validation evidence. Existing tests remain part of the redesign's
acceptance, rather than being replaced by a new demonstration.

| Boundary | Implemented behavior and mismatch with the desired model |
| --- | --- |
| `domain`, `media_catalog`, `app`, `store`, migrations | Items, identities, occurrences, associations, relationships, observations and mappings are already distinct. SQLite is the durable authority. Keep these distinctions and IDs. |
| `scan_staging`, `source_access`, `filesystem` | Complete staged scans and bound root/source identity are explicit. Descriptor-relative no-follow operations protect source bytes and output ownership. Do not route around them. |
| `sql_query`, `query`, `graphql_query` | Bounded read-only SQL already exposes normalized views plus underlying tables. GraphQL has bounded connections and nested entities, but much processing evidence is opaque JSON and common negative-state filters are missing. |
| `selection` | One shared complete-ID evaluator already serves layouts and render rules. SQL and GraphQL selections are embedded in consumer definitions; there is no reusable named query identity or composition contract. Nonpaged SQL permits volatile expressions that paged selections reject. |
| `processing`, `process_runner` | Bounded analysis, current facts, hashing, verification, text extraction and retry attempts already exist. They are separate from artifact rendering for sound execution reasons, but cannot be selected as rule operations. |
| `rendering`, `artifacts` | Versioned recipes, source snapshots, output ownership, validation, registration and interrupted publication recovery are strong. Presets cover more than transcoding. The recipe registry currently assumes local FFmpeg rendering. |
| `rules`, `rule_estimates` | Rules are versioned and attach to retained jobs; estimates distinguish unknowns. Planning nevertheless requires generated storage, active primary associations and render recipes. All derived inputs are excluded, preventing explicit catalog-driven processing chains. |
| `outputs`, `rendition_publication` | Outputs become occurrences, inactive associations and lineage records; cycles are rejected at the common write boundary. Publication requires independently checked readiness. Plain registration is not validated execution. Queries need an easier recorded-currentness view without confusing it with live verification. |
| `processors`, `network_adapters`, `receipts` | External execution has its own fenced leases and receipt identity; completion is atomic and idempotent. Keep these typed jobs and receipts, and attach rules to them instead of creating a third executor. |
| `layouts`, `copy_selection`, mappings | Query folders already are projections in behavior. Layouts, embedded selections, catalog copy policies and rendition policies have separate commands. A coherent projection surface can compose these owners without replacing them. |
| `reconcile`, `hardlinks` | Journaled mutation, recovery, preflight, explicit ownership, removal budgets and independent verification already work. Projected membership must continue through this path. |
| `catalog_refresh`, `maintenance`, `watching` | Committed rendition events durably invalidate opted-in output catalogs. Refresh updates links only; maintenance explicitly sequences work. Preserve bounded batch drains and avoid recursive rule invocation. |
| `item_workflow` | Required work, successful jobs, ready artifacts and healthy projections are distinct. Keep semantic requirements and append-only history; do not equate an empty gap query with successful pending jobs. |
| `manifest`, `exports`, `importers`, `targets`, interchange | Catalog projections feed exports and frozen portable contracts. Internal query/rule definition changes need no manifest rewrite or source mutation. Consumer compatibility remains target-specific evidence. |
| tests and acceptance | Unit, actual FFmpeg, installed wheel, interruption, receipts/fencing, filesystem boundaries, consumer and scale lanes cover different properties. The experience benchmark is a scripted reference, not an agent-quality result. Retain all lanes and report unavailable external infrastructure explicitly. |

## Concrete design

Queries own immutable named definitions and a typed result contract above SQL or
GraphQL. Mutation consumers accept complete item/file/association ID selections;
arbitrary row/document queries remain useful for inspection. Composition uses
bounded set operations over pinned query revisions of the same entity/profile.
It never invokes a rule. Embedded selections remain compatible snapshots.

Rules bind a query revision to an immutable operation definition. Operation kinds
select existing analysis, artifact or external-receipt executors. A file analysis
can legitimately have no logical item or generated destination. Artifact-producing
operations continue to require an identified source and owned generated storage.
Derived input is an explicit policy, with readiness and lineage validation; no
completion callback invokes another rule. Explicit reevaluation connects stages.

A projection binds a query to a reusable layout and the existing catalog-specific
copy/rendition policies. Catalog IDs remain output identities. Layout planning,
publication checks, mapping ownership, sync, verify, removal budgets and durable
refresh remain the implementation. A projection configuration is not a new output
engine, and a query becoming empty never authorizes unreviewed removal.

Recorded-state views expose source revision, evidence currentness and result state
without opening media. Unknown evidence stays nullable. Live readiness is still
revalidated by the executor/publication owner immediately before mutation.

## Migration and compatibility plan

Add numbered migrations; preserve the released SQL and portable manifests. First
introduce named query revisions and projection bindings, adopting embedded legacy
selections without changing existing IDs or their effective selection. Next broaden
operation/rule bindings while retaining existing job/attempt/receipt tables and
legacy render-rule semantics. Any necessary table reconstruction must retain all
rows and foreign-key references, run under the migration runner's transaction,
and pass populated-upgrade, rollback and foreign-key checks before acceptance.

Keep existing SQL, GraphQL, artifact, processing, layout, sync and rule commands.
New coherent CLI surfaces adapt to those owners. Add an opt-in versioned machine
envelope rather than changing legacy JSON responses in place. Explicit command
failures, incomplete selections, pending publication and failed execution remain
separate evidence in the payload.

## Delivery gates

Implement and test query reuse/composition first, projections next, then operation
rules and derived-input chains. Prove a missing-rendition query stops matching
committed current results, generated results can be selected by a second query,
independent projections choose different copies, and repeats perform no work.
Exercise empty/truncated selections, source changes, unavailable outputs, retries,
recipe revisions, disabled rules, concurrent matching rules and schema-14 upgrade.
Retain existing fencing, duplicate-receipt, lineage-cycle, journal, real-media,
consumer, unmount and scale coverage. Documentation must distinguish implemented
operations, fixture validation, live consumer evidence and remaining work.

## Implemented result

Migrations 15 and 16 implement the plan above. `saved_queries` and `projections`
compose existing owners; `operations` reuses recipe identities. `rule_analysis`
and `rule_external` are planning adapters for existing jobs, not schedulers.
`catalog_state` supplies SQL and GraphQL recorded-rendition evidence, and
`revision_evidence` shares snapshot comparisons with item completion checks.
External required work tracks existing fenced processor jobs and validated receipts.
Live derived-rendition admission checks ancestors before execution and publication.

The first full regression run executed 460 tests; three old schema/view assertions
needed additive-schema-aware expectations. New fixture tests subsequently exposed
and fixed the receipt/local-snapshot difference in recorded source currentness.
Final validation and infrastructure-specific results are recorded separately in
[the validation record](PROGRAMMABLE_CATALOG_VALIDATION.md).

The design retains fenced processor workers for network execution, including
bounded `rule run --worker` steps outside the database writer lock, maintenance
cycles, catalog refresh generations, typed output
semantics and current interchange versions. It does not introduce native presets
for every operation mentioned in the design brief. Missing encoder/extractor
capabilities remain explicit; externally executed results can use normal receipts.

Catalog mappings and copy policies remain shared across profiles, while bindings
and rendition admission use profile-specific evidence. Distinct catalog IDs are
the independence boundary for projections. The redesign keeps this existing data
model explicit instead of implying that two profiles of one catalog are unrelated
output definitions.
