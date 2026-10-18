# Aggregate 04 Python triage (57 failing modules)

Base: `codex/rust-migration-m4-plus` @ 9602133. Evidence: `/Volumes/seed1/Projects/catabolic-builds/evidence/retirement-20261018-aggregate-04/`.
The Rust workspace result (1579 pass, 1 fail: FFmpeg libwebp preset test) is a declined prerequisite and is not repeated here.

## Environment used (reproducible)

- Build (Kyoto only), own target dir `CARGO_TARGET_DIR=/Volumes/seed1/Projects/catabolic-builds/triage-target`, with
  `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 PYO3_PYTHON=<main checkout .venv python>`:
  - `cargo test --offline --locked --workspace --message-format=json` (forced `--no-run`)
  - `cargo build --offline --locked -p catabolic-http --bin catabolic-http-experimental`
  - `cargo rustc --offline --locked -p catabolic-python --lib -- -C link-arg=-undefined -C link-arg=dynamic_lookup`
- Tests run one module at a time from the worktree root with the main checkout venv python (3.14.6):
  `CATABOLIC_PYTHON_REFERENCE=/Volumes/seed1/Projects/catabolic-builds/reference-453fca9/src`, `CATABOLIC_PYTHON_ORACLE=<venv python>`,
  `PYTHONPATH=$CATABOLIC_PYTHON_REFERENCE:.`, `CATABOLIC_NATIVE_BINARY` / `CATABOLIC_LEGACY_BINARY` / `CATABOLIC_NATIVE_PARSER_BINARY` (`debug/examples/legacy_parse`) / `CATABOLIC_HTTP_BINARY` under `$CARGO_TARGET_DIR/debug`,
  `TMPDIR=/Users/jagatranvo/Library/Caches/catabolic-triage-tmp` (APFS).
- Extension: `debug/lib_native.dylib` copied to `$TMPDIR/ext/catabolic_native/_native.abi3.so` beside a copy of `crates/catabolic-python/python/catabolic_native`;
  `CATABOLIC_NATIVE_EXTENSION_ROOT=CATABOLIC_COMPLETION_EXTENSION_ROOT=$TMPDIR/ext`, `CATABOLIC_NATIVE_EXTENSION=$TMPDIR/ext/catabolic_native/_native.abi3.so`.
- The main venv was NOT modified (its stale `catabolic_native` 0.2.0 wheel is untouched). A throwaway venv `$TMPDIR/nvenv` (`python -m venv`, nothing installed)
  has one `.pth` listing `$TMPDIR/ext` first and the main venv site-packages second, so it imports the CURRENT `catabolic_native`.
  - `CATABOLIC_NATIVE_PYTHON=$TMPDIR/nvenv/bin/python` (docs/RUST_MIGRATION_ACCESS_PUBLIC_ADAPTERS.md: "an explicit interpreter containing the wheel") for `test_native_content_revision_*`.
  - Modules importing `catabolic_native.domain` / `CatabolicError` in-process (`test_rust_asgi_adapter`, `test_rust_asgi_smoke`, `test_rust_query_setup_ownership`) were run WITH that interpreter.
  - `test_rust_catalog_sort_diagnostics` must run with `CATABOLIC_COMPLETION_EXTENSION_ROOT` UNSET (it refuses an ambient one).
- Timing/signal-sensitive modules (19) were run 3 times each, serially, in isolation.

## Counts

| class | modules |
|---|---|
| (a) environment-only | 38 |
| (b) prerequisite / external path / declined | 8 |
| (c) flaky | 1 (test_rust_plex_login) |
| fixed here (test or generated-artifact defects) | 6 |
| (d) real failures remaining (human decision) | 4 (test_rust_legacy_review also has a (b) part) |

## Table

| module | class | evidence | fix / recommendation |
|---|---|---|---|
| test_application, test_audit_fixes, test_release_safety, test_rendition_workflows | (a) | pass 3/3 isolated; originals were exit-130 / changed-during-read / receipt timing under the long aggregate | run serially on APFS, SIGINT not ignored |
| test_synthetic_library, test_rust_selection, test_rust_queries, test_sql_query, test_queries | (a) | original `KeyError 'Straße%_Café.mkv'` etc. (HFS+ NFD TMPDIR); pass with APFS TMPDIR | APFS TMPDIR |
| test_native_content_revision_contract, _review | (a) | stale 0.2.0 package; pass with `CATABOLIC_NATIVE_PYTHON` | env var |
| test_rust_asgi_adapter, test_rust_asgi_smoke, test_rust_query_setup_ownership | (a) | `catabolic_native.domain` / `CatabolicError` missing in stale wheel; pass (15/6/2 tests) under nvenv | interpreter with current package |
| test_rust_catalog_sort_diagnostics | (a) | "ambient CATABOLIC_COMPLETION_EXTENSION_ROOT is not qualified"; 4/4 pass with it unset | unset var |
| test_rust_graphql_empty_completion | (a) | needs `CATABOLIC_COMPLETION_EXTENSION_ROOT` (docs/RUST_MIGRATION_GRAPHQL_REVIEW.md); 3/3 pass | env var |
| test_rust_existing_reads, test_rust_http_authority_review, _component_privacy, _dynamic_grants, _playback_transport, _transport, _transport_review | (a) | `catabolic-http-experimental` missing (not built by workspace test build) / default `target/` path; pass with the Kyoto-built bin | build the bin, set `CATABOLIC_HTTP_BINARY` |
| test_rust_identify, test_rust_legacy_api_worker, _api_worker_review, _api_worker_signals, test_rust_legacy_consumer_watch, _destination_mappings, _external_rule, _external_worker_diagnostics, _fallback_worker, _mapping_history, _projection, _projection_run, test_rust_observation_scheduler, test_rust_scan_accounting, test_rust_scan_watch_cli | (a) | originals were SIGINT/130, `SQL execution timed out`, scheduler/scan-accounting count and path-length effects; all pass 3/3 in isolation except external_worker_diagnostics and mapping_history (1/1; long) | none |
| test_rust_plex_login | (c) | originals: 8 failures `consumer temporarily_failed; retry delivery`. Reruns: 2 of 3 module runs fully pass; 1 run failed `test_protocol_numeric_equality_and_json_byte_encodings` with the same message; that test passes 12/12 alone. One test skipped by design. Root cause (native request to loopback fixture failing under load) not found | monitor; look at the fixture-endpoint request path if it recurs |
| test_native_catalog_schema_generation | fixed | `generation-revision-inventory.json` stale: public_reconcile.rs, filesystem_mutation.rs, source_events.rs, content_access.rs now mention generation/revision tokens; only that artifact differed | 4c961a5, regenerated with `scripts/native_catalog_schema.py --output-dir .` |
| test_rust_artifact_build (11), test_rust_qualification_plumbing_review (2), test_native_dynamic_prerequisites (2 of 3) | fixed | `'str' object has no attribute 'decode'`: fixture replaces the shared `subprocess.check_output` with a str stub; `platform.platform()` (qualify_native_mutations.py:949) shells out to `file` on macOS | 26bfe6e stubs `platform.architecture` in the fixture only |
| test_native_dynamic_prerequisites (CI test) | fixed | native job of `.github/workflows/native-qualification.yml` installed the reference with `-e`; test and docs/workflow-proposals copy require non-editable | 6d190bd (confirm with CI owner the editable install was not needed) |
| test_rust_manifest (5) | fixed | only diff `generator.version` 1.0.0 (native, 0d1c884) vs 0.2.0 (oracle) | 56c4b35 asserts oracle 0.2.0 and a well-formed native version, compares everything else |
| test_rust_native_fixture_provenance (1) | fixed | macOS `unlink(dir)` is EPERM (PermissionError), Linux EISDIR. The `vendor` addition to source_snapshot/source_digest is NOT the cause (other 25 tests pass) | ba363b3 |
| test_rust_graphql_final_composition, _grammar_provenance, _lexical_capture | (b) | `/workspace/shared/catalog-parser-lexical-capture-preparation-20261015/capture-seal.json` missing (external capture seal) | none possible here |
| test_rust_graphql_parser_budget_precedence, _string_diagnostics | (b) | one case each; oracle traceback embeds `File "/workspace/shared/catalog-python-qualification..."` vs local reference path (path-only diff) | needs reference at that path |
| test_rust_http_logical_transport, test_rust_http_management | (b) | `npm run test:1.7`: `openapi-typescript: command not found`; `npm ci --offline` ENOTCACHED | `npm ci` with network |
| test_rust_legacy_mapping_json_diagnostics | (b) | pinned oracle interpreter sha256 2f7fff38... vs this machine 71a669c1... (pinned interpreter declined) | none |
| test_rust_legacy_review | (d)+(b) | (b) `test_resource_regeneration_is_byte_identical`: embedded interpreter build string (Jun 10 2026/Clang 21 vs Aug 4 2026/Clang 22) differs. (d) 2 failures `accepted source hunk changed: fn main() {`: 15c8b64 added `catabolic_runtime::process::supervise_or_configure();` as the first statement of `main` in `crates/catabolic-cli/src/legacy_main.rs`, ahead of the exactly-guarded notification registration | NOT refreshed (reviewed acceptance guard). Needs re-review of that one-line entry-point addition (supervisor mode never returns, chooses no Python/PATH, adds no `Command::`), then extend `assert_entrypoint_separation` to the exact new line |
| test_native_legacy_adapter_qualification | (d) | (1) `tests.process_tree` is imported by declared fixture test_rust_legacy_external_rule since 363bcb8 but is not in `FIXTURE_FILES`; (2) beyond that, `legacy declared method inventory drift: ...MappingHistoryTest`: 2cc2df2 renamed `test_publication_version_two_stays_explicitly_unavailable` to `..._without_identity_refuses_explicitly` and changed its assertion (the gate it witnessed was lifted). The old id and fixture sha256s are pinned in scripts/native_legacy_adapter_scenarios.py, scripts/qualify_publication_mapping_preview.py, tests/parity/native-legacy-adapter-inventory-checkpoint.json, native-first-historical-qualification.json | NOT changed (reviewed inventory). Human decision: re-baseline the inventory (declare process_tree, new witness name, hashes) and re-review |
| test_rust_publication_bundle_preview | (d) | 14 failures: the manifest embedded in the plan carries `generator.version` 1.0.0 vs oracle 0.2.0, which flows into plan/bundle sha256 values, so the version-only normalisation used for test_rust_manifest does not apply | NOT fixed. Options: test-only version stamp on the native side, or normalise version-derived digests; both change what the test proves |
| test_documentation | (d) | `Stale bundled guides: OPERATIONS.md`: 54fdb76 added the HFS+ paragraph to docs/OPERATIONS.md but `src/catabolic/guides/OPERATIONS.md` was not synced. `src/catabolic` is the frozen 453fca9 Python tree (pinned by tests/parity/*source-manifest.json), so I did NOT keep a `scripts/sync_docs.py` run (tried and reverted). The aggregate's `PYTHONPATH=reference` imports the reference guide, which can never match docs/ | Human decision: move the paragraph to a native-only doc, or deliberately unfreeze that one guide and update the manifests |

## Commits

`git log 9602133..HEAD` on branch `worktree-agent-a69017b39abee2f6a`.
