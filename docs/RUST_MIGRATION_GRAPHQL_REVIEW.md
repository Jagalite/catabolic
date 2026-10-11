# Independent GraphQL input review

This is a scoped development checkpoint for GraphQL input coercion, diagnostic
rendering, and raw request intake. It does not qualify the whole GraphQL parser,
executor, Python inventory, HTTP API, a release wheel, or other migration owners.
The frozen oracle is commit `453fca983222c6665a48775eef67c489c96b527e`, Python
3.14.6, graphql-core 3.2.12. Every catalog and HTTP credential used was disposable;
HTTP tests used numeric loopback.

## Corrections established by independent examples

- Literal diagnostics retain numeric spelling (`-0`, exponent case and signs),
  GraphQL string/control escapes, and block-string node kind and formatting.
  The existing lexical location index supplies literal spelling; no parser or
  canonical JSON encoder was replaced
- A nullable variable explicitly set to null can be legal in the document but
  invalid at an executed non-null argument. These failures now occur at the
  argument boundary, with frozen message, argument location, alias/list path,
  sibling behavior, and nullable ancestor. Schema defaults apply to absent
  variables; explicit null never silently selects the default
- Directive null failures occur at selection collection. Root collection errors
  execute no resolvers; nested collection errors occur only when an actual object
  or non-empty object list is completed. Skipped branches, absent objects and
  empty lists do not manufacture errors. Skip is evaluated before include
- Successful operations with every root selection skipped now return an empty
  data object and ordinary zero-record extensions. Error/abort/deadline paths are
  excluded from this normalization; nested empty-object/list completion remains
  an explicit residual
- Strict framework static validation remains active. Only runtime-null errors at
  identified argument positions are deferred; other validation errors and the
  framework's depth/complexity limits remain. The native timeout, cancellation,
  output, record and field limits remain in force. Rejected arguments do not count
  as resolver calls. No AST eligibility check executes business queries
- Legacy CLI omission and JSON-null variables now mean an empty variables object,
  matching its frozen adapter. Other raw/HTTP intake contracts are not normalized
  together. The package-root typed facade now substitutes `{}` only for `None`,
  rather than erasing all falsy values. See the [separate intake review](RUST_MIGRATION_GRAPHQL_INTAKE_REVIEW.md) for the
  verified per-surface behavior and exception-type limitations

The selection check is not a second query executor. It does not perform query
lookups, materialize source data, calculate response fields, or implement field
merging. Current composite output list items are all non-null; an explicit schema
unit guard requires extending completion behavior before adding nullable object
list items.

## Verified limits remain open

The original four static/parser vectors remain byte-identical in
[`graphql-validation-remaining-gaps.json`](../tests/parity/graphql-validation-remaining-gaps.json).
They cover alias conflicts, duplicate input fields/defaults and discarded variable
uses, and whitespace in wrapped variable types.

The additional thirteen representative vectors in
[`graphql-review-remaining-gaps.json`](../tests/parity/graphql-review-remaining-gaps.json)
retain complete frozen, pre-fix native, and current native responses:

- Escaped surrogate-pair string literals and variables in constant directives
- First-column multiline/bare-CR location differences, including the frozen
  source-location splitlines behavior
- Null enum resolver wording
- Present nested objects/list items whose selections are all skipped still
  complete as null instead of an empty object
- Merge-compatible repeated fields already execute more than once natively,
  giving a different record count even without input errors
- Runtime-null errors involving repeated merge-compatible fields retain the
  pre-fix strict-validation response. Deferring them would expose incomplete
  merging and introduce duplicate errors or resolver work
- Framework-owned introspection argument/descendant-directive null failures also
  retain their pre-fix strict errors. They are not deferred into the separate
  introspection resolver and cannot turn into misleading success

The guarded vectors remain unsuccessful, with `data: null`, no resolver-result
extensions, and complete responses identical to the pre-fix immutable native
binary. They are explicit contract differences, not passing parity cases.

## Reproducible checks

Paired response checks compare complete parsed JSON envelopes, exit codes and
stderr. Error-array order, wording, locations, paths, values and record counts
are exact; response-object property ordering is not treated as a JSON semantic
difference. Raw input-object diagnostic ordering is checked separately.

Production source and artifact hashes and final commands/results are recorded in
`.local-tests/graphql-review/evidence.json`. Historical owner evidence in
`.local-tests/graphql-coercion-final/` remains the pre-review baseline.

- 158 complete new literal diagnostic pairs and 158 durable Rust expected results
- 82 runtime-null/empty-root pairs covering aliases, sibling fields, directives, fragments,
  null bubbling, object lists, skipped/absent/empty branches and non-null roots
- 15 legacy omission/null/object/array-presence pairs
- 13 explicit remaining-gap replays, asserted as differences rather than passes
- Original 182 coercion pairs, 14 GraphQL methods, and eight inbox-review methods,
  including both SIGINT channels, are replayed separately
- Six independent Rust guards cover ordered/duplicate envelopes, sentinel-shaped
  objects, 400-digit positive/negative IDs through defaults/fragments, canonical
  encoding, and same-transaction cancellation before/at execution
- 20 focused Rust units, 28 integration methods, Store all-target Clippy,
  workspace all-target checking, generator checks, and scoped formatting/lint
- A separate 246-case adversarial discovery matrix improved from 194 to 229 exact
  pairs; its 17 remaining differences belong to the explicitly recorded residual
  categories. This matrix is not represented as fully passing
- Raw intake is independently exercised across companion CLI, direct PyO3,
  persistent raw PyO3, persistent public Python, package-root Python, and actual
  HTTP POST: 14 original vectors, 169 checks, plus 11 cross-surface all-skipped
  root checks. A 49-case presence matrix records the explicitly different
  transport contracts; typed-facade presence includes seven paired
  acceptance/rejection cases

Example focused commands, using the pinned reference environment and built native
binaries:

```sh
python -m unittest tests.test_rust_graphql_runtime_inputs tests.test_rust_graphql_literal_diagnostics -v
python -m unittest tests.test_rust_graphql_coercion tests.test_native_graphql_typed_presence_review -v
cargo test -p catabolic-store --locked --offline --lib graphql
cargo test -p catabolic-store --locked --offline --lib validation::literal_diagnostic_render_tests
cargo test -p catabolic-store --locked --offline --test graphql_input_independent --test graphql_literal_diagnostics --test query_document_validation --test query_scalar_coercion --test query_facade_review --test work_inbox_review
cargo clippy -p catabolic-store --all-targets --locked --offline -- -D warnings
cargo check --workspace --all-targets --locked --offline
```

No Cargo dependency or serde feature changes, global `preserve_order`, canonical
identity changes, runtime Python business logic, schema changes, release, or
publication are introduced by these corrections. The development extension
checks do not replace final exact paired-wheel/platform qualification. The
Python default and migration exit gates are unchanged.

## Fresh review of the runtime correction

A separate reviewer tested the frozen correction without changing its tracked
files. The additional matrix contains 313 cases: 307 exact frozen pairs and six
full-response differences also reproduced on the immutable pre-correction native
binary. Five are the existing alias-conflict/repeated-field residuals; one deeply
nested query reaches native parser recursion-limit wording instead of frozen
validation depth-limit wording. Both reject with null data and no resolver-result
extensions. No new correction regression was found in this scope.

The fresh reviewer also passed 28 Rust integration tests, 20 unit tests and three
typed-facade tests, checked strict validation, actual resolver entry boundaries,
cancellation/transaction ownership, and verified the production freeze hashes.
Exact commands, pairs and identities are retained in
`.local-tests/graphql-runtime-independent/receipt.json`. This evidence is separate
from the author's 246-case discovery matrix and does not close the recorded
parser/executor or final paired-wheel/platform gates.
