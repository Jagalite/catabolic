# Public Python, legacy CLI, and Plex migration inventory

## Publisher continuation, 2026-10-12

The ordinary [seven-row Publisher adapter](RUST_MIGRATION_PUBLISHER_FACADE.md)
now uses the real native publication owner during Application.scan. Its selected
copied-debug evidence is separate from the historical counts below; strict
installed qualification and explicit compatibility gaps remain open.

## Current continuation checkpoint, 2026-10-11

This section supersedes the initial implementation-state descriptions below.
The original inventory and recommended dependency order are retained as historical
planning context. No entry here closes a migration hard gate or switches defaults.

This local implementation snapshot is not a qualified aggregate checkpoint.
Runtime GraphQL argument-null/default, root empty-selection and literal-diagnostic
corrections passed focused tests and a fresh independent review. That review added
313 cases, with 307 exact pairs and six confirmed pre-existing differences. Recorded
parser, static-validation, repeated-field and nested empty-selection residuals stay
open. The installed XML qualification drivers also passed an independent 64-test
harness review. See the separate GraphQL and XML qualification review documents.
The first source-fenced full aggregate on commit `20664d5` stopped at a Linux test
fixture's copied-executable `Text file busy` failure; its failed evidence is retained.
The fixture now re-execs its existing `/proc/self/exe` image instead of writing a
new executable. A disposable fork experiment reproduced the retained-writer
mechanism; 20 full parallel runtime runs and 30 process-suite runs passed with
all worker-death/PID identity assertions unchanged, plus a parent exact-case replay.
Production process behavior did not change. The complete aggregate is being rerun;
no passing final aggregate or exact paired-wheel result is claimed yet.

- Persistent ItemWorkflow/Curation, saved-query/SQL/GraphQL, and Artifacts/Processing
  adapters now call production native owners. Independent scoped reviews and
  disposable installed-development tests have passed; final helper-paired,
  source-frozen aggregate qualification remains pending
- The three separate selected gates cover **103 distinct inventory rows**:
  baseline/workflow **69 rows, 19 installed cases, 82 paired groups**; query
  **10 rows, 10 cases, 77 groups**; processing **24 rows, 10 cases, 74 groups**.
  Of these selected rows, **79** carry preserve classifications and **24** are
  additionally selected service rows. **80 of the 159 preserved rows remain
  outside these selected gates**. Neither selection nor passing selected cases
  proves exhaustive contract parity
- All four Plex workflow owners now exist, with separate scoped reviews. Shared
  XML declaration/namespace compatibility passed its independent scoped review
  after reserved-processing-instruction and empty-namespace corrections. A separate
  pre-existing Jellyfin JSON encoding gap remains recorded. Actual services,
  exhaustive crash/cancellation schedules and final installed artifacts are not
  implied by fake-loopback evidence
- The preserved opt-in CLI now has an explicit [242-row owner ledger](../tests/parity/legacy-cli-owners.json):
  **203 adapted but unqualified, 28 missing adapters, 10 partial, one safety-gated**.
  The parser's **20,170** frozen comparisons cover syntax/help, not command effects.
  See [the CLI checkpoint](RUST_MIGRATION_LEGACY_CLI.md) for selected behavior tests
- Real Linux notification intake now reaches the durable source/watcher owners.
  The independent bounded-entry finding was corrected and replayed. macOS kqueue
  remains uncompiled/unrun here; mounted-filesystem and the previously blocked
  broader safety review gates remain open

Remaining preserved-gate families are catalog refresh (8), components (5),
access/secret/content helpers (5), fallback policy/projection/resolution/worker
(20), layouts/plans/reconciliation (17), scan publication/source events/watchers
(18), ASGI create_app (1), and import/console bindings (6). These group counts
include class and import-binding rows where present and total **80**; they are
unselected-gate counts, not proof that every underlying business owner is absent.
Use the exact row IDs in `tests/parity/python-api.json` before changing a contract.

Concrete CLI implementation gaps are the aggregate watch/maintenance loops,
legacy refresh and TMDB intake, fallback worker, remaining projection/mapping
orchestration, local API administration/credential-file routes, and consumer watch.
Partial rows include explicit output-safety restrictions and documented rendering
or lifecycle gaps. New query coercion/validation and shared XML gaps discovered by
review are being implemented separately; their failing fixtures are not counted
as passing qualification.

Existing destructive-output restrictions, unregistered private sidecar-alias
limitations, blocked broad review scopes, M0–M3 prerequisite gates, real service /
macOS acceptance, performance and rollback qualification remain explicit blockers.
Python stays the installed default; no release or publication is authorized by
this checkpoint.

## Returning CLI callable continuation, 2026-10-12

The opt-in `catabolic_native.cli.main(argv=None)` boundary now forwards to the
packaged native legacy parser/dispatch/renderer with returning status, typed parse
exits and Python text-stream adaptation. The companion console and Python default
remain unchanged. See [the callable checkpoint](RUST_MIGRATION_PYTHON_CLI_CALLABLE.md)
for exact selected evidence and limitations. This is the one preserved callable
row, not a claim that the entire native CLI owner ledger has reached parity.

## Historical initial inventory and implementation order

Reference: `453fca983222c6665a48775eef67c489c96b527e`. Native branch: `codex/rust-migration-m4-plus`; inspected working checkpoint on 2026-10-11. This is a static implementation plan, not executable qualification or a security review. No repository source was changed and no tests, live catalog, login, consumer call, publication, or push was performed. Existing blocked independent review lanes remain untouched.

## Recommendation

First implement the persistent ItemWorkflow + Curation boundary: 21 preserved class/constructor/method rows, or 22 including curation.occurrence. Existing native owners already implement the business operations. Expose them through the same NativeStore Session, then qualify nested rollback, read snapshots, errors, signals, closed/detached handles, and installed exact-wheel parity. Do not implement this by calling the path-reopening companion execute_operation.

The next genuinely missing business-owner slice should be Plex library import (bounded remote read + reviewed local curation decisions). It has no remote writes and does not require an output binding. Matching builds on its page parser; metadata publication builds on import/matching evidence. PIN login is a separate owner and can be developed independently with fake service responses and disposable private directories.

## Verified size and meaning of the gap

- Frozen Python inventory: 1,027 rows. 159 have preserve classifications: 23 embedding, 127 repository-consumer, eight import-binding, one console entry point
- The native signature/installed gate explicitly selects 47 rows: the 23 embedding rows plus CatalogQuery (10), MediaCatalog (7), and Tagging (7). Existence/signature matching is not exhaustive behavioral qualification
- The 127 repository-consumer rows span 23 modules. Their persistent native import surfaces are absent. Some have path-based companion equivalents; those do not satisfy the signatures/session contract
- The 868 test-only/unobserved candidates outside preserve classifications are not all new public API promises. Their classification also does not authorize removal. 24 are selected native service rows already present, leaving 844 outside this selected gate; keep them in review rather than silently declaring them migrated
- Frozen CLI inventory: 271 parser nodes, 237 leaf nodes, and five extra saved-query dispatch variants. Native main.rs is a companion JSON parser, not this parser/renderer contract
- Plex gap: four Python workflow modules, six CLI leaves, and 63 frozen test methods (login 10, import 19, matching 14, metadata 20). General consumer transport/credential-read primitives already exist; these workflows do not

## Existing embedding methods still need closure

All Application and Store preserved methods exist, but docs/RUST_MIGRATION_EMBEDDING.md explicitly leaves:
- Application.bind(kind, owner, root=None): creating the implicit directory for an unbound output still refuses. Existing binding reuse works
- Application.binding: BoundRoot values/attributes exist, but arbitrary legacy filesystem consumers and mutable evidence are not qualified; no public native filesystem.root_handle facade
- Application.scan: fresh single-source installed smoke only; persistent continuation, multi-source barriers, amended budgets, reconnect callbacks, and scan-specific cancellation still need qualified evidence
- Application.put_item(_db=...) and MediaCatalog.relate(_db=...): same-owner native handles only; helpers use nested native savepoints, unlike direct legacy _db writes
- Store.detached: database replacement is rejected before callbacks, while frozen Python compared identity after callbacks
- NativeCommandHandle/NativeResult/NativeRow are deliberately bounded native contracts, not sqlite3.Connection/cursor impersonation. Native CatabolicError remains a RuntimeError subclass in the native package; exhaustive sqlite exception-category equivalence remains open
- Preserve the original catabolic package/import/console/extras at final packaging transition. Current wheel is catabolic-native, >=3.11, with only its opt-in console script; this is not yet pip-install-catabolic replacement compatibility

## Exact missing preserved Python surface

Names below include class/constructor rows where present. Full frozen signatures, consumers and reference-test pointers remain in [python-api.json](../tests/parity/python-api.json); parser records are in [inventory.json](../tests/parity/inventory.json). Each grouping is a missing persistent surface, not a claim its native business implementation is missing.

### src/catabolic/access/__init__.py (3 rows)
- `access.principal_put`, `access.grant_put`, `access.issue`

### src/catabolic/api_cli.py (1 rows)
- `api_cli.save_secret`

Selected native adapter and ordinary token CLI route evidence now lives in
[RUST_MIGRATION_PRIVATE_SECRET.md](RUST_MIGRATION_PRIVATE_SECRET.md). Remaining
lifecycle, path, exception and artifact-qualification gaps stay explicit.

### src/catabolic/artifacts.py (12 rows)
- `artifacts.Artifacts`, `artifacts.Artifacts.bind`, `artifacts.Artifacts.recipe`, `artifacts.Artifacts.default_output`, `artifacts.Artifacts.get_recipe`, `artifacts.Artifacts.recipes`, `artifacts.Artifacts.list`, `artifacts.Artifacts.get`, `artifacts.Artifacts.enqueue`, `artifacts.Artifacts.recover`, `artifacts.Artifacts.run`, `artifacts.Artifacts.__init__`

### src/catabolic/catalog_refresh.py (7 rows)
- `catalog_refresh.enqueue`, `catalog_refresh.finish`, `catalog_refresh.CatalogRefresh`, `catalog_refresh.CatalogRefresh.configure`, `catalog_refresh.CatalogRefresh.pending`, `catalog_refresh.CatalogRefresh.run`, `catalog_refresh.CatalogRefresh.__init__`

### src/catabolic/components.py (4 rows)
- `components.revision`, `components.index_file`, `components.backfill`, `components.get`

### src/catabolic/content_access.py (1 rows)
- `content_access.revision_of`

### src/catabolic/curation.py (13 rows)
- `curation.occurrence`, `curation.Curation`, `curation.Curation.snapshot`, `curation.Curation.put`, `curation.Curation.get`, `curation.Curation.accept_existing_association_evidence`, `curation.Curation.list`, `curation.Curation.decide`, `curation.Curation.history`, `curation.Curation.sidecars`, `curation.Curation.expected_put`, `curation.Curation.completeness`, `curation.Curation.__init__`

### src/catabolic/fallback_policies.py (5 rows)
- `fallback_policies.Policies`, `fallback_policies.Policies.get`, `fallback_policies.Policies.put`, `fallback_policies.Policies.listing`, `fallback_policies.Policies.__init__`

### src/catabolic/fallback_projection.py (8 rows)
- `fallback_projection.binding`, `fallback_projection.FallbackProjection`, `fallback_projection.FallbackProjection.bind`, `fallback_projection.FallbackProjection.plan`, `fallback_projection.FallbackProjection.run`, `fallback_projection.FallbackProjection.apply_prepared`, `fallback_projection.FallbackProjection.present`, `fallback_projection.FallbackProjection.__init__`

### src/catabolic/fallback_resolution.py (5 rows)
- `fallback_resolution.Resolver`, `fallback_resolution.Resolver.resolve`, `fallback_resolution.Resolver.health`, `fallback_resolution.Resolver.capture`, `fallback_resolution.Resolver.__init__`

### src/catabolic/fallback_worker.py (2 rows)
- `fallback_worker.configure`, `fallback_worker.tick`

### src/catabolic/graphql_query.py (1 rows)
- `graphql_query.execute_graphql`

### src/catabolic/http/app.py (1 rows)
- `http.app.create_app`

### src/catabolic/item_workflow.py (9 rows)
- `item_workflow.ItemWorkflow`, `item_workflow.ItemWorkflow.get`, `item_workflow.ItemWorkflow.checks`, `item_workflow.ItemWorkflow.note`, `item_workflow.ItemWorkflow.set_status`, `item_workflow.ItemWorkflow.log`, `item_workflow.ItemWorkflow.require`, `item_workflow.ItemWorkflow.resolve`, `item_workflow.ItemWorkflow.__init__`

### src/catabolic/layouts.py (7 rows)
- `layouts.Layouts`, `layouts.Layouts.get`, `layouts.Layouts.list`, `layouts.Layouts.put`, `layouts.Layouts.effective_definition`, `layouts.Layouts.run`, `layouts.Layouts.__init__`

### src/catabolic/plans.py (1 rows)
- `plans.projection_digest`

### src/catabolic/processing.py (12 rows)
- `processing.Processing`, `processing.Processing.enqueue`, `processing.Processing.list`, `processing.Processing.get`, `processing.Processing.attempts`, `processing.Processing.cancel`, `processing.Processing.retry`, `processing.Processing.run`, `processing.Processing.facts`, `processing.Processing.duplicates`, `processing.Processing.search`, `processing.Processing.__init__`

### src/catabolic/reconcile.py (8 rows)
- `reconcile.Reconciler`, `reconcile.Reconciler.owner`, `reconcile.Reconciler.catalogs`, `reconcile.Reconciler.preview`, `reconcile.Reconciler.apply`, `reconcile.Reconciler.recover`, `reconcile.Reconciler.verify`, `reconcile.Reconciler.__init__`

### src/catabolic/saved_queries.py (8 rows)
- `saved_queries.Queries`, `saved_queries.Queries.get`, `saved_queries.Queries.list`, `saved_queries.Queries.validate`, `saved_queries.Queries.put`, `saved_queries.Queries.select`, `saved_queries.Queries.run`, `saved_queries.Queries.__init__`

### src/catabolic/scan_execution.py (7 rows)
- `scan_execution.Publisher`, `scan_execution.Publisher.initial_scopes`, `scan_execution.Publisher.directory`, `scan_execution.Publisher.checkpoint`, `scan_execution.Publisher.stop`, `scan_execution.Publisher.flush`, `scan_execution.Publisher.__init__`

### src/catabolic/source_events.py (1 rows)
- `source_events.invalidate`

### src/catabolic/sql_query.py (1 rows)
- `sql_query.execute_sql`

### src/catabolic/watchers.py (10 rows)
- `watchers.Watchers`, `watchers.Watchers.get`, `watchers.Watchers.list`, `watchers.Watchers.put`, `watchers.Watchers.admit`, `watchers.Watchers.enable`, `watchers.Watchers.require_run`, `watchers.Watchers.history`, `watchers.Watchers.run`, `watchers.Watchers.__init__`

### Import bindings and console

- `catabolic.cli.main`
- `catabolic`
- `catabolic.catalog_refresh`
- `catabolic.component_sql.OCCURRENCES_SQL`
- `catabolic.components`
- `catabolic.documentation.TOPICS`
- `catabolic.graphql_query.SDL`
- `catabolic.http.contract.VERSION`
- `catabolic.layouts.PRESETS`

## Python implementation order and compatibility details

1. ItemWorkflow/Curation: existing `catabolic-store/src/item_workflow.rs` and `curation.rs`; expose snapshot separately because it is not a dispatch action. `components::occurrence` is crate-private, so add an appropriate Store-owned forwarding entry point for `curation.occurrence`. Preserve Curation proposal-return decoding, history cursor types and source/evidence arguments
2. SQL/GraphQL/saved queries and simple helpers: current top-level native execute_sql omits `_store`; execute_graphql omits `_store` and `_context_factory`. A new module alias alone is insufficient. Preserve same-session snapshots and explicit native context adaptation. Queries.select(session=..., _http=...) and Queries.run(session=...) need a borrowing evaluation owner, not a second Store
3. Processing/Artifacts: add persistent wrappers and missing list/facts/attempt/read adapters, while borrowing `&mut Store` for runtime calls. Preserve Processing.run workers/per_device/storage_groups/retry arguments and exact requested-job scope; preserve Artifacts.enqueue internal capabilities/cache/lineage/package arguments and `_refresh` behavior. Existing companion action parameters cover only selected contracts
4. CatalogRefresh/Watchers/Publisher/source events: preserve ongoing session ownership, refresh once-per-batch semantics, invocation `now`/trigger options, transfer semantics and execution callbacks. Publisher is a streamed publication object, not a method-name-only wrapper
5. Fallback and Reconciler/Layouts/plans: adapt policy/value/plan objects and borrowed evaluation/runtime ownership; retain every existing publication gate. This work does not replace or close blocked definition/filesystem reviews
6. Access helpers and api_cli.save_secret: native access owner exists, but module signatures and secret-file output contract remain distinct. Build only fake-credential tests here; no real authentication or grants are authorized by this planning task
7. http.app.create_app(database, read_only=True, allowed_hosts=None, origins=None, streams=8, per_principal=2, rate=120): a [real native ASGI adapter](RUST_MIGRATION_ASGI_FIXTURES.md) now exists only for native-minted disposable fixture capabilities. Ordinary catalog paths remain refused and the production constructor row remains open. Preserve this repository consumer boundary or obtain a separate explicit compatibility decision; neither the native CLI server nor fixture-only ASGI evidence satisfies production parity

## Legacy CLI: global adapter work applies to every leaf

- Restore frozen argparse tree, positional/flag/default/choice/repetition/mutual-exclusion behavior, help/version, unknown-argument handling and global-option scoping. Preserve five dynamic query commands save/show/list/run/contract
- Read CATABOLIC_DB and CATABOLIC_PROFILE. Frozen CLI refuses implicit DB selection; native main.rs currently defaults to catabolic.sqlite3 and profile default without those environment lookups
- Restore --machine v1 envelope: interface_version, command, operation, profile, outcome, exit_code, data. --json and text behavior differ; native currently ignores --json and prints JSON for everything
- Restore documented text/table/docs output, special rule/maintenance reports, raw export bytes, spec docs/schema rendering, JSON indentation/Unicode and stdout/stderr selection. --version must produce the legacy version string, not the companion version/schema/reference object
- Preserve exit 0/2/3/130 and per-command rules: safe/healthy/complete false means incomplete, query/GraphQL errors mean 2, spec diff has its own complete logic, and interruptions return 130. Current companion process returns zero on any successful Value, regardless of incomplete flags
- Native same-spelling operations (init, db upgrade, query, graphql, tag, notify, api serve/worker) are only partial overlap: JSON request arguments, missing legacy flags, output and default behavior still differ. db status is not db inspect. api serve remains explicitly gated for disposable native fixtures
- Keep the current companion API stable while adding a dedicated opt-in legacy parser/renderer. Do not silently switch the default Python command or coerce legacy operation subcommands into companion operation SURFACE ACTION syntax
- Backend availability must be checked command by command. CLI work is not merely renaming: docs/spec docs/check/roundtrip/diff do not have complete native owners in interchange::dispatch, and aggregate maintenance/watch-loop facades are not equivalent to a one-shot runtime call

### Exact leaf command inventory (all require adapter/qualification)

- processor: `processor put`, `processor show`, `processor jobs`, `processor enqueue`, `processor claim`, `processor tick`, `processor job`, `processor retry`, `processor cancel`, `processor heartbeat`, `processor dispatch`, `processor complete`
- catalog-refresh: `catalog-refresh enable`, `catalog-refresh disable`, `catalog-refresh pending`, `catalog-refresh run`, `catalog-refresh watch`
- artifact: `artifact capabilities`, `artifact bind`, `artifact recipe`, `artifact enqueue`, `artifact list`, `artifact recipes`, `artifact run`, `artifact recover`, `artifact show`
- rendition: `rendition policy`, `rendition exclude`, `rendition allow`, `rendition import-receipt`, `rendition receipt-source`, `rendition define`, `rendition register`, `rendition list`, `rendition definitions`, `rendition show`, `rendition definition`
- watch: `watch`
- proposal: `proposal put`, `proposal list`, `proposal show`, `proposal accept`, `proposal reject`, `proposal history`
- sidecar: `sidecar`
- expected: `expected put`, `expected report`
- copies: `copies put`, `copies show`, `copies plan`
- process: `process enqueue`, `process run`, `process list`, `process facts`, `process show`, `process cancel`, `process retry`, `process attempts`
- content: `content duplicates`, `content search`
- refresh: `refresh configure`, `refresh list`, `refresh run`, `refresh retry`
- identify: `identify`
- maintenance: `maintenance`
- fallback: `fallback schema`, `fallback put`, `fallback show`, `fallback list`, `fallback bind`, `fallback resolve`, `fallback preview`, `fallback execute`, `fallback enable`, `fallback disable`, `fallback history`, `fallback worker`
- component: `component schema`, `component show`, `component extract`, `component package`
- inbox: `inbox list`, `inbox show`, `inbox summary`
- rule: `rule put`, `rule list`, `rule show`, `rule stats`, `rule enable`, `rule disable`, `rule preview`, `rule apply`, `rule run`
- init: `init`
- status: `status`
- target: `target list`, `target show`, `target import`
- export: `export`
- docs: `docs`
- spec: `spec schema`, `spec docs`, `spec check`, `spec validate`, `spec roundtrip`, `spec diff`
- manifest: `manifest`
- graphql: `graphql`
- layout: `layout presets`, `layout list`, `layout show`, `layout put`, `layout preview`, `layout apply`
- query: `query`
- db: `db status`, `db upgrade`
- profile: `profile list`, `profile add`
- location: `location list`, `location scan-policy`, `location trust`, `location bind`
- catalog: `catalog list`, `catalog bind`, `catalog retained`
- remount: `remount`
- scan: `scan`
- files: `files`
- item: `item status`, `item note`, `item worklog`, `item require`, `item requirements`, `item resolve`, `item types`, `item list`, `item show`, `item put`
- tag: `tag list`, `tag put`, `tag rename`, `tag alias`, `tag parent`, `tag add`, `tag remove`, `tag assignments`
- association: `association list`, `association put`, `association disable`
- relationship: `relationship list`, `relationship put`, `relationship disable`
- mapping: `mapping list`, `mapping put`, `mapping disable`
- sync: `sync`
- verify: `verify`
- recover: `recover`
- program: `program export`, `program import`
- operation: `operation types`, `operation schema`, `operation put`, `operation show`, `operation list`
- projection: `projection schema`, `projection mapping-capabilities`, `projection http-operation-list`, `projection http-operation-put`, `projection http-operation-show`, `projection http-operation-disable`, `projection http-operation-retry`, `projection mapping-events`, `projection mapping-preview`, `projection mapping-apply`, `projection mapping-recover`, `projection put`, `projection show`, `projection preview`, `projection execute`, `projection verify`, `projection recover`, `projection enable-auto`, `projection disable-auto`
- watcher: `watcher put`, `watcher show`, `watcher enable`, `watcher disable`, `watcher run`, `watcher history`, `watcher preview`, `watcher list`, `watcher pending`, `watcher schema`
- supervise: `supervise`
- api: `api serve`, `api worker`, `api principal-put`, `api grant-put`, `api token-create`, `api token-rotate`, `api token-revoke`, `api tokens`, `api principal-disable`, `api grant-disable`, `api operation-disable`, `api source-expose`, `api operation-approve`
- consumer: `consumer plex-login`, `consumer plex-login-complete`, `consumer import`, `consumer metadata`, `consumer metadata-match`, `consumer metadata-matches`, `consumer connection-put`, `consumer discover`, `consumer bind`, `consumer create`, `consumer bindings`, `consumer connections`, `consumer attempts`, `consumer events`, `consumer creations`, `consumer publications`, `consumer disable`, `consumer enable`, `consumer retry`, `consumer verify-indexing`, `consumer run`, `consumer watch`
- notify: `notify put`, `notify status`, `notify run`, `notify disable`, `notify retry`

Additional dynamic variants: `catabolic query save`, `catabolic query show`, `catabolic query list`, `catabolic query run`, `catabolic query contract`

### CLI backend-owner groupings

- Core catalog: existing Store embedding/Application, query, tagging, curation and item_workflow owners cover substantial profile/location/catalog/item/tag/association/relationship/mapping behavior. Public CLI flags and listings remain to adapt
- Query/definition families: sql, graphql, selection, definitions, layout_mutation, plans, programs and rules provide selected owners. Keep five saved-query variants and raw SQL/file/schema/params rules distinct; existing program external-processor import explicitly refuses in runtime api
- Media/observation: media, artifacts, output_mutation, receipt_mutation, component_packaging, observation, watchers, catalog_refresh and shared rendition owners exist. Copy selection, inbox, native-event watch, service-level listing helpers, aggregate maintenance and perpetual-loop orchestration require exact per-command coverage confirmation
- Import/export: target, exports/export_bundle and runtime imports exist; adapt legacy target import/export/manifest without changing source grouping, expected plan, cancellation, raw bytes or partial outcomes
- Consumer/notification/access: existing native managed consumers, notifications and access cover selected owners; all singular consumer legacy commands still need presentation adapters, and the six Plex-specific leaves need new owners

## Exact Plex workflow-owner gaps

### plex_login (10 frozen tests)
Source: `src/catabolic/plex_login.py`

- `directory(value)`
- `read_private(filename)`
- `write_private(filename, value)`
- `identity(folder)`
- `call(route, client_id, *, method='GET', params=None, secret=None)`
- `start(storage)`
- `complete(storage, identifier)`
- `credential(reference)`

### plex_import (19 frozen tests)
Source: `src/catabolic/plex_import.py`

- `mappings(value)`
- `integer(value, label)`
- `page(adapter, library, offset, limit)`
- `candidate(app, record, part, identity, map_rows)`
- `run(database, profile, identifier, library_id, map_rows, *, offset=0, limit=100, apply=False, expected_plan=None)`

### plex_matching (14 frozen tests)
Source: `src/catabolic/plex_matching.py`

- `published_snapshot(app, binding_id, mapping_id)`
- `match(database, profile, binding_id, mapping_id, *, metadata_source='association', max_items=10000, apply=False, expected_plan=None)`
- `matched_snapshot(database, profile, c, library, match_ids, fields, clear_fields=())`

### plex_metadata (20 frozen tests)
Source: `src/catabolic/plex_metadata.py`

- `local_snapshot(database, profile, c, library, item_ids, fields, clear_fields=())`
- `remote_snapshot(a, library, local, fields)`
- `parameters(row)`
- `record_attempt(database, profile, event_id, payload, *, create=False)`
- `run(database, profile, identifier, library_id, item_ids, fields, *, lock_fields=False, unlock_fields=False, clear_fields=(), match_ids=None, apply=False, expected_plan=None)`

### Plex contract details and order

1. Library import: consumer import CONNECTION --library-id --map [--offset 0] [--limit 100] [--apply --expected-plan]. Reuse bounded native consumer HTTP/XML, server/library validation and credential readers. Add page parser (movie/episode/track/photo, id:asc, GUIDs, multipart fields and strict returned pagination), mapping normalization, deterministic server/library/rating-key identity, curation candidate snapshots and per-file durable decisions. Preserve manual metadata, unchanged idempotent repeats, explicit deferred rows and continuation. Apply refetches and fences reviewed plan; no remote writes. Max 100 records/page and 1,000 parts/page; network runs outside the Store writer lock. Page decisions are intentionally not atomic as a batch
2. Matching: consumer metadata-match BINDING --mapping [--metadata-source association|item] [--max-items 10000] [--apply --expected-plan], plus consumer metadata-matches [--binding] [--limit 100] [--after]. Capture current owned published link, unique primary association and source revision through existing owners, scan complete bounded library pages, require one exact remote path, and save accepted association evidence. Do not rewrite item identities or metadata; unchanged repeat reuses proposal. Preserve default association metadata source and item-kind requirement. The listing currently lives in consumer_cli.dispatch, not a separate plex_matching function
3. Metadata publication: consumer metadata CONNECTION --library-id (--match ... | --item ...) --field ... [--clear-field ...] [--lock-fields | --unlock-fields] [--apply --expected-plan]. Reuse accepted import/match provenance; validate the entire bounded batch before any write, preserve unselected fields/locks and explicit clears. Pin local/remote values and before states in reviewed plan, serialize same-database publication separately from the writer lock, append metadata_publish intent, send one update per remote item, read back, stop on first uncertainty, report remaining items, and never auto-retry writes. Preserve started/verified/uncertain states, 100-item selection bound, 8,192-character text, 16 KiB encoded update, 4 MiB plan and 0/2/3 exits
4. PIN login (independent): consumer plex-login [--credential-dir ${HOME}/.config/catabolic/credentials] and consumer plex-login-complete ID [same directory]. This never opens a catalog. Port/reuse stable client identity, start/poll/user-validation response parsing, expiry, resumable PIN state, repeat completion and atomic private state-to-credential transition; return authorization URL or credential file reference, never token contents. Existing native credential-file consumption does not implement login. All implementation evidence here must remain fake-service/disposable-file only

### Proposed Plex files and evidence

- `crates/catabolic-runtime/src/plex_import.rs`, `plex_matching.rs`, `plex_metadata.rs`, `plex_login.rs` for detached network orchestration; matching Store modules for snapshots, curation decisions and durable events. Factor shared pure wire/value logic rather than duplicate consumer XML/HTTP owners
- Extend runtime `lib.rs` and `api.rs` with explicit opt-in Plex workflow surfaces; later route legacy consumer commands into those owners. Do not expose login as an authenticated catalog HTTP operation unless it is already in the frozen contract
- Add `tests/test_rust_plex_import.py`, `test_rust_plex_matching.py`, `test_rust_plex_metadata.py`, `test_rust_plex_login.py` against pinned-oracle subprocesses and loopback fixtures. Start from all 63 frozen method names in the reference Plex test modules; add native actual-subprocess cancellation and same-catalog writer-progress evidence where orchestration changes
- Use existing `tests/test_rust_consumers.py` to verify transport reuse. Import tests should compare local rows/proposals/decisions and repeated/interrupted outcomes. Matching tests should use disposable published fixtures under existing gates. Metadata tests compare captured method/route/params, remote fake state, event states and lost-response outcomes. Do not run real consumer acceptance here

## Next slice: concrete files and acceptance

1. Add `crates/catabolic-python/python/catabolic_native/item_workflow.py` and `curation.py`; export classes from native __init__.py if following current selected-service convention. Preserve frozen signatures verbatim
2. Extend `crates/catabolic-python/src/embedding.rs` NativeStore.call with workflow and curation service routing; dispatch through the borrowed current Session. Add Store-owned occurrence/snapshot adapters where needed in `crates/catabolic-store/src/embedding.rs` or existing curation.rs, without path reopen or a Python business-rule implementation
3. Exact ItemWorkflow route adapter: get→get, checks→checks, note→note, set_status→status, log→log, require→require, resolve→resolve. IDs must use item_id vs requirement_id exactly; expected_revision remains arbitrary JSON-compatible integer until the native owner validates it
4. Exact Curation route adapter: snapshot→proposal_snapshot, put→proposal-put, get→proposal-get, accept_existing_association_evidence→proposal-accept-existing, list→proposal-list, decide→proposal-decide, history→proposal-history, sidecars→sidecars, expected_put→expected-put (definition becomes value), completeness→completeness. Preserve original positional/keyword shape and return shape
5. Extend `tests/test_native_embedding_contract.py` beyond hardcoded 47 rows, its no-path-reopen checks, `scripts/qualify_native_embedding.py` provenance/call accounting, and the installed paired scenario/lifecycle suite. Add explicit per-method ledger status; do not qualify uncalled rows from signature coverage
6. Reuse `tests/test_rust_item_workflow.py` and `tests/test_rust_curation.py` as native owner regressions; pair frozen `tests/test_item_workflow.py` and curation tests referenced per row. Add same-Store two-service visibility, read-only refusal, surrounding transaction rollback including caught inner helper errors, stale revision behavior, history/check cursor boundaries, close/detach/reconnect, and interrupted-query restoration
7. Run source signature tests, focused Rust owner tests, exact installed wheel paired tests outside checkout on each promised Python interpreter, then parent source-stable aggregate checks. Keep source, installed-wheel, and platform evidence separate. No milestone or release claim follows just from this facade slice

## Caveats

- This report inspected the in-progress working checkpoint; The frozen ledgers retain source hashes and reference identities. Later implementation checkpoints may change the native surface
- RUST_MIGRATION_INTEGRATIONS.md has older remaining-work bullets (for example public HTTP) superseded by later HTTP/observation checkpoints. Its explicit missing Plex-workflow statement remains accurate. Use latest owner documents and code rather than unioning stale missing lists
- No new test was executed for this inventory. Existing checkpoint receipts are referenced as historical evidence only. Blocked definition/filesystem independent reviews, real platforms, rollback, performance and release gates remain open independently of these public-surface gaps
