# Versioned GraphQL parser boundary ownership

This bounded extension adds two independent contracts. It supersedes the old
ordinary-ownership exclusions described in
`native-graphql-parser-budget-precedence.md`; all historical fixture bytes and
captured native envelopes remain unchanged.

## Ordinary ownership

`graphql-parser-boundary-ownership-v1.json` records 61 complete-process controls
and 26 separate frozen parse-only scalar controls. Its policy owns:

- The lexer's generic `UnsupportedScalar` fallback, initial or noninitial,
  including ASCII controls and Unicode scalars
- Empty **field** argument lists, using the existing argument-start provenance
- `)`, `]`, or `}` encountered where a **field** input value is required, with
  explicit argument-root, list-element, or object-member-value provenance

Balanced empty or finished lists and objects consume their matching closer
before scheduling another required value. Object names, object colons, `$`
variable names, EOF, other punctuation, and unrelated numeric/string reasons
are not promoted by this rule. Existing variable and string ownership remains
separate. Diagnostic formatting uses original source coordinates, with no
public error-type change or message-based ownership matching.

Generic lexical ownership intentionally applies inside directives: for example,
`{profile @skip(if:%)}` receives the frozen unsupported-character diagnostic.
Directive **grammar** does not gain the field-value or empty-field-arguments
rules. Context travels through iterative value tasks, so completing a field
value cannot leak field context into the next directive, or vice versa.
The pre-existing immediate-colon/`)` directive exception and variable-definition
directive ownership remain intact.

The immutable ordinary-ownership-v1 fixture's exact `/cases/18`, `/cases/19`,
`/cases/20`, and `/cases/24` are promoted only after checking fixture provenance,
position, name, exact input and arguments, complete frozen envelope, and both
original native baseline envelopes. Python also pins full fixture and canonical
case inventories, rejecting missing, duplicate, reordered, or changed rows.
Composition changes only the selected copies' expectation to `oracle`; it never
rewrites the historical JSON or native captures.

The previous composed inventory contained 28 baseline rows: 12 from budget-v2
(after its already-versioned slash replacement), 11 from ordinary-ownership-v1,
and five from unsupported-scalar-v1. Only the four specified ordinary rows move
to oracle expectation. The other **24 baseline-classified rows** retain unchanged
inputs, envelopes and classifications. Of those, 20 have frozen-versus-legacy
envelope mismatches; four already match: `invalid-timeout-admission`,
`invalid-variables-admission`, `unowned-variable-directive-empty`, and
`unowned-variable-directive-colon`. A baseline label is not itself evidence of a
mismatch. Separate additive controls in these new contracts do not relabel any
of those 24 historical rows.

## Independent admission precedence

`graphql-parser-required-value-precedence-v1.json` records 50 complete-process
controls. It adds the same explicitly field-scoped required-value closing-token
category to the independent before-token-limit policy. Ordinary ownership alone
would leave the three malformed root/list/object long-tail cases mismatching.

The 4,000-token limit, skipped-comment accounting, 131,072-byte source limit,
and lex-before-count single-token advance are unchanged. A successfully lexed
token 4,001 stops the bounded recognizer before grammar classification. An
already-eligible lexical error during that advance keeps its established order.
The first unclaimed failure cannot be skipped to search for a later eligible
one. Existing timeout, source-size, variables-object and variable-size admission
continues to run before syntax inspection.

Controls cover the five original witnesses and their unterminated-string tails;
three required-value long-tail promotions; lexical and empty-argument long-tail
neighbors; 27 LF/CR/CRLF combinations at positions 3,999/4,000/4,001; an already
over-budget valid prefix before a later closer; and over-budget directive,
earlier-unclaimed, object-name/colon and punctuation negatives. Original Unicode
and newline coordinates, balanced values and both directive-context directions
are independently represented.

## Consumers and qualification boundary

The existing Python scoped replay composes the four checked ordinary
replacements before choosing runtime envelopes. The new runtime test class
loads each separately validated contract through that same complete-process
runner. Automatic `test_rust_*.py` discovery includes eight new source-only
fixture tests and two new runtime scenarios; no source-only test launches a
product process. Historical string, token-budget, comment, EOF and scalar
contracts keep their own validated replacements and historical counts.

The Rust scoped integration composes the same four exact preimages. It also
runs 56 new ordinary syntax controls and 50 admission controls through local and
HTTP preflight and borrowed-owner queries, with query-only off/on, pending
cancellation, an open transaction, preserved TEMP scope/marker, unchanged
`total_changes`, and owner recovery. Five syntax-accepted ordinary neighbors
remain full-process controls and are not misreported as syntax preflight cases.
Existing 148/29/31 preflight counts for the three earlier contracts are unchanged.

The 26 scalar/NUL source controls contain only actual frozen parse results.
NUL was sent to that frozen parser through stdin, not OS argv. They contain no
invented native or companion result and are not passed to the process replay.
The earlier unsupported-scalar suite retains its explicit file transport for
frozen/legacy NUL replay and its unsupported, unexecuted companion argv entry.

All added complete-process envelopes come from the installed frozen
Python 3.14.6 / graphql-core 3.2.12 reference, revision
`453fca983222c6665a48775eef67c489c96b527e`, and immutable native helpers bound to
the old stage02 source map
`994e356514f736a469c9ace6f7e0e4779c274cab4eb142a97feb907ddf8a10d4`.
The fixtures reuse the sealed 58-document assessment and 53 bounded supplemental
documents. The latter made 178 actual processes; existing exact frozen parses
were reused rather than invented or counted twice. Raw stdout/stderr/exit records
and source/artifact identities are preserved in the candidate evidence package.
All three disposable catalogs stayed byte-identical, with at least
2,000,000,000 bytes free before each supplemental process.

These are frozen expectations and source-only checks, **not execution of the
edited candidate**. Source-bound compilation, native unit/integration execution,
and full qualification remain required after handoff. The four historical
crash/deep-recursion exclusions are unchanged and were not re-executed as new
parity witnesses. No crash emulation, broader token allowance, architecture
change, schema work, or unrelated compatibility choice is part of this repair.
