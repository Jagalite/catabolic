# Legacy GraphQL variable admission diagnostics, version 1

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

This narrow error-only adapter qualifies the seven well-formed variable cases
left open by [ordinary variable syntax diagnostics](RUST_MIGRATION_GRAPHQL_VARIABLE_ERRORS.md):
a 4,301-digit integer, `NaN`, positive and negative infinity, finite-syntax
floating-point overflow, and unpaired high/low surrogate escapes.

The frozen Python 3.14.6 CLI rejects all seven. Six differences were refusal
type/message differences; the unused oversized integer was a legacy CLI
acceptance difference. The adapter returns `ValueError` for integer conversion
and nonfinite serialization, and `UnicodeEncodeError` for unpaired surrogates.
It preserves complete default, JSON, and machine output envelopes, including
Python float spelling and character positions in canonical JSON.

## Ownership and safety boundary

Only the legacy `graphql --variables` admission path uses this adapter, after
existing source, file/stdin, timeout, byte-budget, and syntax checks. A private
diagnostic tree preserves surrogate code points, sorted keys, and last-key
values solely to compute an error. It cannot be returned to a resolver or Store.
Successful admission still hands the original input to the unchanged hardened
shared decoder and `InputVariables` order recorder. The 4,300-digit integer
limit applies at this CLI boundary, including nested and overwritten integers,
without limiting arbitrary integers in the shared decoder or companion CLI.

A typed failure variant carries the refusal through the existing supervisor
and envelope renderer. Matching an ordinary error's message cannot change its
type. Existing cancellation and repeated-signal cleanup behavior remains covered.
No core, Store, HTTP, Python wrapper, dependency, transport, or GraphQL parser
owner is changed.

Duplicate-overwritten `NaN`, infinity, and surrogate inputs retain the existing
strict decoder refusals even where Python would discard the invalid value.
Overwritten finite-syntax overflow (`{"x":1e400,"x":0}`) was already accepted by
both the existing decoder and frozen Python; that acceptance is explicitly
retained. No duplicate policy or strict decoder behavior is broadened.

## Versioned qualification

The original `native-graphql-variable-errors-checkpoint.json` and its historical
residual assertions remain byte-for-byte unchanged. New equality assertions
live in `graphql-variable-admission-v1.json` and
`test_rust_graphql_variable_admission.py`; the new checkpoint records their exact
source, artifact, oracle, and evidence identities.

The version-one matrix uses immutable baseline and candidate executables and
the frozen Python commit `453fca983222c6665a48775eef67c489c96b527e` under Python
3.14.6. It compares full stdout, stderr, and exit status in all three output
modes, plus file/stdin query sources for the seven historical cases. Catalog,
fixture, executable, and frozen-source hashes are fenced around the run.
Controls include ordinary 4,299/4,300-digit integers, negative and nested
integers, integer strings and keys, scalar/top-level shape, sorted and escaped
duplicate keys, private-number-object literals, finite extremes, underflow,
Unicode/astral/control-character canonical positions, and refusal precedence.

See `tests/parity/native-graphql-variable-admission-v1-checkpoint.json` for the
completed bounded results and retained failed attempts. These are author
qualification results, not an independent review or release qualification.

## Portable current-source regression

The original live-baseline matrix, input fixture, and
`native-graphql-variable-admission-v1-checkpoint.json` remain unchanged.
`graphql-variable-admission-baseline-v1.json` is a separate historical response
fixture derived from the sealed 242-result matrix. Every recorded exit status,
stdout string, and stderr string is unchanged, including whitespace and escaped
characters; none contains an ephemeral path. A whole-file SHA-256 pin in the
harness binds the source commit, original source-manifest and qualification-test
hashes, baseline executable hash/size, original report and checkpoint hashes,
input-fixture identity, pinned oracle provenance, and all 242 responses. Each row
also binds its name, mode, query source, expectation, document, and variables.

The standard aggregate/CI entry point does not need to provision a historical
executable. When `CATABOLIC_VARIABLE_ADMISSION_BASELINE` is **unset**, the test
requires the pinned historical response fixture and runs the current native
executable and live frozen Python oracle for every one of the 242 variants.
There are 242 native runs, 242 oracle runs, and zero live baseline runs. Recorded
responses are used only as historical comparison data. Equality assertions still
compare the live native and oracle; strict controls still require the unchanged
historical refusal and a distinct live oracle response; the already-accepted
overflow control still requires native/oracle/historical equality and exit zero.

When `CATABOLIC_VARIABLE_ADMISSION_BASELINE` names an executable, all 242 baseline
runs also execute. Every live baseline response must match the pinned historical
response, including cases whose native assertion is oracle equality. The supplied
artifact's actual hash is fenced and reported independently; a different build
cannot evade the response checks. An empty setting, a missing explicit artifact,
a missing or changed historical fixture, provenance/input drift, or incomplete,
duplicated, or reordered rows fails qualification. Nothing silently skips a case
or switches a requested live baseline run to recorded mode.

Report version 2 distinguishes `historical_baseline` from `live_baseline`, names
the baseline mode, and reports actual matrix process-run counts. The oracle's
separate temporary-catalog initialization is not counted as a matrix case.
Six fast integrity/drift tests accompany the single 242-variant live regression;
the module therefore contributes seven methods to dynamic aggregate discovery.
The harness-only qualification is recorded separately in
`native-graphql-variable-admission-harness-v1-checkpoint.json`. It changes no
production Rust, fixture acceptance assertions, historical receipt, or release
gate.

## Limits

This does not claim blanket JSON, GraphQL, transport, wrapper, or installed
parity. Depth limits and other existing strict-boundary differences remain.
The integer ceiling models CPython's default configuration; environment overrides
to that ceiling are not qualified. At this checkpoint syntax validation preceded this adapter: an oversized
integer followed by a syntax defect retained the prior native syntax error, even
when Python's integer conversion fails first. Track D closes that precedence
residual; see below. No release/default/M4–M11 gate closes, no blocked review is
retried or replaced, and unqualified effects remain fail-closed.

Reproduce with explicit immutable baseline/candidate binaries, the pinned oracle,
and the official pinned Rust toolchain. The recorded build is locked/offline,
non-incremental debug with debug information disabled and two build jobs.
Set `CATABOLIC_LEGACY_BINARY` and `CATABOLIC_NATIVE_BINARY` to the current
candidate copies, `CATABOLIC_PYTHON_ORACLE` to the pinned interpreter, and
`CATABOLIC_PYTHON_REFERENCE` to the frozen `src/` directory. Leave
`CATABOLIC_VARIABLE_ADMISSION_BASELINE` unset for the portable mode, or point it
to an explicit baseline executable for live triple comparison. Set
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`, and
`CARGO_INCREMENTAL=0`; use a dedicated `CARGO_TARGET_DIR`.


```sh
cargo build --locked --offline -j2 -p catabolic-cli --bins
cargo test --locked --offline -j2 -p catabolic-cli --bin catabolic-legacy-native
cargo clippy --locked --offline -j2 -p catabolic-cli --all-targets -- -D warnings
python -W error::ResourceWarning -m unittest \
  tests.test_rust_graphql_variable_admission \
  tests.test_rust_graphql_variable_errors \
  tests.test_rust_graphql_file_boundary \
  tests.test_rust_graphql_coercion \
  tests.test_rust_graphql_runtime_inputs \
  tests.test_rust_json_boundaries
```

## Track D: integer-conversion versus syntax precedence (October 16)

Frozen `json.loads` converts each integer token as soon as the scanner reaches
it, so an integer beyond the 4,300-digit default fails with `ValueError` before
any later syntax defect, while an earlier syntax defect still wins. The legacy
`graphql --variables` boundary now runs the syntax recognizer with that integer
check in scan order (`json_diagnostic::validate_with_integer_limit`). Fraction
and exponent tokens are floats and are not limited; a leading-zero token stops
at `0`, so the following digits are a syntax error. The shared `validate` used
by other JSON boundaries is unchanged, and the private diagnostic tree still
owns nonfinite and surrogate refusals afterwards.

The pinned version-one input and historical rows are unchanged. The
`syntax-after-huge` row's current assertion is replaced by a versioned
`superseded_equal` mode: live native output must equal the oracle **and** the
pinned historical response must differ from the oracle. A new
`test_rust_graphql_variable_precedence.py` compares 15 precedence inputs in all
three output modes with the live frozen oracle, and Rust unit tests cover scan
order, the 4,300-digit boundary, float tokens and duplicate keys.

Local macOS arm64 results at commit `065d6f2`: the precedence module passes
(45 envelopes); the admission module passes 8 methods (242 live comparisons);
variable-error and file-boundary modules pass (153 + 55 pairs); and the legacy
unit executable passes 34 functions, including the new scan-order test.
