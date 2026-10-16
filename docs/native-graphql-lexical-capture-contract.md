# Captured lexical ownership composition v1

This is a test/fixture-only composition on sealed stage05 source map
`d73e0e8fa13698000cab48196f2598d01d7750259852a70276c692cf2b816720`.
No production ownership rule is changed. All pre-existing fixture bytes,
including the thirteen lexical promotions and the earlier four-plus-slash
promotions, remain intact. Stage05 compilation and execution are still pending.

## Observed results and comparison semantics

The frozen/stage04 capture seal is
`c2f5193760f889a37bcb2e2e4a52a2c15bc1ca768617dff0a535a8ff8cb08c96`.
Its 217 named cases represent 214 distinct full inputs and 198 documents.
The capture ran 635 CLI processes, 198 frozen parses and two setup processes.
Every raw stdin/argv/stdout/stderr/exit receipt remains in that unchanged capture.

- Legacy/reference: 213 supported complete processes; 69 matches, 144 diagnostic
  mismatches. The 69 matches are also literal stdout/stderr/exit equality.
- GraphQL-only subset: 197 named cases on each native surface; 53 matches and 144
  diagnostic mismatches. Legacy comparison uses the full parsed stdout envelope,
  exact exit status and decoded unmodified stderr. Companion comparison uses the
  existing explicit adapter `(exit=0, stdout=reference.stdout.data, stderr="")`.
  It is parsed GraphQL-envelope equality, not raw process equality: none of the
  209 supported companion processes is byte-for-byte equal to reference.
- Other CLI admission: twelve timeout/variables controls match reference/legacy.
  Companion retains its distinct captured exit-2, empty-stdout, stderr wrapper.
  Four oversized-document cases match reference/legacy through `graphql --file`;
  they test the bounded CLI file reader, not direct GraphQL document admission.
- Frozen parsing: 181 rejected and 17 accepted distinct documents. All 180
  applicable named reference syntax errors match the exact frozen `formatted`
  message/location object. The 17 parse-accepted named controls proceed to CLI
  validation/execution. Twelve CLI admission cases and four file-reader cases
  are separate surfaces, not parse/CLI equivalence assertions.
- No observed crash, unexpected exit status, frozen/reference diagnostic drift
  or disposable-database mutation occurred. The 3999/4000/4001-labelled comment
  controls all produced native stage04 token-budget diagnostics (36 cases),
  while frozen parsing reported the earlier lexical failure. These are explicit
  single-advance precedence controls, not assertions that their labels alone
  establish the native front-end token count.

### GraphQL category counts (same on both native surfaces)

| Capture group | Existing matches | Diagnostic mismatches |
|---|---:|---:|
| MissingDigit branches | 0 | 24 |
| InvalidContinuation branches | 0 | 3 |
| SingleQuote contexts/inert neighbors | 4 | 10 |
| InvalidUnicodeEscape branches | 13 | 0 |
| Unicode valid/inert neighbors | 7 | 0 |
| Numeric neighbors | 8 | 0 |
| First failure before later owned error | 3 | 13 |
| Unicode/newline original coordinates | 3 | 9 |
| Same-line Unicode coordinates | 1 | 3 |
| Original coordinates after Unicode rewrite | 1 | 3 |
| Earlier valid variable/default/directive rewrite | 1 | 3 |
| Single-advance lexical/count precedence | 0 | 36 |
| Successful token limit blocks later failure | 4 | 0 |
| Earlier unowned grammar must stop | 8 | 40 |
| Total | 53 | 144 |

## Explicit pending expectation composition

The new versioned fixture stores 197 complete captured GraphQL rows with raw
receipt references and exact frozen parse observations. Its raw state labels
all rows `baseline`. The fail-closed composer changes exactly 104 labels to
`oracle`: 43 MissingDigit, 22 InvalidContinuation, 29 SingleQuote and ten
InvalidUnicodeEscape before-limit cases. Every input and observed envelope is
unchanged. The other 93 rows stay exact stage04 baselines: 53 already matching
controls plus 40 earlier-grammar diagnostic negatives. No target native stage05
outcome has been fabricated or claimed as observed; these are explicit proposed
expectations copied from captured reference/frozen evidence for later testing.

The companion expectation uses its existing observed exit-0 GraphQL wrapper;
its twelve exit-2 admission wrappers are preserved separately. JSON parsing
ignores serialization whitespace and object-key order, but does not normalize
messages, coordinates, values, stderr or exit codes. Raw bytes remain separately
bound. Runtime replay creates new disposable database/query-file paths in argv;
those transport paths are not asserted byte-identical to the historical argv.
No stdout/stderr path normalization is performed.

The sixteen supported admission controls have 44 captured process envelopes in
a separate consumer. The four oversized-variables named cases have no CLI
expectation on any lane. Companion has no outcome for the four oversized query
files. All sixteen unsupported lanes remain excluded before any argv launch.

## Consumers and unexecuted Rust controls

- `CapturedLexicalRuntimeTest.test_captured_graphql_complete_process_envelopes`
  reuses the existing scoped parser replay for 197 rows / 591 case processes.
- `test_captured_cli_admission_complete_process_envelopes` replays only the 44
  supported admission envelopes, keeping `--file` distinct from argv transport.
- Five metadata tests validate exact fixture/case/receipt identities, the 104
  exclusive promotions, all 40 retained mismatches, exclusions, mutation
  rejection and 4974 bound raw receipt files. They never execute product code.
- Two Rust unit tests stage exact frozen message/location and typed-reason checks
  for 104 targets, and first-failure stopping for all 48 earlier-grammar controls.
- One Rust integration test stages 180 syntax rows through local/HTTP preflight
  and query-only off/on borrowed-owner cancellation/nonmutation/recovery checks.
  It excludes every CLI admission/file-reader/unsupported case. It is future
  direct native evidence, not a reinterpretation of captured CLI file results.

None of these new Rust or complete-process consumers has run against stage05.
The sibling field-value `!`/EOF candidate is not merged. Against its described
narrow policy, the 40 retained negatives have zero overlap: their first failures
are `GrammarSite::Other` name/colon expectations, including `{!}` expecting a
field name, rather than a field `RequiredValue` `!` or EOF. The separate overlap
report binds every exact capture ID and document digest; typed classifications
are source-reviewed, with their Rust assertions still unexecuted.
