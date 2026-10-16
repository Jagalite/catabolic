# Legacy GraphQL variable JSON diagnostics

This page records the original syntax-diagnostic checkpoint. The subsequent
[version-one variable admission adapter](RUST_MIGRATION_GRAPHQL_VARIABLE_ADMISSION.md)
qualifies the seven non-syntax residuals below with separate, versioned assertions;
the original checkpoint and its historical residual evidence remain unchanged.

This narrow CLI change closes the ordinary malformed `graphql --variables`
diagnostic gap retained by the [file-admission review](RUST_MIGRATION_GRAPHQL_FILE_BOUNDARY.md).
The frozen Python 3.14.6 CLI reports `JSONDecodeError`, its syntax message, and
character-based line/column/offset. The earlier native CLI flattened those
failures to `CatabolicError: invalid JSON`.

The legacy CLI now has a syntax-only JSON recognizer and a typed failure path.
The recognizer does not build values, normalize number tokens, resolve duplicate
keys, or serialize anything. Accepted values still pass through the existing
companion decoder and `InputVariables` order recorder. Literal private-number
objects remain ordinary objects. No core decoder, GraphQL value/coercion owner,
companion command, HTTP request decoder, Python wrapper, dependency, or global
serialization behavior changes.

The CLI retains source-selection and database-selection checks before file
admission. The 131,072-byte file/stdin limit still precedes variable parsing.
After reading the document, timeout, document-byte, and variable-byte admission
precede variable syntax, matching the frozen CLI. Syntax errors retain their
identity through a small additive typed-result supervisor entry point and the
existing envelope renderer; ordinary/native errors cannot acquire a syntax-error
type from matching message text. Existing supervisor callers retain the same
signal selection, cancellation flag, joined worker cleanup, and TERM policy.

## Bounded qualification

The oracle remains frozen commit `453fca983222c6665a48775eef67c489c96b527e`
under the existing pinned Python 3.14.6 environment. All CLI requests use
ordinary arguments, temporary files/stdin, and disposable catalog copies.
Neither production executable invokes Python.

`tests/test_rust_graphql_variable_errors.py` compares exact stdout/stderr and
exit status for a finite set of malformed JSON inputs in default, JSON, and
machine modes. It also covers omitted/null/scalar variables, duplicates and
escaped duplicate keys, huge accepted integer IDs, floating-point tokens,
Unicode strings/locations, private-number-object literals, input error order,
file/stdin admission, and preflight precedence. Source, executable, and catalog
identities are checked around the run.

Rust controls separately verify character-based locations, Python 3.14 trailing
comma diagnostics, typed propagation through the supervisor, same-text ordinary
error provenance, unchanged cancellation envelopes, and budget ordering.
The existing repeated-signal cleanup fixture now also exercises the typed-result
entry point, alongside the unchanged shared and standalone legacy modes.

All 15 focused Python test methods pass: 153 variable-diagnostic pairs,
55 file/stdin boundary pairs, 182 coercion pairs, and 104 runtime-input pairs.
Four additional exact cap/over-cap malformed-variable composition cases match.
All 22 release CLI unit tests and strict all-target release CLI Clippy pass,
along with Rust formatting, scoped Python lint/format, and guide freshness.
The initial immutable b116 matrix retained 48 matches and 96 differences across
144 cases; the expanded file-boundary-checkpoint baseline retained 59 matches
and 94 differences across the final 153 cases. Neither baseline was rewritten.

Fresh independent review found no blocking scoped issue: 53 exact oracle pairs,
nine unchanged residual controls, five unchanged companion controls, and three
direct immutable-harness typed/signal tests passed. It checked artifact hashes
before and after execution; its complete responses and review are retained.

Qualification results and exact source/artifact identities are recorded in
`tests/parity/native-graphql-variable-errors-checkpoint.json`. Local complete
responses, retained initial failures, build identities, focused test logs, and
independent narrow review evidence are under
`.local-tests/graphql-variable-errors/` in the isolated feature worktree.

## Limits

This is ordinary legacy CLI malformed-JSON diagnostic qualification, not blanket
JSON, GraphQL, CLI, or installed-wrapper parity. Nonfinite numbers, unpaired
surrogates, very large integer conversion limits, and depth limits remain the
existing value/admission owners' responsibility; valid JSON rejected by those
owners is not relabeled as a syntax failure. Independent well-formed negative controls retain seven pre-existing oracle
mismatches: an unused 4,301-digit integer; `NaN`, positive/negative infinity,
and a finite-syntax overflow float; and unpaired high/low surrogate escapes.
All nine residual controls (including the two passing controls) are unchanged
against the prior native artifact. This recognizer also does not reproduce
non-syntax integer-conversion failures that precede a later syntax defect.
The companion's existing malformed JSON behavior is intentionally unchanged,
verified by five direct controls. No release/default/M4–M11 gate is
closed by this work, and no source/reference checkpoint was rewritten.

Reproduction uses explicit immutable legacy/companion binaries and the pinned
oracle environment, with Cargo offline, locked, and one build job:

```sh
cargo test --release --locked --offline -j1 -p catabolic-cli --bin catabolic-legacy-native
cargo clippy --release --locked --offline -j1 -p catabolic-cli --all-targets -- -D warnings
python -W error::ResourceWarning -m unittest \
  tests.test_rust_graphql_variable_errors \
  tests.test_rust_graphql_file_boundary \
  tests.test_rust_graphql_coercion \
  tests.test_rust_graphql_runtime_inputs
```
