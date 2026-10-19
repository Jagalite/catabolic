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
  excluded from this normalization. Nested empty-object/list completion is now
  covered by the narrow correction described below
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

The three original static-validation vectors are closed by the bounded
[native document-validation slice](RUST_MIGRATION_GRAPHQL_DOCUMENT_VALIDATION.md).
Their exact failing inputs and historical responses remain in
[`graphql-validation-remaining-gaps.json`](../tests/parity/graphql-validation-remaining-gaps.json).
This qualifies direct-sibling differing-field-name conflicts and duplicate input
fields/defaults with preserved variable uses. Broader cross-fragment, nested
selection merging and differing-argument overlap diagnostics remain unqualified.
The wrapped-type parser witness moved to the passing fixture below.

The additional seven remaining representative vectors in
[`graphql-review-remaining-gaps.json`](../tests/parity/graphql-review-remaining-gaps.json)
retain complete frozen, pre-fix native, and current native responses:

- Null enum resolver wording
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
- The original review replayed 13 explicit remaining gaps as differences, not
  passes. The completion correction below moves two witnesses to exact passing
  fixtures; the other 11 remain explicit differences
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

## Present objects with every selected field skipped

The two previously documented `nested-empty-object-completion` and
`nested-empty-list-completion` witnesses now match the frozen response. Their
complete frozen and pre-correction native responses are retained in
[`graphql-empty-completion.json`](../tests/parity/graphql-empty-completion.json).
Only these two witnesses have been removed from the remaining-gap fixture.

The correction lives in the shared Rust GraphQL output/completion owner. It
records the response path of actual non-null resolver objects and object-list
items after authorization and runtime-input checks. When the framework completes
such a container as null without a null-bubble boundary, completion returns an
empty object. A missing resolver value, null scalar, resolver failure, or bubbled
non-null error is never promoted to an object. It neither recollects selections
nor performs additional business queries; aliases and list indices use the
executor's actual response paths. Field/record counts, limits, transaction
restoration, schema bytes, canonical encoding and authority checks are unchanged.
The existing root-all-skipped correction remains separate.

The durable focused fixture includes 41 complete local responses and three
scoped HTTP responses. Cases cover both named witnesses, literal/variable
skip/include, skip-before-null-include precedence, inline/named/nested fragments,
operation selection, aliases, list items, page objects, non-null objects, absent
objects, empty lists, nullable and JSON scalars, skipped parents, static errors,
runtime argument/directive errors and non-null bubbling. The same local expected
responses are checked through legacy CLI, companion CLI, direct raw PyO3,
package-root Python, persistent raw PyO3 and persistent public Python. Actual
numeric-loopback HTTP is paired with frozen Python for 36 compatible cases plus
three scoped-access controls, including hidden objects and a forbidden non-null
parent. Tests use disposable catalogs and locally issued fixture credentials.

Reproduction uses the pinned frozen commit and oracle from the top of this page,
explicit `CATABOLIC_LEGACY_BINARY`, `CATABOLIC_NATIVE_BINARY`,
`CATABOLIC_HTTP_BINARY`, and `CATABOLIC_COMPLETION_EXTENSION_ROOT` (containing a
copied `catabolic_native/_native.abi3.so` development package):

```sh
python -m unittest tests.test_rust_graphql_empty_completion -v
cargo test -p catabolic-store --locked --offline --test graphql_empty_completion
cargo test -p catabolic-store --locked --offline --lib graphql
```

The local receipt and exact artifact/source identities are retained separately in
`.local-tests/graphql-skipped-objects/receipt.json`; pre-correction evidence is
not overwritten. This is focused completion evidence, not full parser/executor,
installed paired-wheel, platform, safety, or release qualification. That completion
correction did not cover source-location differences; the later narrow correction
below covers their two recorded witnesses. No broader owner review is implied.

The final focused run passed the 41-case local matrix on Python 3.14.6 and 3.11,
all 39 HTTP pairs on Python 3.14.6, 21 selected Rust units, 22 selected Rust
integration tests, and 31 prior GraphQL/typed-facade methods (including the 158
literal and 182 coercion pairs, 82 runtime-input pairs, and remaining-gap guards).
Scoped Clippy, formatting, lint and documentation-source checks also passed.
Python 3.11 paired HTTP is unqualified because its pinned reference environment
has neither `httpx` nor `fastapi`; no dependencies were installed or copied.

Fresh independent checks added 55 cases across four embedding surfaces and ten
scoped HTTP pairs, plus record-limit and both CLI SIGINT controls. The review
also retained one unchanged budget difference: native charges materialized
metadata before selection completion, so an item with more than 8 MiB of unused
metadata can reach the native output limit even when frozen Python returns an
empty object. The immutable pre-correction CLI produced the identical native
error envelope for that case. This correction deliberately preserves that
existing charging behavior; it does not claim budget parity for every input.

## Static validation source locations

The two recorded `first-column-multiline-location` and `bare-cr-location`
witnesses now match frozen graphql-core. Their complete frozen and original
native responses have moved to
[`graphql-source-locations.json`](../tests/parity/graphql-source-locations.json).
At that checkpoint only these two entries were removed from the remaining-gap
fixture. The later parser correction below closes two of its other nine entries.

The shared Rust validator keeps canonical lexical positions for token lookup,
definition order and fragment identity, and separate diagnostic coordinates for
output. Frozen `Source.get_location` measures the Unicode-scalar prefix with
Python `splitlines`, including its additional Unicode separators. A prefix
ending in a line separator has no final empty line, so a token at first column
reports the preceding line's end. Output conversion never becomes an internal
ordering or identity key.

The framework parser resets its column without advancing its line at bare CR,
so those positions cannot uniquely identify source tokens. Only for documents
containing bare CR, validation uses a second parsed view with bare CR replaced
by LF and CRLF left intact. Original parse admission happens first; the original
request and execution AST remain unchanged. Ignored whitespace, comments and
block-string values have equivalent semantics in this view; normal strings do
not admit raw CR. Source spelling and output coordinates still come from the
unmodified document. No schema, definition/digest, canonical JSON, resolver,
authorization or budget logic was changed.

The focused fixture has 58 complete-response cases covering both witnesses,
LF/CR/CRLF, first/second columns and blank lines, tabs, BOM, Unicode scalar
offsets, Unicode separators in strings/comments, block strings, nested literals,
defaults, directives, repeated same-column tokens, source-ordered multiple
definitions and the 20-error cap. Rust tests check the full matrix through one
persistent owner and retain its transaction, TEMP marker and query-only setting,
and check preflight in both local and HTTP schema modes before context creation.
Frozen/native legacy and companion CLI pairs pass on Python 3.14.6 and 3.11.
Six additional controls separately preserve the exact original native syntax
and token-budget responses, including their remaining differences from frozen
Python. Syntax-error, token-budget and runtime-location parity are not claimed.

Reproduction uses the pinned reference and explicit `CATABOLIC_LEGACY_BINARY`
and `CATABOLIC_NATIVE_BINARY`:

```sh
python -m unittest tests.test_rust_graphql_source_locations -v
cargo test -p catabolic-store --locked --offline --test graphql_source_locations
cargo test -p catabolic-store --locked --offline --lib literal_diagnostic_render_tests
```

Exact response pairs, commands and source/artifact hashes are retained separately
in `.local-tests/graphql-source-locations/receipt.json`. The final scoped checks
passed 25 Rust units, 11 Rust integrations, 28 prior Python GraphQL methods
(including 158 literal and 182 coercion pairs), Clippy, formatting, lint and
documentation-source checks. Fresh independent review found no blocking issues;
it repeated both interpreter matrices and all nine remaining gaps, then passed
114 additional cases on both CLI surfaces, including every extra Unicode
splitlines separator and repeated CR position collisions. These are development
artifacts, not an installed-wheel or release qualification. Python 3.11 HTTP
remains unqualified: its pinned reference lacks optional `httpx` and `fastapi`,
and no packages were installed or copied for this correction.


## Bounded native parser correction

The `escaped-surrogate-pair`, `variable-in-constant-directive`, and
`whitespace-in-wrapped-variable-type` witnesses now match the frozen result.
Their original documents, complete expected responses, and original failing
native responses are retained in
[`graphql-parser-cases.json`](../tests/parity/graphql-parser-cases.json).
Only those three entries moved out of the two remaining-gap files; their other
ten vectors are unchanged.

A narrow Rust executable-parser adapter decodes valid UTF-16 surrogate escape
pairs in ordinary strings and removes GraphQL-ignored text between variable-type
tokens. Escaped backslashes, block strings, non-GraphQL whitespace, invalid pairs,
and malformed wrapped types are not rewritten. Every positioned AST node is
mapped back to the original source before static validation and execution.
Bare CR and CRLF are canonicalized only inside an edited parser view, avoiding
framework position collisions. A constant-variable candidate also uses this
view when CR is present so earlier/later syntax errors use comparable ordering
coordinates. The original source still owns token/byte budgets, diagnostic
spelling, request identity, and input-location lookup.
Both GraphQL execution and selection contract admission use the same adapter.
No canonical encoder, resolver, ownership, authorization, or cancellation policy
was changed.

Variables in constant variable-definition directives immediately following the
type, without a preceding default, produce the frozen syntax error before
document validation, including nested lists/objects and later malformed syntax.
An earlier or same-position syntax failure keeps precedence. Constant-value
syntax errors also precede the framework's post-parse duplicate-operation,
duplicate-fragment and anonymous/multiple-operation checks; recursion limits
remain authoritative. The adapter does not generalize all syntax diagnostics: failed transformed documents retain the
original framework rejection and source excerpt. Variable defaults containing
variables, directives following defaults, unpaired/reversed surrogate escapes,
and other malformed syntax remain explicitly different. These are tracked
separately as fourteen controls, not passing parity evidence.

The focused fixture contains 60 complete-response cases, including Unicode-seeded
catalogs that verify exact decoded IDs at U+10000, U+10FFFF and mixed pairs,
escaped-string controls, following-field/error positions, ignored type text,
nested wrappers, coercion failures, and constant-directive ordering. It also
records complete frozen preflight results in local and HTTP schema modes; the
HTTP labels denote validation only, not transport qualification. CLI replay
compares frozen Python 3.14.6 with both native CLI surfaces, complete envelopes,
exit codes and stderr, while preserving both disposable catalog hashes.
Rust library checks cover direct execution and one persistent owner with an
open transaction, TEMP marker, and query-only both off and on.

```sh
python -m unittest tests.test_rust_graphql_parser_cases -v
cargo test -p catabolic-store --locked --offline --test graphql_parser_cases
cargo test -p catabolic-store --locked --offline --lib graphql_parse
```

This is native behavior evidence only. It introduces no Python runtime path,
wrapper expansion, dependency installation, release/default-distribution change,
or new claim about duplicate fields, introspection, merged fields, production
HTTP, M4–M11, or the denied review lanes.


Fresh independent review approved this bounded correction after replaying all
60 complete-response targets and 14 declared non-parity controls on both CLI
surfaces, plus 44 additional probes (some overlap the durable fixture and are
not added to its unique-case count). Review-driven fixes cover bare-CR syntax
ordering, structural-error precedence, and same-position malformed names;
their failing review responses remain in the durable fixture. Scoped Rust
verification passed 28 unit tests and 29 integration tests, including original
byte/token budgets, cancellation, selection admission, canonical JSON guards,
and persistent ownership. Thirty prior native CLI regression methods also
passed, including 182 coercion, 158 literal-diagnostic, 58 source-location,
runtime-null, and skipped-object completion pairs. Store all-target Clippy,
formatting, and the focused
Python test's lint/format checks passed. Exact source/artifact hashes, commands,
and response reports are retained in
`.local-tests/graphql-parser-cases/receipt.json`. These development artifacts
are not release, resource/performance, or cross-platform qualification.
