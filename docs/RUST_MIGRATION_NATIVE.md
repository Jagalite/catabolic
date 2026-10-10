# Native M2 and M3 qualification

M2 and M3 are implemented and locally qualified. Their M1 packaging and lifecycle
prerequisites are also qualified. The reviewed implementation snapshot is
`f884f29`; [native-m2-m3.json](../tests/parity/native-m2-m3.json) binds the source,
scenario IDs, installed artifacts and retained evidence by SHA-256.

All commits and qualification work stayed local. The Python distribution remains
the production default. `catabolic-native` and `catabolic_native` are opt-in
companions backed by the same Rust engine. Full CLI presentation, the complete
Python facade, HTTP authentication/server qualification and release/default
switching remain later milestones.

The frozen Python reference remains `453fca983222c6665a48775eef67c489c96b527e`,
schema 32. M0's broader baseline performance/resource gaps remain explicit in
[m0-gates.json](../tests/parity/m0-gates.json); this qualification does not claim
that the entire migration or M0 is complete.

## Implemented scope

M2 owns database opening, shared Python-compatible writer locks, stable read
snapshots, nested savepoints, detach/reconnect identity checks, exact schema and
history validation, canonical encodings and typed row digests. All 32 SQL
migration resources are retained byte for byte. Native hooks preserve query
canonicalization, foreign-key deferral and legacy component adoption. Upgrade
uses a validated online backup, rehearsal, concurrent-edit refusal and an atomic
live transaction. Backup files/directories have private permissions and durable
manifest writes. A failure after a successful commit reports the committed
upgrade with a warning.

Recovery eligibility is the reviewed set `1–17, 24, 25, 26, 28`, plus the current
schema. Opening a catalog neither creates it nor upgrades it. Symlink/hardlink
catalogs and invalid ledgers, schema structures or identities are refused.

M3 implements catalog entities and status; every frozen SQL view and UDF;
read-only SQL with limits, deadlines, stable-selection function restrictions and
HTTP disclosure restrictions; local GraphQL including frozen introspection,
aliases, fragments, variables, nested objects, evidence pages and error contracts;
complete SQL/GraphQL ID selections; saved-query compositions and rows/document
execution; and read-only layout planning.

Planning covers all shipped naming presets, relationship traversal, copy
preferences, rendition evidence admission, portable collisions, mapping
ownership and the empty-selection removal guard. It never publishes outputs or
modifies source bytes. GraphQL and catalog HTTP evaluation without authorization
context are refused; the complete HTTP adapter belongs to M10.

The companion Python interface exposes initialization, inspection, upgrade,
canonical JSON, SQL, catalog/GraphQL reads, selections, saved queries and layout
planning. Long native work releases the GIL. It is a companion interface, not a
claim that the complete existing Python facade has already been replaced.

## Qualification receipts

| Local platform | Final native differential scenarios | Rust checks | Installed Python wheel |
|---|---:|---|---|
| macOS 26.5.2, arm64, APFS | 69 passed, 0 skipped; 77.514s | 6 unit checks; 7 storage-fault checks | Python 3.11.15 and 3.14.6; `cp311-abi3-macosx_11_0_arm64` |
| Ubuntu 26.04, aarch64, ext4 | 69 passed, 0 skipped; 51.727s | 6 unit checks; 7 storage-fault checks | Python 3.11.15 and 3.14.6; `cp311-abi3-manylinux_2_39_aarch64` |

Linux ran in an isolated local Lima/Apple VZ VM with a read-only host checkout
mount. Source and tests were copied onto its own ext4 disk. No remote workflow,
push, container service or existing user catalog was used. These are receipts for
the listed platforms; broader release platforms and older operating-system
runtime acceptance remain M10/M11 work.

Storage qualification includes:

- Independent Python/native upgrades of all retained schema 1–32 fixtures, exact
  typed row comparisons, integrity/FK checks and Python reads of native results.
- Unchanged richer schema-14 recipes/rules/query and schema-24 probe/sidecar
  migration scenarios from the existing suite. Only new `created_at` values in
  newly created tables are checked by format and execution window, as permitted
  by plan section 9.1. Other values and historical timestamps compare exactly.
- Rollback at all 124 before/after-migration boundaries across rehearsal and live
  upgrade; 10 actual process-exit cases around migration and commit; committed WAL
  backup data; concurrent-edit cancellation; actual `SQLITE_FULL`; and correct
  post-commit warning/manifest semantics.
- Cross-language writer contention, stable snapshots, nested rollback, recovery
  eligibility, linked-path/invalid-catalog refusals and read/dry-run immutability.

Read qualification includes 28 unchanged reference catalog/SQL assertion methods
through native adapters, alongside differential SQL, GraphQL, selection, layout,
tag, component and populated workflow scenarios. Python owns fixture writes and
explicit reference calls; those are not counted as native mutation coverage.
The register lists each unique scenario instead of counting imported test classes
twice. CLI rendering and transport tests remain separately scoped.

The read checks compare values, types, ordering, cursor exchange, introspection,
validation/error locations, composed-selection budgets and failure behavior.
They exercise private-table attacks, deadlines and SQLite limits; populated jobs,
queries, projections, components, tags and work-inbox evidence; item decoration
across batch boundaries; normalized Unicode tag refusals; ownership/collision
planning; stale rendition evidence; and unchanged databases/source files.

Both installed wheels import outside the checkout on both Python versions. The
executable and extension return the same read-only SQL use case; database bytes
remain unchanged; another Python thread runs during a deadline-limited native
query; and 3,006 finite float samples encode byte for byte like Python JSON.
SQLite's actual library version is reported rather than rewritten to match a
different build. Retained logs preserve Python reference `ResourceWarning`
messages about unclosed reference connections; the qualification checks pass.

Rust 1.99.0 and dependencies are pinned. The separately qualified Statelessness
registry release is 0.2.0, with its exact checksum in Cargo.lock. Its adapter calls
the production pure reducer and independently audits caller/fence effects. The
bounded graph exhausts 52 states and 364 transitions with zero skipped checks.
An injected cancellation/stale-completion fault shrinks from eight inputs to
three and reproduces in a fresh process with an exact matching build fingerprint
(reducer, adapter and Cargo.lock). Both platforms retain their traces.

Rust formatting/Clippy and Python Ruff pass. Frozen-source regeneration verifies
all 38 resources, including schema introspection and Unicode categories.

## Performance review

The reproducible 10,000-item read benchmark reports all seven interleaved samples,
p50 and p95, after one untimed request per engine. It includes snapshot validation;
OS caches are uncontrolled and this is not the full release performance gate.

The first GraphQL comparison exposed per-item identity/workflow query preparation
in Rust. Batching those reads preserved parity and reduced the macOS GraphQL
median from roughly 196ms to 46ms in the investigation. Final artifact runs had
native/Python median ratios below 1 for SQL and GraphQL on both platforms. Exact
samples and artifact hashes are retained in the register. No validation or
boundedness was removed to obtain those results.

The local Linux reference storage acceptance also passed actual cross-filesystem
hardlink rejection, output preservation after source unmount and explicit rebind
on disposable tmpfs mounts, with successful cleanup. This closes the previously
unexecuted Linux storage capture in M0; its other open gates remain open.

## Reproduce and audit

After restoring the retained sanitized M0 fixtures under
`.local-tests/rust-migration/m0-completion/legacy-fixtures/`:

```sh
scripts/check-native-migration.sh
.venv/bin/python scripts/migration_rust_resources.py \
  --source .local-tests/rust-migration/m0-evidence-final/source --check
.venv/bin/python scripts/migration_native_audit.py
```

`check-native-migration.sh` builds release executables, runs native differential
and storage-fault checks, Clippy and the lifecycle graph. Set
`CATABOLIC_QUALIFICATION_PYTHON` to select the oracle interpreter. The native tests
also accept an explicit `CATABOLIC_NATIVE_BINARY` for artifact qualification.

Build the companion wheel with `maturin build --release --manifest-path
crates/catabolic-python/Cargo.toml`; install it into isolated Python environments
and run `scripts/qualify_native_wheel.py` with cwd outside the checkout. Benchmark
with `scripts/migration_native_benchmark.py`. Lifecycle `find` requires a fresh
trace directory; `replay` is a separate process.

Evidence remains under `.local-tests/rust-migration/m2-m3/`, release wheels under
`.local-tests/rust-migration/native-wheels-final/`, and the retained Linux binaries
under `.local-tests/rust-migration/native-artifacts/linux-aarch64/`. The audit
refuses missing or changed source/evidence/artifact bytes. Rebuilding an artifact
requires a fresh installed qualification and register update.
