# Pure revision and API-worker adapter checkpoint

Reference: `453fca983222c6665a48775eef67c489c96b527e`, Python **3.14.6**.
These are opt-in adapters, not full access/API parity, a public HTTP enablement,
a Python retirement, or release qualification.

## Implemented ownership

- `catabolic_native.content_access.revision_of(store, profile, file_id)` borrows
  the caller's persistent native Store through the existing PyO3 read/signal
  boundary and calls `catabolic_store::content_access::revision_of` directly
- The Python adapter contains no hashing, SQL, source opening, authorization
  policy, or database-path reopening. The existing native snapshot helper owns
  the returned revision
- `catabolic_native.access.AccessError` is the preserved error transport type.
  Missing-file revision lookups return `AccessError("not_found", 404)`; closed
  and detached owner errors are not rewritten as missing files
- `catabolic-legacy-native api worker` directly invokes the existing bounded
  `catabolic_runtime::http_worker::run` owner through the existing native signal
  supervisor. It preserves `--once`, the two-second default interval, and the
  parser's single-render-job constraint. The existing owner retains recovery,
  heartbeat, notification joining, singleton and cleanup behavior
- The CLI adapter restores the legacy interrupt presentation after the native
  supervisor returns, and preserves `CatalogBusy` for the exact API-worker
  singleton refusal. Other error categories are not reclassified. The frozen
  `api serve` route remains explicitly refused

## Selected checks

`tests/test_native_content_revision_contract.py` pins exactly three inventory
rows: the AccessError class, its constructor, and revision_of. It also checks
the thin forwarding boundary and rejects optimized qualification execution.
`scripts/native_content_revision_tests.py` produces twelve fixed paired groups
in separate frozen/installed-native processes:

1. Exact signatures and AccessError fields/defaults
2. Unobserved and observed revision digests, including Unicode and a timestamp
   larger than the exact integer range of an IEEE-754 double
3. Alternate profile and missing-file behavior
4. Revision changes for individual observation fields, the stored policy and
   volume identity
5. Same-owner nested/outer rollback and read-only results

Five additional installed-native checks cover uncommitted-row visibility and
rollback, detached refusal/reconnect, ordinary Python interrupt rollback/reuse,
closed-owner precedence, and a caller-pinned read-only snapshot while a separate
writer changes the catalog. These are selected signal schedules, not exhaustive
timing or corruption qualification. Invalid non-string IDs/profiles, foreign
Python Store objects, and exact SQLite exception-category parity are not claimed.

`tests/test_rust_legacy_api_worker.py` uses only empty disposable catalogs and
compares exact stdout/stderr/exit for empty `--once`, catalog-busy, missing-db,
single-render-job parser bounds, and SIGINT scenarios. It checks heartbeat
cleanup and continued `api serve` refusal. Native SIGTERM cleanup is a separate
native-only case: frozen Python does not install an equivalent SIGTERM handler.
No fixture creates accounts, grants, tokens, credential files, recipients,
render work or network services. Optional media programs are absent from PATH.

Signal readiness requires the selected profile and exact child PID, not any
heartbeat row. The frozen paired SIGINT fixture also uses the explicit
`tests/frozen_api_worker_signal_fixture.py` idle-loop trace boundary, after the
tick Store has closed. It imports and executes the actual frozen modules without
rewriting their bytes, announces the selected boundary over a private pipe, and
waits there for a real parent-sent SIGINT. Native behavior is not instrumented.
Exact interrupt stdout/stderr/exit, an empty heartbeat table, and immediate
successful restart with cleanup remain required. These are selected schedules,
not evidence that every possible cancellation boundary cleans up.

`tests/test_rust_legacy_api_worker_signals.py` rejects stale-worker and
same-PID/other-profile readiness, including through the actual interrupt
fixture's control flow. A separate frozen-only diagnostic selects SIGINT after
the tick Store closes its SQLite connection but before it closes its writer
descriptor. In plain/JSON/machine modes that selected schedule returns the normal
interrupt result but leaves its heartbeat; an immediate fresh `--once` run then
clears it. No external writer is held during that signal. This demonstrates one
possible frozen cleanup-miss schedule. It does not establish the cause of the
earlier uninstrumented intermittent failure, change the frozen implementation,
or relax the idle fixture's cleanup assertion. The external-writer-lock
best-effort fixture below is a different case and remains unchanged.

### Independent forwarding and supervisor probes

`tests/test_native_content_revision_review.py` adds source-only fake-owner
checks for exact arguments, result identity, no path inspection, and exact
`not_found` translation while preserving unrelated exceptions. These source
checks are not installed-wheel evidence. Its separately routed installed test
pairs writable autocommit, nested commit/error reuse, unknown string profiles,
and read-only transaction retention. Native-only probes check that transaction
thread ownership and implicit-rollback errors precede a missing-file result,
and that the caller can reuse the owner after scope cleanup.

`tests/test_rust_legacy_api_worker_review.py` adds exact paired envelopes for
profile errors and environment-selected profiles, valid-profile singleton
refusal in plain/JSON/machine output, SIGINT, and immediate restart. The singleton
probe exposed and now guards the legacy `CatalogBusy` transport correction.
An external catalog-writer-lock cancellation fixture also preserves the saved
best-effort cleanup behavior: both engines release the singleton and return
interrupt status while a heartbeat may remain if its deletion cannot acquire
the writer. A subsequent successful empty `--once` run clears that row.

The supervisor now follows the frozen admission boundary: resolve the database
path, acquire the singleton, then open the initial native identity snapshot.
Profile validation remains in each short writable/reopened owner, after its
writer admission and identity check. A competing request with an invalid profile
therefore reports `CatalogBusy`, and an external catalog writer produces the
ordinary `catalog_busy` result before profile validation. An invalid profile
without contention still reports `unknown profile`. Database identity checks,
writable/standalone caller requirements, Linux qualification gating, and signal
cleanup are retained.

The review tests compare exact plain/JSON/machine envelopes for these competing
errors, unknown-profile retry after the original worker stops, and successful
restart after each admission failure. They also retain missing-database error
precedence, original-worker heartbeat ownership, SIGINT, and best-effort cleanup
coverage. The native unit checks pin identity-before-profile rejection and
writer-lease release, plus writable/transaction refusal for invalid profiles.
These are selected empty-catalog admission checks, not a broader worker workload
or release qualification.

Run the installed checks with an explicit interpreter containing the wheel:

```
CATABOLIC_NATIVE_PYTHON=/path/to/installed/python \
  python -m unittest tests.test_native_content_revision_contract \
    tests.test_native_content_revision_review -v
python -m unittest tests.test_rust_legacy_api_worker \
  tests.test_rust_legacy_api_worker_review \
  tests.test_rust_legacy_api_worker_signals -v
```

The selected helperless development checks do not bind a paired release wheel,
full source/artifact provenance, or all production worker workloads. Fresh
artifact-bound and independent review evidence remains necessary.

## Subsequent local administration checkpoint

The [owner-only checkpoint](RUST_MIGRATION_ACCESS_ADMIN_OWNERS.md) adds three
local administration owners. The subsequent
[public facade checkpoint](RUST_MIGRATION_ACCESS_ADMIN_FACADES.md) adds their
three preserved Python adapters and nine ordinary local CLI leaves, with
selected-contract evidence and explicit transport/SQLite diagnostic gaps.
Neither checkpoint qualifies complete access/API parity.

`api_cli.save_secret`, legacy `api token-create` and `api token-rotate` remain
unavailable pending a separately reviewed credential-file writer. Native
scalar-ID token authentication remains intentionally fail-closed because the
frozen cache can collide across distinct grants. `api serve` refusal, M6 safety
blockers, publication restrictions and default-engine choices remain unchanged.
