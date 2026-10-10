# Native migration implementation checkpoints

The reference remains `453fca983222c6665a48775eef67c489c96b527e`.
All work and commits remain local. The existing Python distribution remains the
production default; `catabolic-native` and `catabolic_native` are opt-in companions.

## Storage and query checkpoint

The workspace pins Rust 1.99.0 and dependencies in Cargo.lock. Both the executable
and the abi3 Python extension use the same Rust storage and query implementation.
Migration resources retain the original 32 SQL files byte for byte; the generator
checks the frozen source inventory before producing schema/view/UDF resources.

Local verification on macOS arm64:

- Seven native storage scenarios passed (77.398 seconds), including independent
  Python/Rust upgrades of every retained schema 1–32 fixture, typed row digests,
  full integrity/foreign-key validation, refusal of invalid history and linked
  databases, writer contention, dry-run immutability, and injected rollback.
- Nine native SQL/GraphQL differential scenarios passed (7.316 seconds), including
  every frozen SQL view, typed values, keyset cursor exchange between runtimes,
  aliases/fragments/variables, read-only attacks, HTTP SQL disclosure restrictions,
  SQLite execution limits, and immutable database checks.
- `cargo clippy --workspace --all-targets -- -D warnings` passed.
- An abi3 wheel was built and installed into Python 3.11 and 3.14 environments.
  Outside-checkout qualification and final artifact hashes remain pending.

Commands: `.venv/bin/python -m unittest tests.test_rust_migration`,
`.venv/bin/python -m unittest tests.test_rust_queries tests.test_rust_graphql`,
`cargo clippy --workspace --all-targets -- -D warnings`.
The retained local logs are under `.local-tests/rust-migration/m2-m3/`.
The persistent JSON-line executable test protocol avoids repeated macOS loader
startup stalls; each request still opens its own validated native snapshot.

## Open gates

This checkpoint does **not** close M1, M2, or M3. Remaining work includes the
shared-request lifecycle verification adapter, minimum-version installed-wheel
receipts, broader migration fault boundaries and populated component fixtures,
read-only selection/saved-query/layout planning, additional GraphQL contract and
security comparisons, and explicit parity-ledger evidence mapping.

M0 platform/performance gaps remain as recorded in RUST_MIGRATION_M0.md. No remote
workflow was dispatched and no unexecuted Linux lane is counted as passing.

## Complete selection and planning checkpoint

The Rust engine now evaluates complete SQL/GraphQL ID selections, bounded SQL
keyset pages, saved selection compositions and rows/document queries in one
caller-owned snapshot. It previews layouts (including all shipped target naming
presets), relationships, copy preferences, rendition evidence admission, portable
collisions, ownership changes, and the empty-selection removal guard. Both the
native executable and Python companion expose these read-only operations.

Additional local verification:

- 18 read-surface differential scenarios passed in 34.756 seconds.
- Two populated historical scenarios (schema 14 recipes/rules/layout selections
  and schema 24 probe/sidecar components) and five layout scenarios passed in
  31.521 seconds. Only newly generated `created_at` values in newly created
  tables are compared by format and execution window, per plan section 9.1;
  every other value and all historical timestamps compare exactly.
- Real SQLite rollback passed at all 124 before/after-migration boundaries across
  rehearsal and live upgrade; the same run checked exact recovery eligibility,
  committed WAL backup data and cancellation after concurrent edits (114.59s).
- Native unit checks passed for nested savepoint rollback, stable read snapshots,
  read-only transaction refusal, detach/reconnect locking and replacement identity.
- The rebuilt abi3 wheel imports and runs outside the checkout on Python 3.11.15;
  CLI and extension return the same SQL result without changing database bytes.
  3,006 finite float samples encode byte for byte like Python JSON.
- Rust Clippy and Python Ruff checks pass. Frozen resource regeneration verifies
  all 36 resource files, including naming presets captured from the pinned source.

New reproducible checks: `tests.test_rust_selection`, `tests.test_rust_layouts`,
`tests.test_rust_populated_migration`, `cargo test -p catabolic-store --test
storage_faults`, and `scripts/qualify_native_wheel.py` (outside-checkout cwd).

The complete HTTP server/authentication adapter remains M10 work. The read-only
GraphQL selection evaluator refuses HTTP execution without authorization context;
SQL's HTTP mode applies the frozen private-table/field disclosure restrictions.
No platform result is inferred from a macOS-only execution.
