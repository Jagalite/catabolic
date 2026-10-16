# Native GraphQL parser boundary checkpoint

This bounded native Rust slice follows `6b2d401da58f16bd0425080b87e43902b959792f`.
It closes the 14 previously explicit `remaining_parser_controls` in
`tests/parity/graphql-parser-cases.json`. Their original documents, frozen
responses, `native` pre-fix captures, and any earlier review captures remain
verbatim; the cases now participate in the passing-case suite. The provenance
lists all 14 names, so promoting them does not erase the historical failures.

## Implementation boundary

Only `crates/catabolic-store/src/graphql_parse.rs` and its parser-local
`graphql_parse/boundary.rs` module change production behavior. There is no
schema, validation-rule, runtime, HTTP, CLI, Python wrapper, or PyO3 replacement.

The source recognizer lazily follows executable grammar to establish ordering
between errors. It owns malformed Unicode escapes and variable-definition
syntax: wrapped type punctuation, constant defaults, and constant directive
arguments, including nested lists and input objects. A variable is diagnosed
only in a value position. Argument names and object keys still require names.
The recognizer does not construct an alternative AST or execute values.
Unrelated earlier syntax errors remain with the existing framework parser.

GraphQL places a variable default before directives. The framework grammar
expects the opposite order. A private parser view reorders only grammar-admitted
ranges; it composes existing surrogate-pair and wrapped-type edits and maps AST
positions back to the unchanged source. The source's inverse ordering, a second
default, malformed values, and invalid directive names are rejected. Comments,
CR/LF/CRLF, block strings, supplementary characters, multiple variables, and
nested defaults are covered. All public validation and execution continue using
the ordinary framework AST and the original request text.

Bare-CR normalization is also carried forward from the coordinated runtime
correction: distinct accepted fields cannot collide in source-position-keyed
argument plans. The exact null-argument witness is replayed with LF, CR, and CRLF.

The original document remains the document-byte and token-budget input. The
recognizer uses iterative input and selection walks rather than recursive
recognizer-only cutoffs; the framework
recursion guard is preserved after adaptation. Existing depth, deadline,
interruption, connection ownership, transaction, TEMP state, and `query_only`
contracts remain in force.

## Evidence and limits

The fixture contains 191 full-result parity cases and five explicit remaining
non-parity controls. Tests compare parsed complete envelopes, process exits,
stderr, direct Rust execution, local and HTTP-schema preflight, and persistent
owners in both `query_only` states. HTTP-schema labels are preflight contexts,
not a transport-coverage claim. Existing GraphQL regression suites supplement
these focused cases.

The five retained differences are:

- Unrelated malformed field arguments before a later Unicode error
- Unrelated malformed field arguments before a later constant-value error
- Unrelated malformed field arguments after an admitted default/directive pair
- Valid variable-width/braced Unicode escapes, which the framework still rejects
- An unrelated missing field argument value

These controls are not counted as parity. They retain complete frozen and native
responses. The original-source excerpt fallback is deliberately retained for
unrelated framework syntax diagnostics; this checkpoint does not claim general
GraphQL parser equivalence.

The oracle is frozen commit `453fca983222c6665a48775eef67c489c96b527e`, Python
3.14.6, graphql-core 3.2.12. Added historical pre-fix captures are explicitly
labelled `b11666f5259fb414cacf6653b81f74129e30bfcd`, from an immutable earlier
native checkpoint. They are not represented as binaries built from the newer
`6b2d401` source. The original 14 pre-fix captures retain their previous identity.

Final replay logs, binary/source hashes, independent review, and receipts live
in the local qualification directory `.local-tests/graphql-parser-diagnostics/`.
Only the native Rust release cache is used. No preserved checkpoint artifacts
are modified and no new wrapper qualification is claimed.

## Final verification

The tested and independently reviewed source is
`31a330c3f9d140f68488b58f71f90123e90989f3`. A subsequent documentation-only
commit records these results without changing the 381 hashed production/test
inputs. Final evidence is `final-inputs.json`, the `verified-*` logs and receipts,
and `review/`; earlier and superseded build/test logs are retained but are not
final evidence, including attempts made while source was still changing.

- 16 selected Rust integration tests pass, including persistent-owner state,
  original budgets/cancellation precedence, and accepted CR argument plans
- 28 GraphQL Rust unit tests pass; strict release Clippy and formatting pass
- 31 CLI regression tests pass, including 191 parser, 58 source-location,
  158 literal-diagnostic, and 182 coercion differential pairs
- The CLI regressions also retain seven pre-existing runtime differences as
  expected differences on this isolated branch, not parser parity successes
- Final source inputs, copied native binaries, and frozen oracle inventory are
  unchanged throughout the final replay

Fresh independent review found no high-confidence defect in the scoped changes.
Its first 316 probes produced 291 full-result matches and 25 retained differences;
37 supplementary probes all matched. The 25 originals remain explicit: six
true-EOF Unicode inputs crash the frozen Python oracle while native returns a
controlled syntax error; 18 probes contained unintended earlier unrelated syntax
with different diagnostics; one encountered an unrelated field-conflict
validation difference. All 18 corrected location probes, the corrected field
case, and 18 additional runtime probes match. The original differing results
remain in `review/independent-results.json`; they are not relabeled passes.

This is a bounded parser review, not a full GraphQL equivalence claim. Unrelated
broad definitions and filesystem/source-access safety reviews remain blocked
independently and were neither performed nor cleared here.


## Native37 compatibility addendum, October14

The earlier five retained controls and their original fixture bytes remain
historical evidence. The additive `graphql-braced-unicode-cases.json` and
`graphql-missing-argument-cases.json` fixtures now reproduce the pinned oracle
for all five. The first increment decodes valid braced Unicode without changing
the framework-owned recursion and unrelated syntax boundaries; the second
recognizes an argument colon immediately followed by its closing parenthesis
in the existing original-source diagnostic boundary. It does not introduce a
second executable parser.

The integrated source-bound result is
`tests/parity/native-catalog-native37-followups-checkpoint.json`: 14 parser unit
tests, six integration methods (included in its Store command), and 250 complete
CLI/oracle envelopes passed. No historical parser mismatch control remains in
that fixture set. This is bounded compatibility evidence, not a new independent
review or general GraphQL equivalence claim. Prior failures and the unrelated
blocked review lanes retain their original status.
