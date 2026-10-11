# Native media implementation checkpoint

This is an implementation checkpoint for M7 and part of M8, not an M7/M8 exit-gate declaration. Python remains the default. Native media work owns complete admitted operations without calling Python for execution; Python is used only as an independent test oracle on disposable catalog copies.

## Native ownership

- `catabolic-core::execution`: production durable-job admission, lease, fence, renewal, cancellation, retry and completion decisions consumed by actual SQLite mutations; state contains opaque evidence fingerprints rather than raw paths or tokens
- `catabolic-core::media`: probe normalization, conservative failure classification, required stream policy, bounded coverage descriptions and filename redaction
- `catabolic-runtime::process`: concurrent bounded stdin/stdout/stderr, deadlines, cancellation, explicit inherited descriptors, private process groups, descriptor-pinned child working directories, descendant cleanup and leader reaping; Linux direct-tool worker-death signals with parent-PID race checks
- `catabolic-runtime::media` and `catabolic-store::media_mutation`: explicit-ID analysis admission, immutable options/cache identity, source descriptor fencing, durable claims/heartbeats, atomic facts/baselines/attempts, retry intents, text indexing and current component inventory
- `catabolic-runtime::document`: UTF-8 text/subtitles, embedded PDF text and single-image OCR with explicit optional tools and uncertainty descriptions
- `catabolic-runtime::rendering`: fixed recipe validation, tool capability checks, all fifteen non-package presets, independent output probing, codec/stream/duration/dimension checks, bounded output and revision-bound SHA-256 verification
- `catabolic-store::output_mutation`: immutable output definitions and provenance registration which preserves existing curated associations and rejects lineage cycles
- `catabolic-store::artifact_mutation` and `catabolic-runtime::artifacts`: generated-location ownership, exclusive private temporary files, fenced rendering, durable publication intent, no-overwrite rename, atomic registration and recovery
- `catabolic-store::component_mutation`: current occurrence reads, strict accepted assertions and pinned compatibility/dependencies, reviewed extraction plans, package evidence revalidation and output lineage
- `catabolic-runtime::component_packaging` and `component_mux`: exact selected-stream extraction and multi-input remux; packet payload identity, exact-decimal timing tolerance, stream metadata/dispositions and opaque font attachment hashes are independently verified
- `catabolic-store::{receipt_mutation,processor_mutation}` and runtime receipt/processor adapters: bounded external receipts, canonical digest/idempotency, live source/output validation, atomic provenance, leased external dispatch/completion and capped nonredirecting HTTP
- `catabolic-core::fallback` and `catabolic-store::fallback_mutation`: immutable fallback policy normalization, query dependencies, profile isolation and revision/digest reuse; these functions do not resolve or publish a fallback

The current native analysis batch API is sequential and explicitly scoped to supplied job IDs. It does not yet provide the reference runner's full per-device parallel scheduling, physical-result reuse or transient-retry scheduling options. Existing shared-demand and transport APIs are not implied by analysis job support.

## Safety checks

External tools run while the owning catalog connection and writer lock are detached. Heartbeats use brief independent sessions and must match the database identity and current claim. A completion transaction rechecks token, generation, attempt, queued snapshot and options, lease and job state. Cancellation, expiry or takeover prevents the previous worker from writing facts or publishing media.

Source traversal uses no-follow descriptors and validates file revision, named identity and root identity before accepting a result. Source bytes are never modified. Rendering writes only to a distinct, empty, exclusively created output descriptor. Successful process exit is insufficient: the output is independently probed, checked against the recipe, hashed and fenced against the same observed revision before a publication intent can commit. Unknown output names are never overwritten. Partial files remain available for inspection after failure.

Probe and header results deliberately distinguish conclusive invalid evidence, unsupported codecs, environmental failures, missing required content, and incomplete/unknown analysis. A successful bounded probe is not represented as a successful full decode.

Text terms use the same frozen casefold data as native SQL and Unicode 16.0 alphanumeric ranges. `scripts/migration_media_resources.py --check` verifies those ranges using Unicode 16.0 tables. This pins native behavior instead of silently depending on the Rust compiler's changing Unicode version.

## Executable evidence

The current local checks are individually reproducible:

```sh
cargo test -p catabolic-core --locked
cargo test -p catabolic-runtime process:: --locked
cargo test -p catabolic-runtime document:: --locked -- --include-ignored
cargo test -p catabolic-runtime --test media_workflows --locked
cargo test -p catabolic-runtime --test render_boundaries --locked
cargo test -p catabolic-store artifact_mutation:: --locked
cargo test -p catabolic-runtime --test artifact_publication --locked
cargo test -p catabolic-runtime --test execution_ledger --locked
cargo test -p catabolic-runtime packet_verification:: --locked
.venv/bin/python -m unittest tests.test_rust_media -v
.venv/bin/python -m unittest tests.test_rust_fallback_policy -v
.venv/bin/python -m unittest tests.test_rust_component_media -v
```

Real-media checks require the local FFmpeg/ffprobe, Poppler and Tesseract tools. The optional document tests are explicitly selected with `--include-ignored`; an unexecuted optional-tool test is not a passing acceptance gate. Render acceptance requires every non-package preset to be available and exercises all fifteen, including AV1, HDR tone mapping, static image resizing, waveform and normalization. Source hashes are compared before and after each transform.

The Python/native analysis comparison checks separate cloned catalogs, exact snapshots/options/cache keys and durable job/attempt/fact/baseline/text/component contents. Only generated job IDs and operation timestamps are normalized. `text_terms` is compared by its logical key because the Python oracle inserts an unordered, hash-randomized set; term contents and segment relationships remain exact.

Artifact-specific tests and failpoints cover retained partial files, source changes, cancellation/takeover, ownership tampering, output checksum corruption, no-overwrite conflicts, rename/registration recovery and cross-language pending publication. Debug-only crash hooks are unavailable in release builds.

The independent durable execution ledger explores 37,448 actual SQLite mutation edges at depth five over eight explicit actions. It advances from inputs and operation disposition, never from the production reducer's next state. Eleven separate persisted identity/lease corruptions must reject completion without attempts/facts. A deliberately injected forbidden stale write is detected, reduced to three events and reproduced in a fresh process using a bounded event-only trace. This qualifies the tested durable-job slice, not the still-unported multi-caller scheduler.

Native component acceptance uses frozen Python-created plans and actual native CLI execution. Independent Python component, resolver and managed-publication readers then inspect native results. This uncovered and corrected the generated-component `probe_path` distinction (`validation.output` versus a source probe). These tests do not imply a native fallback resolver: Python acts only as fixture/oracle here.

## Reference fixture correction

The unchanged reference HDR test initially failed on FFmpeg 7.1.5 Debian. Its FFV1 stimulus supplied generic color options, but an independent ffprobe showed absent color primaries and transfer tags. Catabolic correctly rejected that untagged input. The test fixture now uses explicit libx264 VUI options and asserts the intended input HDR tags before exercising the renderer. The production safety rule and expected SDR output remain unchanged. The corrected reference test passes.

## Remaining gates

M7 still needs full scheduling/retry/cancellation surface parity, exhaustive failure/resource combinations, and the promised Linux/macOS installed-artifact/platform matrix. Receipt/processor refresh intent is durable, but automatic refresh draining remains unported. Native HTTP global deadlines do not terminate ureq's underlying DNS thread; strict worker-lifetime parity remains open. Malformed processor endpoint validation is currently stricter than Python.

M8 still needs full revision-pinned rendition/shared caller demand, component/fallback selection and reviewed package admission from a native resolver, failback/projection generations, and every existing fallback family mapped to native tests. Native execution can consume existing admitted component package jobs; native policy resolution is not implied. Artifact admission still explicitly refuses accepted-source-revision work. Linux parent-death signaling is qualified only for the direct tool, not grandchildren; equivalent macOS worker-death supervision remains open.

Fallback policy and component assertion validation reproduce the accepted schema and normalized values; their structural error descriptions are currently native diagnostics rather than Pydantic's exact multi-location error formatting. That remains a visible contract-qualification gap. No release, production-default switch or redundant Python implementation retirement is authorized by these local checks.

## Qualified local checkpoint (2026-10-11)

- Core/runtime: 86 tests passed with optional PDF/OCR tests explicitly enabled (17 core, 48 runtime unit, 7 artifact publication, 4 independent execution ledger, 8 media workflow and 2 renderer acceptance tests)
- Store ownership: 27 tests passed (11 artifacts including both Python/native recovery directions, 10 receipts, 6 processors)
- Native component CLI: five cross-language real-media tests passed, including external subtitle offset, fonts, exact extraction plan, independent projection consumption and abrupt mux worker recovery
- Refreshed analysis/policy/assertion/HDR comparisons: nine tests passed; analysis includes eleven operation/fixture pairs
- `cargo clippy -p catabolic-runtime -p catabolic-core --all-targets --no-deps --locked -- -D warnings` passed
- Unicode resource regeneration check, owned Python Ruff checks and `git diff --check` passed

Evidence logs are under `.local-tests/rust-migration/m4-plus/`: `media-integrated-final.log`, `native-component-media.log`, `media-final-differential.log`, `media-final-clippy.log`, and `media-final-store.log`. These logs describe a bounded local checkpoint; later integration changes must rerun them. No Linux/macOS release-matrix or full M7/M8 completion claim is made.
