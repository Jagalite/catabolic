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
  supervisor returns. The frozen `api serve` route remains explicitly refused

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

Run the installed checks with an explicit interpreter containing the wheel:

```
CATABOLIC_NATIVE_PYTHON=/path/to/installed/python \
  python -m unittest tests.test_native_content_revision_contract -v
python -m unittest tests.test_rust_legacy_api_worker -v
```

The selected helperless development checks do not bind a paired release wheel,
full source/artifact provenance, or all production worker workloads. Fresh
artifact-bound and independent review evidence remains necessary.

## Explicit missing owners and unchanged gates

The inventory pass found native authentication/authorization owners, but no
native principal/grant/token administration or reusable credential-file writer.
Forwarding cannot implement those missing business owners. Consequently:

- `access.principal_put`, `access.grant_put`, `access.issue` and
  `api_cli.save_secret` are still unavailable
- Legacy `api principal-put`, `grant-put`, `token-create`, `token-rotate`,
  `token-revoke`, `tokens`, `principal-disable`, `grant-disable`,
  `operation-disable`, `source-expose`, and `operation-approve` remain missing
- Only `api worker` is eligible for `adapted_unqualified` in the CLI ledger
- No authorization policy, default grant, credential persistence protocol,
  private-sidecar alias handling, or content/output safety gate was changed
- The existing private-sidecar alias and output-safety blockers remain open

Adding administration owners and credential persistence requires separately
scoped implementation and fresh review. This checkpoint does not approximate
them through Python SQL, raw callback contexts, or a second secret-file protocol.
