# Catabolic local takeover handoff, 2026-10-16

The bounded installed checkpoint passed and its exact source was approved for local adoption. This document accompanies the local takeover commit; the separate recovery receipt records that commit and archive identities. Further development stops at this handoff.

## Exact source and artifacts

The candidate contains 1,688 sealed source-manifest entries. Relative to primary commit `a577438f43347a277e12c64fb9f0c1b49ca3fe3a` on `codex/rust-migration-m4-plus`, the delta is 81 additions, nine changes and no deletions. All existing source modes are retained. This summary is a separate additional document and does not alter the sealed source bytes.

- Source manifest: `/workspace/shared/catalog-final-wheel-query-owner-fix-attempt01/evidence/source-manifest-v4.json`, SHA-256 `5b9d481d48c1355f4c38db038967942a8a2f13cf179207a57872fe0f9b33e2d8`
- Complete source map: `8ea811cc773d3ecf20dd81bf81b8358dcfcbf4e4e5de383cf5fa54c5fea7ba04`
- Build source subset: `a927fd2d81adbb24fa1e36a019196f4c7b25ed0b268c1c4095fff3553a406801`
- Native wheel: `catabolic_native-0.2.0-cp311-abi3-manylinux_2_39_x86_64.whl`, SHA-256 `b993479b5e74e4ae1d3d2d3e8a2ccaabce78ff1a2a0aadb9514d38c0a0205cb2`
- Compatibility wheel: `catabolic_native_compat-0.2.0-py3-none-any.whl`, SHA-256 `e5b5a4952f4dbbc9beea3d1d15e9424816b8bbefa57073ed88ffe95a00028e1d`
- Build manifest: `/workspace/shared/catalog-final-wheel-query-owner-fix-attempt01/wheel/build-manifest.json`, SHA-256 `c7e669521dde545eac679004be09235e346325bd15ac93e03559da06cbe105d2`

Runtime changes comprise private parser diagnostic-ownership composition in `crates/catabolic-store/src/graphql_parse/boundary.rs`, initial TEMP storage selection in `lib.rs`, and preservation of caller TEMP storage during query setup in `sql.rs`. Changes in `graphql_parse.rs` are test-only. The separate 37-file compatibility package is explicitly opt-in. Existing default packaging, Cargo configuration and workflows are unchanged.

The four generated helper outputs, wheels, external executables, evidence, environments, caches and temporary directories are excluded from source adoption. Historical stage documents remain byte-exact records; their earlier uncompiled/unexecuted and superseded parser-ownership statements do not describe the present candidate.

## Actual results for the current fixed candidate

- Release build: passed, managed session 16879, with source, artifact, tool and cleanup fences
- Rust affected tests: 501 ordinary store-library tests and four new ownership regressions passed. Four named existing library opt-ins were explicitly ignored in this run and receive no new execution credit
- Installed refusal checks: 30/30 passed
- Installed baseline: exact combined terminal coverage of 48 invocations and 629 scenarios. The original attempt contributed 34 terminal-passed lanes/408 scenarios; a fresh disjoint continuation contributed 14 terminal-passed lanes/221 scenarios, with 93 fresh owned operations terminal and clean
- Python 3.11 diagnostics: 2,688 surfaces and 1,827 helper executions passed. Thirteen unsupported transports were not executed
- Python 3.14 diagnostics: 2,688 surfaces and 1,827 helper executions passed. Thirteen unsupported transports were not executed
- Python 3.11 ownership: both selected tests passed, zero failures/errors/skips, with source/runtime/artifact fences and owned cleanup verified
- Python 3.14 ownership: both selected tests passed, zero failures/errors/skips, with source/runtime/artifact fences and owned cleanup verified
- Paired installed shim: fresh corrected-checker session 16953 passed all seven phases (environment creation, clean admission, offline paired install, forwarding, console, native console control, finalization). All 159 inventory rows passed: 156 native-object identity checks and three distinct module-proxy checks, covering 35 installed modules. Console help was byte-identical to the native control. Exact source/runtime/artifact/global fences and same-owner cleanup passed; 41 old-root quarantine checks passed. Product source and both wheel payloads were unchanged

The original baseline session 33347 remains INTERRUPTED with cleanup UNKNOWN. It has no aggregate passing result. Its original records, arguments and failures remain preserved. Combined terminal-lane coverage does not turn the interrupted aggregate into a pass or claim cleanup of its unknown processes.

## Evidence anchors

Evidence paths refer to retained local qualification records, not repository-portable artifacts.

- Current composition and regression binding: `/workspace/shared/catalog-installed-query-owner-fix-actual-20261015/actual-composition-binding.json`, SHA-256 `ee53bc9f81bfa6a895ec84a22eab3990f7b6c9cc59cdc7a80120c0a187fa2895`
- Fixed release: `/workspace/shared/catalog-owner-recovery-00950e7-01/fixed-release-build-independent-verification.json`, SHA-256 `39924324f907a6f8ff6ccab5820be96cdd9fa6b93e8d2a2deb63435e72c08cba`
- Baseline reconciliation: `/workspace/shared/catalog-owner-recovery-00950e7-01/fixed-installed-continuation-independent-terminal-verification.json`, SHA-256 `bb4bbf0cc9f609ed1932d0ede0c76d571366f60b7694902e813fff2472fe27e4`
- Python 3.11 diagnostics: `/workspace/shared/catalog-owner-recovery-00950e7-01/fixed-installed-python311-diagnostic-independent-verification.json`, SHA-256 `74d46e7f463fe30c62cfb16388f215639ca8facdc95294ccd8bd2bc7114274b8`
- Python 3.14 diagnostics: `/workspace/shared/catalog-owner-recovery-00950e7-01/fixed-installed-python314-diagnostic-independent-verification.json`, SHA-256 `04f112354038b03650a1aa1facc21c14610b7a2c5d8477854c8bee382734b27d`

- Python 3.11 ownership: `/workspace/shared/catalog-owner-recovery-00950e7-01/fixed-installed-python311-ownership-independent-verification.json`, SHA-256 `6cd08e0529b736818502f7a8bf59b1d62ec0ec828412dd3db4f3da46276701da`
- Python 3.14 ownership: `/workspace/shared/catalog-owner-recovery-00950e7-01/fixed-installed-python314-ownership-independent-verification.json`, SHA-256 `97998720063ff7945078f6c97dab5d453bdda51db0b55f7403bc135485b2c4b8`

- Final installed-shim independent proof: `/workspace/shared/catalog-owner-recovery-00950e7-01/fixed-installed-shim-retry-independent-verification.json`, SHA-256 `f1e9ca03763f9f04cf0219ad557ccae0d53dea9bca0e276c1307920d0af9b371`
- Final installed-shim terminal: `/workspace/shared/catalog-installed-shim-query-owner-fix-attempt02/outer-terminal.json`, SHA-256 `751c5d2a8d7a708505ce9486be7e529593fba02eb82777bbc0c122a0f70d2b8a`
- Final installed-shim result: `/workspace/shared/catalog-installed-shim-query-owner-fix-attempt02/result.json`, SHA-256 `6f069ad28ff126912f0f01c04a8b98b26acd747145f0f59bd401d80866207e71`

## Preserved failures and checker-only retry

The first paired-wheel shim attempt installed both wheels successfully, then failed a checker assumption. Exactly three `catabolic.access` inventory rows reference frozen `src/catabolic/access/__init__.py`; the pinned compatibility wheel intentionally supplies `catabolic/access.py`. The correction is confined to the checker and its expected wheel-module mapping. The first failed result remains a failure; neither the product source nor either wheel was changed. All four phases that actually started had independently verified owned-process cleanup, and the outer process group was absent at terminal. The original aggregate cleanup flag remains false because it was coupled to full aggregate success; that original field was not rewritten. Console, native-control and finalization phases were not executed in this failed attempt. The separate fresh seven-phase retry established its own success; it did not relabel the first attempt.

Original shim failure: `/workspace/shared/catalog-owner-recovery-00950e7-01/fixed-installed-shim-attempt01-failure-independent-verification.json`, SHA-256 `42948e00fb40d114b37ad5ed8576a4741592af164eb94b97ed85a0ce3032e429`. Its unchanged original terminal is `/workspace/shared/catalog-installed-shim-query-owner-fix-attempt01/outer-terminal.json`, SHA-256 `9520820a53fd23a4221affdc04ecdf5adb3eccfe313345c8bdb1132f9812b807`.

Corrected checker: `/workspace/shared/catalog-installed-fixed-shim-retry-preparation-20261016/installed_shim_gate.py`, SHA-256 `69112fac68e7337b8373afbbfe3385edfa2e3ff74a63bce18809ee8c4533bb33`. Exactly three access-row origin assertions were corrected to the existing archive-pinned access.py bytes; the other 156 origin assertions, all forwarding checks and remaining checker AST were unchanged. Its 27 existing and 15 focused source controls passed independently on both pinned Python versions. Both attempts remain retained; the failed environment was archived losslessly before a new environment was created for the retry.

Earlier failed or interrupted attempts, including the first ownership-test expectation/API mismatch, remain retained in their original records. The separate v4 correction changed only the two test files; the production TEMP-storage fix was unchanged.

## Historical coverage and remaining limits

Full native 1,408 ordinary passes plus 28 mandatory opt-ins and two models across 104 targets, reference 1,412 passes plus one named case-insensitive-filesystem skip, and canonical 1,292 passes are historical results for the 1,647-file source map `9b2e227e7d0a2c1a2781fb764f1c2a773802559c0b85a08348faf63c5d817cce`. They are not reclassified as full qualification of the current 1,688-file candidate. Earlier failed attempts remain failures.

The R0–R7/M0–M11 migration/release program remains incomplete. Production publication and recovery, production HTTP admission, implicit output-root creation, supported-platform and service coverage, performance, rollback and default switching remain blocked or unqualified. Unknown-profile and mapping-runtime-v2 choices remain pending. Compatibility forwarding does not establish complete behavioral parity or authorize retiring the Python engine. The compatibility namespace still requires an exclusive environment because it collides with the legacy `catabolic` namespace.

### Work that can continue locally

The project is not fully blocked. The user can continue fixture-only implementation and tests for uncovered native behavior and compatibility gaps, including import/API coverage outside the 159 preserved inventory rows and the remaining production lifecycle, publication/recovery and HTTP contracts. Turn intended semantics into explicit contracts before extending them. The opt-in forwarding package does not cover the whole historical Python surface. These are future work items, not claims that this run implemented or approved them.

Fresh current-source full native, canonical and reference qualification can also be performed locally with the pinned prerequisites and new run directories. Keep ordinary tests, mandatory ignored opt-ins, model tests, installed-wheel checks and platform runs as separate evidence cohorts. Historical passing numbers are not target counts to manufacture for a new source revision.

### Decisions, access and evidence gates

Two user decisions remain: keep strict unknown-profile rejection or provide a separate legacy-compatible ordinary-read entry point; and whether mapping-runtime-v2 errors should use structured output without a traceback while preserving the approved exception class, message and exit status 1. Preserve the current fail-closed boundaries until those choices are made; this handoff does not silently choose a policy.

Previously declined independent schema, broad-definition and filesystem review lanes remain outstanding. This checkpoint does not authorize retrying or rerouting those restricted reviews.

Production publication/recovery, live service credentials or endpoints, real-catalog migration, destructive output behavior and default switching need their own product decisions, explicit authorization and applicable evidence. Supported OS/architecture, service integration, performance and rollback need real platform/service/benchmark or recovery evidence. Local fixture work can proceed without claiming these gates are satisfied. R0–R7/M0–M11 are not all complete.

## Safe local resume

Start from the recorded local commit in a separate scratch checkout. Preserve the takeover commit and recovery archive first. Use new target/evidence directories and disposable fixtures; do not reuse an old qualification run directory, old cloud supervisor, or a live catalog. The cloud evidence paths above may not exist on the user's machine: obtain the recovery package separately and retain hashes with any copied evidence. The existing native wheel is Linux x86-64/manylinux_2_39 with CPython abi3 >=3.11; it is not a macOS or Windows artifact.

With Rust 1.99.0 and cached locked dependencies already present, these are ordinary local test commands, not production commands:

```sh
export RUSTUP_TOOLCHAIN=1.99.0
export CARGO_TARGET_DIR="$PWD/.local-tests/takeover-target"
cargo test --offline --locked -p catabolic-store --test query_setup_ownership
cargo test --offline --locked --workspace
```

Missing cached dependencies or required tools are setup failures, not passing tests. The ordinary Cargo command leaves ignored tests ignored. For the distinct mandatory opt-in/oracle/model cohort, use the existing runner in that scratch checkout after preparing the pinned frozen reference, historical disposable fixtures, exact oracle interpreter, and ffmpeg/ffprobe, pdftotext and tesseract prerequisites:

```sh
CATABOLIC_QUALIFICATION_PYTHON=/absolute/path/to/pinned-oracle-python \
CATABOLIC_PYTHON_REFERENCE=/absolute/path/to/frozen-reference/src \
scripts/check-native-mutations.sh
```

This runner selects and checks required ignored test identities and executed outcomes. Do not replace it with a blanket `--ignored` pass or treat a zero-match filter as success. Follow `.github/workflows/native-qualification.yml` for the pinned oracle, generated clients, isolated installed wheel and Python 3.11/3.14 lanes. Preserve run logs and exact source/artifact identities; local prerequisites can differ from the retained cloud cohort.

For the separate bounded standalone packaging/platform smoke lane, use the repository's existing offline runner on each intended platform with its documented tools/cache prerequisites and a new absolute run directory:

```sh
scripts/standalone-native-ci.sh "$PWD" /absolute/new/takeover-standalone-run
```

It requires an absolute `CARGO_TARGET_DIR` as above. Its bounded smoke/provider results do not establish full platform, service or production release qualification. New platform wheels need fresh source-fenced builds and qualification in clean environments; do not install the compatibility and legacy distributions into the same environment.

## Ownership boundary and recovery

After the corrected checker retry, exact source adoption/local commit and explicitly requested source/patch recovery packaging, assistant development stops. The user takes over locally. No follow-on research, implementation, automatic qualification, monitoring, publication, push, default switch or real migration is implied.

The accompanying post-commit recovery receipt will record the resulting local commit and source/patch recovery filenames and SHA-256 values. It remains separate so this commit and its summary do not require an amendment merely to embed their own IDs. Do not claim recovery packaging completed before its actual verification.

This milestone permits exact source adoption and a local commit only. It does not establish production release readiness, authorize a push or default switch, or authorize real-catalog migration.
