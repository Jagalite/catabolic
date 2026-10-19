# Preserved spec roundtrip output order

This is a narrow CLI presentation change based on frozen source `721b80f`.
Python remains the default; the owner ledger still marks `spec roundtrip`
partial pending independent review and full-leaf qualification. No migration
milestone, release or default-switch gate is closed.

## Boundary and preserved behavior

The former adapter retained JSON values but sorted arbitrary extension objects.
It also applied generic CLI report-order heuristics to opaque document objects.
The new command-local sidecar records object insertion order from the original
input, only after `catabolic_store::interchange::decode` succeeds. The native
owner is called unchanged and before the sidecar, preserving existing input
limits, duplicate handling, checksum validation and refusal precedence.

The preserved Python model dump emits declared fields in model declaration
order, followed by extension fields in input order. For frozen v1-v3 models,
the embedded schemas' `required` arrays contain every declared field in that
same order. A pinned Python metadata test verifies this for all model types,
including inherited records. Only those arrays and their model references guide
presentation. Opaque metadata and unknown objects never use report-shape
heuristics, even when they contain keys such as `type`, `message`, `definition`,
`revision` or `next_cursor`.

The sidecar contains container order only. Numbers, strings, nulls and booleans
come from the validated native value, and scalar formatting reuses the existing
encoder. Standard output uses the existing machine envelope or two-space JSON
indentation. `complete: false` in an extension does not change spec exit 0.
Input files are never rewritten and stdin is read once.

No workspace serde features, canonical encoders, hashes, digests, frozen schema
resources, golden fixture bytes or business owners are changed. No Python
runtime fallback is introduced. Other command presentation and existing native
owner limitations are outside this change.

## Focused evidence

`tests.test_rust_legacy_roundtrip` runs fresh, pinned Python **3.14.6** processes
against the frozen reference `453fca983222c6665a48775eef67c489c96b527e` and compares
stdout bytes, stderr bytes and exit status without normalization:

- 54 v1-v3 comparisons permute every known and unknown object's keys across six
  seeds, ordinary JSON, explicit `--json`, and machine output; file and stdin
  paths both run
- Unknown fields are added throughout disposable copies of the frozen fixture;
  nested objects/arrays, explicit nulls, Unicode/escaped keys, huge positive and
  negative integers, finite floats, subnormals, negative zero, empty containers
  and the literal serde private-number key are retained
- Seven alternate numeric token spellings verify the existing scalar encoder
- 84 failure comparisons cover 14 malformed/duplicate/version/required-field/
  checksum cases across three output modes and both roundtrip and unchanged
  validate dispatch; combined faults check error precedence
- Three missing-argument comparisons retain parser-before-dispatch behavior
- Nine successful validate comparisons check the unchanged ordinary path
- Metadata introspection checks declaration order for every frozen model type

Three focused Rust unit tests cover opaque report-like objects, declared fields
before stable extensions, escaping, indentation and owner scalar encoding.
The existing `LC-SPEC-002` scenario now requires exact bytes rather than parsed
JSON equality. Its catalog-initializing parent suite was not run for this
spec-only change.

The local 2026-10-12 run passed all five Python test methods (157 exact CLI
comparisons plus model metadata), the three Rust unit tests, scoped Clippy with
warnings denied, Rustfmt/Ruff checks and owner-ledger regeneration checking.
These are local development results, not independent review or release proof.

Run the narrow checks with explicitly selected build and oracle artifacts:

```sh
cargo build -p catabolic-cli --bin catabolic-legacy-native --locked --offline
cargo test -p catabolic-cli --bin catabolic-legacy-native spec_roundtrip:: --locked --offline
python -m unittest tests.test_rust_legacy_roundtrip -v
cargo clippy -p catabolic-cli --bin catabolic-legacy-native --locked --offline --no-deps -- -D warnings
python scripts/legacy_cli_owners.py --check
```

Set `CATABOLIC_LEGACY_BINARY`, `CATABOLIC_PYTHON_ORACLE`, and
`CATABOLIC_PYTHON_REFERENCE` (`src/`) before the Python tests. The tests use only
synthetic/disposable spec files and local frozen Python processes: no network,
catalog initialization, credentials or live effects. A private target with debug
information and incremental compilation disabled keeps these checks separate
from qualification artifacts.

The earlier broad definitions/curation/programs/interchange/manifest/layout
review is not repeated or resolved by this formatting change. Fresh independent
review of this narrow adapter and final frozen-artifact qualification are still
required.
