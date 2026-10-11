# Persistent Artifacts and Processing checkpoint

This opt-in checkpoint exposes `catabolic_native.artifacts.Artifacts` and
`catabolic_native.processing.Processing`, also exported at package root. Its
24 class, constructor and public-method signatures match the frozen preserved
inventory. This is a separate partial M10 gate: the existing 69-signature /
19-test / 82-group embedding report and 10-row / 10-test / 77-group query report
remain unchanged. No aggregate inventory or final platform qualification is
claimed by these development results.

## Shared ownership

Both facades retain their supplied Application, Store and profile. Their calls
use the existing opaque `NativeStore` Session; no Python reducer, Python engine
fallback, path-reopening companion operation or silent retry is involved.
Bounded list, fact, attempt, duplicate and content-search reads use the same
`public_reads` owner as the terminal adapter. Content search shares the frozen
Unicode casefold/tokenizer with native text publication.

Artifact definition, binding, admission, cache verification, package/lineage
validation, execution and publication delegate to production owners. The new
admission adapter composes `_capabilities`, `_full_cache_check`,
`_source_lineage_mode` and `_component_package`; none is silently dropped.
Metadata-only cache verification can borrow an active transaction. Full cache
verification and capability discovery detach when they perform external work.
`run(_refresh=False)` suppresses the per-batch catalog refresh; default run and
recovery reuse the production refresh owner.

Processing enqueue supports explicit IDs or a bounded source page, immutable
analysis definitions, options, refresh and cache reuse. The shared runtime
scheduler preserves workers/per-device/storage-group quotas, invocation-scoped
due retries and retry delay, bounded physical-result reuse, exact requested-job
scope (including an explicitly empty list), and remaining queued-job counts.
Abandoned normalization and retry claiming use the existing execution reducer;
claims, heartbeats and final publication fences remain Store-owned. Source
validation and output authorization rules are unchanged.

Long calls borrow the same Store into a scoped worker while the calling Python
thread polls signals. Captured exceptions are latched, cancellation reaches
implicit tool-identity/capability probes as well as media workers, and every
worker/tool is cancelled and reaped before the owner returns and the original
Python exception escapes. Checksums and artifact recovery also poll this
cancellation bridge. Detached waits retain neither the SQLite connection nor
the process-wide writer lease; short heartbeat/reconnect phases still acquire
normal writer ownership. The scoped command cancellation adapter restores any
outer token on return or unwind.

## Explicit boundaries

- Native JSON-compatible capability and component-package objects are accepted.
  Foreign Python filesystem/claim objects, arbitrary callbacks or legacy Store
  connections are not transferable. Those are not silently ignored
- Runtime processing execution, artifact execution and artifact recovery refuse
  active caller transactions before starting work. External discovery and full
  cache verification also refuse if they need to detach an active transaction.
  Simple recipe writes, plain analysis admission and supplied-capability,
  metadata-only artifact admission participate in nested native savepoints
- Nonempty Processing enqueue requires a writable Store, even where the frozen
  implementation might reuse an already cached job from a read-only session.
  Empty explicit file lists remain no-op reads. Read-only listings retain their
  existing snapshot; closed/detached/poisoned and foreign-thread transaction
  access is rejected by the existing Session
- Native exceptions are `catabolic_native.CatabolicError`, rather than imported
  legacy exception classes. Arbitrary Python object arguments, SQLite BLOB or
  memoryview cursors, and exhaustive invalid-input error precedence are not
  qualified. Default artifact capability discovery occurs before durable
  admission validation, so multiple simultaneous errors can have different
  precedence from the frozen source
- Every package/historical-derivative combination, rendering preset, recovery
  phase, refresh target, large result boundary, source race and scheduler
  interleaving is not covered by this facade report. Method-specific gaps are
  recorded per inventory row. Existing blocked broad reviews remain blocked;
  this checkpoint does not replace or close them

## Separate qualification

Source and runtime fixtures:

```sh
python -m unittest tests.test_native_processing_embedding_contract -v
cargo test -p catabolic-runtime --lib public_processing::tests --offline
cargo test -p catabolic-runtime --test media_workflows --offline
cargo test -p catabolic-store --test execution_ledger --offline
```

Run the installed acceptance and frozen/native comparator from outside the
checkout using the exact paired wheel:

```sh
/path/to/clean/bin/python -I /path/to/repo/scripts/native_processing_embedding_tests.py \
  --wheel /path/to/exact.whl \
  --inventory /path/to/repo/tests/parity/python-api.json \
  --reference-root /path/to/frozen-checkout \
  --reference-python /path/to/pinned-3.14.6/bin/python \
  --report /path/to/processing-proof.json
```

The ten installed tests include real bounded FFmpeg/ffprobe processing and FLAC
publication, both cache verification modes, nested rollback, poisoned scope
refusal, snapshots, thread affinity, explicit empty scopes, writer availability,
SIGINT during tool discovery and custom Python exceptions during analysis and
artifact execution. Disposable blocking executables test cancellation and
process reaping; no live media libraries, services or credentials are used.
Six separate runtime fixture tests cover concurrency, distinct-device storage
groups, physical hardlink reuse, exact retry scope, claims, heartbeat renewal,
late publication rejection and cancellation/reaping.

The paired scenario checks 24 inventory rows and method-specific calls against
the verified frozen source in a separate pinned interpreter. Generated UUIDs
and the recipe digest dependent on its generated output-definition UUID are
normalized; each real recipe digest and sampled job cache key is recomputed and verified
before normalization. Generated timestamps and cache keys are excluded. Media content digests, stable IDs,
cursors, values, errors and legacy serialized attempt results remain compared.
The report records all installed package hashes, imported oracle module bytes,
script hashes, call counts and remaining per-method gaps.

`--dev` permits explicitly weaker helperless development evidence with a null
wheel hash and `development_only=true`. Strict `--wheel` reuses the existing
complete-helper verifier without relaxing it. Final exact helper/extension
pairing and promised platform/Python qualification remain required after all
shared source lanes freeze.
