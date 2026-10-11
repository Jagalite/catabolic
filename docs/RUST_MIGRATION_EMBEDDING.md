# Persistent Python embedding checkpoint

This is an **opt-in, partial M10 checkpoint**, not a replacement for the default
`catabolic` package or a declaration that the Python API is fully migrated.
`catabolic_native` now exposes `Store`, `Application`, `CatalogQuery`,
`MediaCatalog`, and `Tagging`, including matching signatures for all 23
`preserve-embedding-facade` inventory rows. The original package is unchanged.

## Ownership and public boundary

- A `Store` holds one opaque PyO3 owner of a `catabolic_store::embedding::Session`.
  Reads and mutations use that same native Store. Facade methods never call the
  path-reopening companion functions or import Python Catabolic business logic
- Read-only sessions retain the SQLite snapshot opened by the native Store
- `transaction()` uses native BEGIN IMMEDIATE and nested savepoints. Context
  exceptions roll back their own scope. Transaction tokens expire at exit,
  enforce nesting order, and reject access from a different thread
- `db` and transaction contexts expose an explicitly named
  `NativeCommandHandle`, **not** a `sqlite3.Connection`. Supported commands are
  `execute`, `executemany`, and `in_transaction`. Results are materialized
  `NativeResult` values with iteration, fetchone/fetchmany/fetchall, description,
  rowcount, and lastrowid. `NativeRow` supports named/indexed reads and dict(row)
- Native SQL parameters preserve null, text, SQLite integers, floats and blobs.
  Transaction SQL, ATTACH/DETACH and guard-disabling PRAGMAs cannot escape the
  owning Store contexts. Multiple SQL statements are rejected
- No cursor/connection impersonation, Python SQL functions, extension loading,
  shared Python sqlite connections, or arbitrary sqlite3 API compatibility is
  promised. Results are materialized rather than streaming cursors
- Long SQLite work releases the GIL and polls Python signals. SIGINT interrupts
  SQL and propagates KeyboardInterrupt after returning ownership. Closing,
  reconnecting and ending transactions use a cleanup boundary that is not
  preempted by a pending signal before cleanup
- SQLite auto-rollback or transaction ownership loss poisons every live lexical
  scope. Stale
  handles and Application/service calls cannot resume writes in autocommit mode.
  Nested contexts unwind without masking the original KeyboardInterrupt; caught
  cancellation that destroyed the transaction cannot commit an outer scope.
  Interrupted SELECTs retaining a valid transaction roll back only their exited
  context, preserving a valid outer scope. Ordinary statement-level constraint
  failures leave a still-valid transaction usable
- Native Store transactions have an unwind rollback guard. A failed cleanup
  retires the connection with a deny-all authorizer; PyO3 never restores an
  unchecked live transaction after panic
- Every operation checks its native owner out of a short-lived mutex. The mutex
  is released before any Python signal handler runs; concurrent/reentrant use
  fails explicitly. RAII restores the same session on ordinary error and unwind
- Explicit detach drops the connection and writer lease, then reacquires a
  validated Store with the original database identity. Failed reopen or database
  replacement leaves the owner unavailable. Runtime-internal detach failures also
  reject all facade access before a command can replace the failed connection
  guard. Deferred callbacks are Python-level
  callable adapters invoked only after the native owner is restored

The existing packaged helper registration and operation-specific import
cancellation bridge remain unchanged. This facade never retries a failed native
operation in the Python engine.

## Shared native services

Application status, profiles, bindings and mapping adapters live in the Store
embedding module. Item operations use native curation. Query, media-association,
relationship and tagging facades delegate to existing Store owners. Scanning
uses the shared native runtime observation owner through `&mut Store`; the
embedding does not open another Store behind an active session.

## Explicit gaps and differences

These prevent a full M10/public-Python parity claim:

1. `Application.bind(..., root=None)` can reuse an existing output binding, but
   cannot yet create the default directory for an unbound output. It fails
   explicitly and tells the caller to supply a root. No filesystem publication
   gate is relaxed
2. `Application.binding` provides the native BoundRoot mapping plus
   volume_uuid, policy, trust_path and evidence attributes. These values are
   paired against the frozen object. Arbitrary legacy Python filesystem
   consumers and their mutating evidence updates are not qualified by this
   checkpoint; no native public `filesystem.root_handle` facade is provided
3. `Application.scan` has installed fresh-single-source smoke coverage. The
   persistent-facade continuation, multi-source barrier, time-budget, internal
   reconnect callback and cancellation contracts need further qualification.
   Generic SQL SIGINT evidence does not qualify scan cancellation
4. Explicit `_db` arguments in `Application.put_item` and `MediaCatalog.relate`
   accept only same-owner native handles. Shared helpers create a nested native
   savepoint, while legacy `_db` writes directly. A caught helper failure may
   therefore roll back more of that helper's writes in the native facade
5. Detached replacement is rejected **before** reconnect callbacks. The frozen
   implementation called callbacks before its final database-ID comparison
6. The full `sqlite3.Connection`/cursor contract is intentionally outside the
   native command-handle surface. Native error classes are
   `catabolic_native.CatabolicError` (a RuntimeError subclass), not classes
   imported from the separately installed legacy package. Exhaustive sqlite
   exception-category equivalence is not qualified
7. The remaining repository-consumer service imports are still absent from the
   persistent facade. Existing companion operations do not count as persistent
   facade parity. Pure value/selected-contract tests do not qualify all possible
   arguments, failure paths or methods of a service

## Reproducible qualification

Source checks (no installed extension or silent skips):

```sh
.venv/bin/python -m unittest tests.test_native_embedding_contract -v
cargo test -p catabolic-store embedding::tests --offline
cargo test -p catabolic-store --test review_embedding_unwind --offline
```

The required installed test is a separate lane. Install the exact wheel in a
clean environment, then run from a directory outside the checkout with `-I`:

```sh
/path/to/clean/bin/python -I /path/to/repo/scripts/qualify_native_embedding.py \
  --wheel /path/to/exact.whl \
  --inventory /path/to/repo/tests/parity/python-api.json \
  --reference-root /path/to/frozen-checkout \
  --reference-python /path/to/frozen-venv/bin/python \
  --report /path/to/embedding-proof.json
```

The frozen oracle runs in a separate interpreter and imports only the frozen
source root, whose own Git root and HEAD must match the inventory pin. Tracked
package bytes are checked against committed blobs (including files marked
assume-unchanged), extra package payloads are rejected, and each imported oracle
module is bound to those frozen bytes. Both child runs bypass existing bytecode
caches. An archived directory inside a different Git checkout does not qualify.
The native scenario runs with the installed wheel and isolated
Python import mode. Both receive copies of one initialized disposable database,
so stable IDs/cursors can be compared exactly. Thirteen installed tests cover native
lifecycle/SQL/signals/concurrency and a paired scenario compares 33 selected
results across Application, Store, query, media and tagging services.

The report reuses the shared strict wheel verifier, rejects empty/incomplete
archives, binds the exact installed payload inventory to the supplied wheel, hashes every
installed package file, records the inventory hash and independently verified oracle commit, and emits
one provenance row for each of the 47 exposed inventory entries. Actual facade call counts are recorded per method; uncalled rows remain
untested. Executed rows are labelled selected-contract coverage, with
method-specific gaps; it does not
upgrade the inventory or assert all M10 rows qualified. Omitting the frozen
oracle or wheel argument produces correspondingly weaker recorded evidence.

Dev-wheel evidence is not final platform/release evidence. Parent artifact
qualification must repeat the installed suite against each exact final wheel
and promised Python/platform combination.
