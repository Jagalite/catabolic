# Rust migration: paused implementation handoff

**Historical handoff:** stopped 2026-10-11 at the user's request. The
[October 13 stop handoff](RUST_MIGRATION_STOP_2026_10_13.md) now describes current
status; the original evidence below remains a historical record. The requested action is to preserve and
push the work with documentation, not to finish implementation or release it.

**Consolidated feature branch:** `codex/rust-migration-m4-plus`.
`main` remains unchanged. Python remains the default implementation. No M4–M11
hard exit gate is complete; prerequisite M0–M3 qualification remains incomplete.
All native additions are experimental unless a narrower source/platform-bound
receipt explicitly says otherwise. A passing selected test is not full parity.

## What was already implemented

The branch contains native Store/runtime owners and adapters, not merely a plan:

| Milestone | Implemented, with selected evidence | Still open |
|---|---|---|
| M4 | Schema-32 definitions, curation, revisions, interchange, query/GraphQL, plans and program/layout-related owners; selected differential suites | Complete ledger/unknown-field/digest/stale-plan parity; documented GraphQL parser/executor residuals; broad independent review unavailable |
| M5 | Guarded observations, continuation, trust/remount, scheduling, native events (Linux inotify and unqualified kqueue implementation), native scan/supervise adapters | Real macOS compilation/FS acceptance, broader mounted-filesystem qualification, remaining public scan/event/watch facades, blocked broad review |
| M6 | Native symlink/hardlink journal/recovery and selected two-way Python/native recovery fixtures | Known foreign-leaf/marker races, durable-boundary/platform matrix, safe compatible destructive protocol, blocked broad review; dangerous paths stay gated |
| M7 | Probe/media/document/artifact/processor/rendition owners, actual disposable FFmpeg/Poppler/Tesseract checks, claims/receipts/cancellation; selected public Processing/Artifacts facades | Full preset/tool/late-receipt/cancellation/platform matrix and remaining public facade contracts |
| M8 | Component inventory/packaging, fallback selection/planning/runtime owners, reviewed Components/Policies facades; new Resolver/Projection/Worker facades | New fallback adapter defects below, first installed fallback gate, all fallback families/failback/component mux fixtures and foreign-context parity |
| M9 | Consumer delivery, notifications/HTTP operations, Plex PIN/import/matching/metadata, playback/HLS/session ownership; CatalogRefresh facade and legacy refresh adapters | Real services, optional Apprise, complete growing-HLS/session/platform matrix, legacy refresh independent replay, remaining watch/maintenance/identify flows |
| M10 | Native experimental HTTP, historical contracts/generated clients, extensive legacy parser/CLI adapters, persistent Python Store/evaluation and selected public facades | Full HTTP 1.7/auth/TypeScript/public Python/installed-platform parity, missing access administration/secret writer, remaining CLI/facades |
| M11 | Local paired-wheel builder, provenance/qualification scripts, release refusals, CI lanes and docs | All hard dependencies, latest full installed cohort, actual CI/macOS/performance/rollback/release gates; no publication or Python retirement |

These groupings summarize implementation, not complete milestone acceptance.
The migration plan remains the source of the hard-gate requirements.

## Exact checkpoints and completed validation

All receipts are historical and apply only to their recorded source/artifacts.
The frozen Python oracle is always
`453fca983222c6665a48775eef67c489c96b527e`, with the pinned Python 3.14.6
interpreter for cross-language differential qualification.

### Earlier aggregate checkpoint

- Implementation/correction: `23d0f60281feed6ff4a64a3c59bcc5d440a78764`,
  `55d54483c9281efb3ca6ed73b05e589bbdfed78d`
- Evidence/documentation: `c69859eff3169545fddcb57e7d396744e48c738c`
- 526 native Python tests, 245 distinct Rust checks (242 default, two PDF/OCR,
  one journal), and Clippy passed
- Frozen reference: 1,041 run, 1,040 passed, one case-insensitive-filesystem skip
- Exact paired-wheel checks on Python 3.11.15 and 3.14.6: 13 embedding cases,
  33 paired groups, 47 selected rows (41 contract, six partial), 14 payloads
- 30 release refusals; 33 HTTP/generated-client journey cases; historical
  TypeScript 1.2/1.7 compilation; model/replay, docs/spec/resources checks
- Tracked receipt: `tests/parity/native-m4-plus-checkpoint2.json`
- Wheel SHA-256:
  `54e0df0ca6774a2b720db61ab0dddfea3a75a8801bf7be13422c5e17313c4100`

This does not qualify later commits. See [the mutation checkpoint](RUST_MIGRATION_MUTATIONS.md).

### Public-workflow continuation (phase three)

- `480d030`: source snapshot of public workflows and additional adapters
- `d4d27be`: integrates earlier checkpoint history without tree differences
- `5de6d41`: independently reviewed XML gate hardening
- `20664d5db7a342704d719a7f038963fa9a3f5f0d`: reviewed GraphQL boundary corrections
- `8325d6270ab947118bebc21b772656be419236ed`: Linux process-test fixture repair
- Selected workflow/query/processing gates select 79 preserved rows plus 24 extra
  rows, with 69/10/24 selected inventory rows in their respective gates
- Plex import/login/matching/metadata, XML encodings, CLI parser and GraphQL each
  have focused owner and independent evidence in their linked checkpoint docs
- GraphQL input ordering is confined to the boundary; workspace-wide serde map
  ordering was not changed. Existing canonical/digest assumptions are preserved
- Copied-executable ETXTBSY fixture failure was reproduced, repaired by executing
  `/proc/self/exe` directly, then checked by 20 full parallel runtime runs, 30
  process-suite runs and one parent replay. Production process behavior did not change

The phase-three aggregate's first attempt failed on that test-fixture race.
Attempt two was running at the stop request and finished before publication,
**failed**, at the unchanged exact source fence
`b18ae9a527af513f8ebf8be86157eaf81db76507184851f92882430723c4c884`.
The Python lane ran 844 tests and recorded four subtest failures in
`NativeQueryTest.test_sql_attacks_and_http_private_tables`: queries against
`api_tokens`, `execution_claims`, `watchers`, and `sqlite_schema` were refused,
but the diagnostics said `SQL query failed: access to ... is prohibited` instead
of the asserted `read-only SQL rejected`. This result does not indicate those
reads succeeded, and it does not establish complete privacy qualification.
The mismatches were not fixed after stop. Later stages of this aggregate command
were not reached and must not be reported as passed.
Its report is `.local-tests/rust-migration/phase3/aggregate-attempt-2.json`
in the `catabolic-public-workflows` worktree; the log is `aggregate-2.log`.
A small tracked stop receipt is `tests/parity/native-paused-aggregate.json`.

A local paired release-profile wheel did finish successfully at the same commit:

- Wheel: `catabolic_native-0.2.0-cp311-abi3-manylinux_2_39_x86_64.whl`
- Wheel SHA-256:
  `c667ba9783363eff8e64c6e90ddaa0e1ea6acba6e5bcb9c50cdeea8e5042eaa7`
- Packaged native helper SHA-256:
  `556156fe0d40979d9a231cbdb23de8ea4b01d9a2ed7755b7a21b9aaba1e6b01a`
- Local path: `.local-tests/rust-migration/phase3/paired-wheel-1/`

Fresh installed 3.11/3.14 gates, the new wheel's actual HTTP/client journey and
full reference replay were **not started** before the stop. This is a successful
build, not a qualified release. It does not include subsequent phase-four code.

### Reviewed facade development snapshot (phase four)

`a067cca` records independently reviewed Components/Policies, CatalogRefresh and
consumer response decoding. `749755e` merges phase three through `8325d62`.
The earlier development wheel remains historical after this merge.

- Components/Policies: 13 installed cases, 37 exact paired groups, 10 preserved
  rows; seven Store and seven installed Session lifecycle checks; additional
  independent 23 revision/eight budget/60 cursor cases
- CatalogRefresh: 16 installed cases, 43 paired groups, eight preserved rows;
  five Store checks and real-signal cancellation/rollback checks
- Consumer JSON: eight owner methods/48 response scenarios through copied CLI and
  installed extension; independent short-byte and Python-oracle corpus checks
- Parent: 16 source-contract tests and whole-checkout Ruff/format checks
- Helperless development wheel SHA-256:
  `922f7ec922275752abd42e7f4103eca6097f5b051b40a460d9bbb5018f4daf0b`
- Crate source fence:
  `6f9230216379d0a8a74007b8ea3d807b855681cbfc46802901fd6887d1a8371e`
- Manifest: `.local-tests/phase4-shared-dev-3/build-manifest.json`

See [the exact reviewed facade snapshot](RUST_MIGRATION_PUBLIC_FACADES_CHECKPOINT.md),
[Components/Policies](RUST_MIGRATION_COMPONENT_POLICY_FACADES.md),
[CatalogRefresh](RUST_MIGRATION_CATALOG_REFRESH_FACADE.md), and
[consumer JSON review](RUST_MIGRATION_CONSUMER_JSON_REVIEW.md).

## Work in the middle when stopped

The latest checkpoint preserves these additions even though full qualification
and independent review were unfinished. No planned shared development build 4
or phase-four paired release build was started.

### New fallback public facades: implemented, known defects

New Store/runtime/PyO3 owners and thin Python Resolver, Projection and Worker
adapters use persistent evaluation and native prepared-plan handles.

Completed owner checks: 12 runtime adapter tests, seven Components, six query
ownership, four unwind, seven source-contract tests, Clippy, and 27 frozen-only
paired groups. The planned installed gate is 18 cases, 27 exact groups, 15
preserved rows and two separately counted publication-refusal witnesses.
**That installed gate has not run.**

Independent diagnostic probes found three unresolved issues:

1. Dropping a prepared-plan object during an in-flight Store callback loses its
   handle release. Only 255 subsequent plans fit the 256-handle capacity
2. Cancellation after native plan preparation discards the returned token without
   releasing its handle. Capacity likewise falls to 255; the exact callback
   exception is preserved
3. An invalid evaluation token can invoke and raise the cancellation callback
   before the expected identity error is returned

The probe copied current adapters and used the existing debug extension. This is
reproducible diagnostic evidence, not a source-bound installed qualification.
Evidence: `.local-tests/fallback-facades-independent-review/probe_lifetimes.py`
and `diagnostic-lifetimes.jsonl`. No fixes were made after the stop instruction.
Potential future corrections are a Store-local deferred-release queue, cleanup
of prepared tokens when post-call cancellation prevents delivery, and callback
polling only after identity/epoch validation. Each requires fresh regression and
independent review before any qualification claim.

Foreign Access/capture callbacks, non-JSON values, worker limit differences and
publication gates remain explicit in [the fallback facade doc](RUST_MIGRATION_FALLBACK_FACADES.md).

### Legacy refresh CLI: owner-tested, independent review interrupted

`refresh configure/list/run/retry` now use native Store/runtime owners and the
bounded Jellyfin transport helper. The four ledger entries are
`adapted_unqualified`. Completed: 14 exact frozen/native integration methods,
five focused Rust tests, Clippy and a separately strengthened 15-second timeout /
cancellation test. Evidence is `.local-tests/legacy-refresh/run2/` and `run3/`;
the copied binary SHA-256 is
`89055cf9c1f7a99f4237aeb148bacc7481ca39ffbcb3b84621ad7ade0a7be674`.

Fresh independent review only inspected source and owner evidence before stop;
it ran no new tests and found no confirmed defect in that preliminary scope.
URL admission, integer overflow, read budgets, transport and identity differences
are documented in [the legacy refresh checkpoint](RUST_MIGRATION_LEGACY_REFRESH.md).

### API worker and pure content revision: owner-tested, partial surface

`api worker` forwards to the existing native supervisor. The pure revision helper
uses the caller's Store and maps only native missing-file errors to AccessError.
Completed owner evidence: five CLI methods/14 paired cases, separate native
SIGTERM cleanup and server-refusal checks; four revision methods/12 groups plus
five Store ownership checks and three source-contract checks. These helperless
checks do not bind a new paired release wheel. Fresh independent review stopped
after preliminary source inspection, with no new tests or confirmed defect.

Native principal/grant/token administration and a reusable secret writer do not
exist yet. The three preserved administration functions, save_secret and eleven
legacy administration leaves remain missing. See [the adapter checkpoint](RUST_MIGRATION_ACCESS_PUBLIC_ADAPTERS.md).

Two small ledger integration tasks were deliberately left at stop: the generator
still omits the already implemented `catalog-refresh watch`, and `api worker` is
implemented but not yet marked adapted in the generator/ledger. Do not regenerate
blindly: it would downgrade the historical watch row. Reconcile only those rows
with actual entrypoints and then rerun the ledger test on resumption.

### Strict facade wheel gates: implemented and harness-reviewed, unrun cohort

The builder now builds both CLIs from a source-fenced release cohort, packages
only `catabolic-native`, and binds the external legacy CLI in the build manifest.
Components and CatalogRefresh strict gates check source/build provenance,
installed payloads, helper identity, frozen oracle, scripts/inventory and fixed
counts; explicitly named development modes remain non-release evidence.
CI wiring was added but not executed on GitHub before stop.

Independent review found and corrected acceptance of an extra packaged legacy
CLI in both the builder and strict verifier; three negative regressions were
added. Completed retests: 62 relevant tests (34 wheel harness tests), three
additional provenance/import checks, frozen 37/43 groups, CLI bootstrap and wrong
snapshot/commit refusals, Ruff/diff. Builder SHA-256 at stop:
`6283078de7ee5136e4986b9432157823fc385ac65830c6975e59d92e9873a5bc`.
Evidence: `.local-tests/native-facade-independent-review/`.
No real new paired wheel/installed strict acceptance was performed.

## Safety and review restrictions

- Destructive symlink/hardlink replacement, removal and retirement remain refused
  by default and in release builds while the known races are unresolved.
  Debug fixture opt-ins are for disposable test roots only. DB-only hard_forget
  is distinct. Existing gates must not be bypassed to make parity tests pass
- Journal formats remain compatible; no silent native-only state protocol was
  introduced. Historical-effect adoption is not proof of current healthy output
- Native HTTP remains experimental and loopback-only. Non-Linux playback execution
  remains blocked. Private WAL/SHM alias protection and filesystem race acceptance
  remain unresolved; earlier tests do not qualify them
- Broad independent definitions/curation/programs/plans/interchange/manifest/layout
  review was blocked by a platform possible-cybersecurity-risk refusal
- Broad independent filesystem/hardlink/source-access/observation/remount/watcher
  safety review was blocked by the same class of refusal. Neither lane was retried
  or replaced through another route. Ordinary newer adapter reviews do not resolve them
- An earlier HTTP poll cancellation left that particular run's result unknown;
  it was distinct from those blocked reviews. It is not evidence of a passing run
- No ptrace qualification or actual macOS/live-service/performance/rollback claim
  is made. No package was published, no PR/merge was requested by this checkpoint,
  and no Python retirement is enabled

## Reproduction and resumption, only after a new instruction to continue

Local tool setup was `catabolic/.local-tests/rust-migration/env.sh`, Rust 1.99.0,
per-worktree `CARGO_TARGET_DIR=$PWD/target`, debug/incremental storage bounded.
Local evidence paths above are ignored workspace artifacts, **not files shipped
in the repository**. The repository contains executable suites, structured
historical receipts and this handoff; do not assume a fresh checkout has local logs.

1. Inspect the stop checkpoint and all remaining acceptance gaps before editing.
   Preserve the oracle at the exact hash above in its own checkout. Use the actual
   pinned Python 3.14.6 environment path, not a resolved symlink to another runtime
2. Resolve the documented fallback handle/callback defects and ledger drift;
   obtain fresh narrow independent review. Finish the interrupted adapter reviews
3. Build a fresh source-stable cohort. Development evidence must not be relabeled
   release evidence. The strict builder command is:

   ```sh
   python scripts/build_native_wheel.py --python /path/to/build-python --out /path/to/new-output
   ```

4. The broad source-fenced native lane is `scripts/check-native-mutations.sh` with
   `CATABOLIC_QUALIFICATION_PYTHON` set to pinned Python 3.14.6,
   `CATABOLIC_PYTHON_REFERENCE` set to the frozen checkout's `src`, and that source
   plus the working checkout on `PYTHONPATH`. Preserve each attempt separately.
   Run the full reference suite, selected ignored real-tool tests, model/fault
   replay, release refusals and artifact checks; never infer them from compilation
5. Follow `.github/workflows/native-qualification.yml` for clean installed
   Python 3.11/3.14 gates and actual HTTP/generated-client journey. For the new
   fallback gate, run outside the checkout using its explicit artifact flags:

   ```sh
   /path/to/installed/python -I -B /checkout/scripts/native_fallback_embedding_tests.py \
     --wheel /path/to/paired.whl --build-manifest /path/to/build-manifest.json \
     --inventory /checkout/tests/parity/python-api.json \
     --reference-root /path/to/frozen-checkout \
     --reference-python /path/to/pinned-3.14.6/python --report /path/to/fresh-report.json
   ```

   A helperless development run uses `--dev-wheel` and is explicitly development-only
6. Continue the missing business owners, public facades, CLI flows and documented
   GraphQL residuals. Satisfy real platform/services/performance/rollback and
   independent review requirements separately. Update exact ledger rows only when
   their acceptance evidence exists. M11 cannot close while prerequisite gates remain open

The user asked to stop; these are handoff instructions, not work continuing in
this checkpoint.
