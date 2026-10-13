# Native GraphQL grouped execution and runtime completion

This bounded slice replaces the framework's dynamic field-completion layer with
native grouped completion over the existing Rust catalog resolver owner. Python
3.14.6 at frozen commit `453fca983222c6665a48775eef67c489c96b527e` is only the
independent test oracle. There is no runtime fallback, wrapper/wheel expansion,
new service dependency, or default/release change.

## Closed contracts

The seven original witnesses in
`tests/parity/graphql-review-remaining-gaps.json` remain unchanged as historical
failure evidence. Their names are:

- `null-enum-resolver`
- `merged-field-records`
- `merged-field-null-argument`
- `merged-field-null-directive`
- `introspection-null-argument`
- `introspection-null-directive`
- `introspection-list-null-directive`

The new `graphql-runtime-completion.json` freezes 59 complete reference responses,
including these seven and additional directives, aliases, named/inline fragments,
absent objects/lists, skipped branches, merged child selections, argument-error
positions, ordinary/introspection ordering and nullable/non-null completion.
Rust direct and persistent execution, the native legacy CLI, and the standalone
companion CLI must match those responses, including records, errors and paths.
The CLI test also compares exit status and stderr and checks catalog bytes.

## Native ownership and ordering

`graphql_runtime_input.rs` retains original source positions and runtime-null
argument plans. Its field collector evaluates `skip` before `include`, observes
named-fragment visitation, and groups response keys using an encounter-order
vector with a keyed index. The first contributing field supplies arguments; all
contributing fields supply combined child selections and resolver-error locations.
A field group invokes its resolver once, with one set of budget and record effects.

`graphql.rs` still runs the original strict framework validation, depth and
complexity checks. Only precisely identified runtime-null errors are deferred.
After validation, native completion obtains argument values before resolver and
budget middleware, then calls the existing `Context::authorized_resolve`. The
same borrowed Store, scoped TEMP views, authority/cursor owners, transaction and
query-only cleanup remain in place. Cancellation is polled outside the resolver
mutex and during list completion. No Python signal handler is invoked under that
mutex.

Introspection now resolves through its native metadata owner at the same
source-ordered execution boundary as catalog fields. It is no longer overwritten
by a later projection pass. Its dictionary-view versus list record accounting is
preserved. Collection errors happen only when their containing object is actually
completed. Null objects and empty lists do not evaluate descendants. Non-null
failures bubble immediately to the correct nullable boundary, with numeric list
indices, and stop work below that boundary. Completion handles nullable list
items generically; a synthetic Rust test covers them. Schema construction
explicitly refuses interfaces/unions rather than guessing runtime object types.

Scalar/enum leaves are checked by native completion. JSON remains opaque and
BigInt remains a string. The final transport conversion transfers JSON variants
directly, sharing the underlying arbitrary-precision Number type without a serde
deserializer round-trip. This preserves wide integers and ordinary reserved-looking
object keys. Existing native bounded value inspection is reused for output diagnostics. Two explicit raw-recorded-state Int controls (`1_000` and `١٢٣` stored as
relationship positions) remain in `graphql-runtime-scalar-remaining.json`.
Frozen Python serializes them as integers; immutable native source b11666f
returned raw strings, and current native completion rejects them. The baseline
artifact hash and all three complete outcomes are retained. No ordinary public
mutation path was demonstrated, and no catalog canonicalization is changed.
This is retained compatibility debt, not ordinary API parity evidence.

An independent review also found accepted bare-CR documents could produce
colliding framework AST positions, allowing one runtime argument plan to overwrite
another. The coordinated parser change always uses the source-mapped view for
CR-containing accepted documents, even without another lexical rewrite. Original
physical positions and invalid-document fallback are preserved. Nine exact
LF/CR/CRLF vectors cover grouping, separate-field plan identity and directive
errors. Independent review additionally corrected owner-specific record charging:
deprecated-inclusive `__Directive.args` is a list, while `__Field.args` is a
dictionary view; both now have complete frozen vectors.

A final narrow inspection found the initial whole-response generic conversion
could panic for `2**100`, or turn larger integers into serde-private number maps.
The original failing responses remain in `graphql-runtime-json-completion.json`,
alongside frozen and immutable b11666f outcomes. The baseline already had field-level
wide-integer errors or private-map corruption; the new completion must not turn
those into a query-wide panic. Six complete frozen JSON vectors now match through
both CLI surfaces and direct/persistent Rust owners, including both signs of
`2**100`, nested 1000-digit integers and a reserved-looking ordinary object. Thirteen
independent review vectors additionally cover 128-bit boundaries, 4096-digit
integers, arbitrary JSON keys, finite floats and output-budget rejection. No
workspace serde features or catalog JSON canonicalization changed. The same-owner
regression also checks cancellation, reuse and large opaque JSON budget rejection.

## Conservative overlap guard and remaining differences

The expanded controls found that framework validation could permit incompatible
nested aliases across repeated parent fields, and differing arguments across a
fragment boundary. Selecting one branch would make merged execution ambiguous.
A generic compatibility guard now runs in document preflight, before context
construction or resolver effects, and rejects gathered same-response-key fields
with different names or arguments. Static `skip`/`include` conditions cannot make
such conflicting fields valid. The original document-byte, token, expanded-field
and depth bounds apply before this traversal.

This guard does **not** implement the complete GraphQL overlapping-fields
validation rule or its diagnostic ordering and nested reason chains. Five
separate controls retain full frozen and current guarded responses. Complete
cross-fragment/nested/argument conflict diagnostics remain a named implementation
gap. Earlier validation/parser residuals are not converted to parity evidence.
The parallel document-validation slice's own residual baselines remain historical
and must be checked again on the composed source.

The shared `CatalogQuery::page` fix treats explicit null sort as an invalid member
of the available sort set. Omitted/default/valid text behavior is unchanged.
Non-null numeric, boolean and object sorts retain their prior native `sort must
be text` rejection. `graphql-runtime-sort-controls.json` separately records eight
frozen direct-query results and the current native outcomes; three non-text
controls remain explicit diagnostic differences.

## Verification and limits

- `cargo test -p catabolic-store --test graphql_runtime_completion` covers the
  frozen direct/persistent matrix, pre-execution overlap rejection, shared sort
  behavior, records/fields/output budgets and cancellation/owner reuse
- `tests.test_rust_graphql_runtime_completion` compares complete responses on
  isolated populated catalog copies through both CLI surfaces, pinned reference
  identity, artifact destinations and unchanged database bytes
- `tests.test_rust_graphql_runtime_inputs` replays the existing runtime-null and
  variables-presence matrix, while treating the seven retained failures as
  historical evidence rather than expected current failures
- Existing full introspection, scoped-authority, cursor, empty-completion, parser,
  source-location, input-coercion and transaction regressions remain required
- A fresh independent narrow review cleared the final production snapshot after
  rechecking the accounting and source-position findings: 13 general probes, nine newline probes, six ordinary
  scalar/null probes and both introspection-accounting controls on both CLIs.
  Fourteen scoped/owner/cancellation unit checks passed on the preceding unit
  binary; the final full native test run also passed. Reopened review of the
  final JSON conversion passed 13 complete frozen/both-CLI probes, output-budget
  rejection and the final eight-test library suite
- Strict Clippy covers all targets in catabolic-store, catabolic-runtime,
  catabolic-http and catabolic-cli; formatting and Python Ruff checks cover the
  changed source/tests

The first local all-store-target attempt reached the storage-fault target without
its retained migration fixture path and failed six tests for missing files. That
failed log is retained. Restoring the known read-only fixture link permits a fresh
run; it does not turn the first attempt into a pass.

This is a native runtime slice, not full GraphQL, HTTP/M6, recovery, publication,
platform, performance, or Python-retirement qualification. Existing restrictions
and denied review lanes remain unchanged.

## Final local results

- Store all-target tests: **271 passed**, with three existing explicit ignores
- HTTP crate all-target tests: **2 passed**; this is not full HTTP qualification
- Final frozen/native regression invocation: **34 test methods passed**
- Final strict-kind CLI matrix: **59 passing cases**, five guarded overlap
  controls, six exact opaque-JSON cases and two explicit scalar residuals; both
  standalone CLI surfaces
- Existing runtime-null matrix: **82 vectors**; source-location replay: **58**;
  literal diagnostics: **158**; parser replay: **60**, with all **14** existing
  parser diagnostic differences still retained separately
- Strict all-target Clippy for store/runtime/http/cli, Rust formatting, Python
  Ruff checks and diff whitespace checks passed

## Reviewed artifact identities

The independent review verified the final six implementation-source hashes and
these non-release development artifacts (no immutable candidate was rebuilt):

- Native legacy CLI SHA256:
  `dabb32c587130db3dd0c99bdd138c4f30da923886824c2d8319547b7c9668c6b`
- Native companion CLI SHA256:
  `6447a1d9115e5c31a65d129c7e52fe7dc58d68fe57aa3b49cb0e036cc7066243`

Local exact logs/results are retained under
`.local-tests/graphql-runtime-completion/`, with independent probes under
`.local-tests/graphql-runtime-completion-review/`. The receipt identifies final
source/artifact hashes and separate passed, historical-failed and residual lanes.
These artifacts identify this worker snapshot; composed checkpoint artifacts
require their own rebuild, source freeze and replay.
