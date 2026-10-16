# Caller-specific grammar provenance, stage09

This is a source-only sparse overlay on sealed composed-stage07 map
`c07dabe4716ba7efe512319d72cbca07a01ac5e11c3d910b2aa79adbb45ef572`,
seal `a4fd8d8bbadd03c73f9910cacd43a307d09d15321742c3d79e1f29379c82d0bc`.
It is not a compilation, runtime pass, compatibility certification, waiver,
commit, adoption or default flip. All native consumers remain unexecuted.

## Exact private policy

The recognizer records caller-specific tags at the point a required name or
colon fails. Selection field versus alias names and first versus subsequent
argument names remain distinct. Object member and variable-reference names
carry ValueContext; variable references also carry ValuePosition. Variable-definition
directives receive a private VariableDefinitionDirective context.
Generic Directive remains reserved for the other existing directive parents.

Both independent policies own exactly these tuples:

- SelectionName(Field), Expected::Name, found `}` or `!`
- ArgumentName(Field, first=true), Expected::Name, found `:`
- ArgumentName(FieldDirective or VariableDefinitionDirective, first=true),
  Expected::Name, found `)`
- DirectiveArgumentColon(FieldDirective or VariableDefinitionDirective),
  Expected::Literal(":"), Found::Name
- ObjectMemberName(Argument(Field)), Expected::Name, found `:`
- ObjectMemberColon(Argument(Field)), Expected::Literal(":"), Found::Number

Ordinary ownership alone also claims VariableReferenceName(Argument(Field),
Root), Expected::Name, found `)`. No exact captured admission-lane witness exists
for that tuple, so before-limit remains false even when ordinary is true.

Other, alias-name failures, subsequent argument-name failures, generic Directive,
non-field object/reference contexts and all unlisted found tokens receive no new
ownership. Existing variable/string and immediate-value-colon exceptions remain
independent and unchanged. Legacy variable ownership is never used as an admission
shortcut. Broadening these tuples requires a separate evidence/scope decision.
This intentionally leaves other malformed-name variants with their prior owner.

## Compact independent-lane evidence map

All entries below refer to complete immutable historical records. The names are
abbreviated only in this narrative; the new contract stores exact fixture name,
contract, index, pointer, name, input/args/whole-case hashes, complete reference,
legacy and companion envelope hashes, frozen-parse hash and typed classification.

| Tuple | Ordinary witness | Before-limit witness |
|---|---|---|
| Selection Field / `}` | boundary /17; lexical capture /73 (matching) | budget /116 |
| Selection Field / `!` | lexical capture /75 (matching) | budget /121 |
| First Field argument / `:` | boundary /20 | budget /122 |
| Field object member name / `:` | boundary /38 | required-value /44 |
| Field object member colon / Number | boundary /21,/39 | required-value /42,/45 |
| First FieldDirective argument / `)` | boundary /10 | budget /146 |
| FieldDirective colon / Name | boundary /11 | budget /147 |
| First VariableDefinitionDirective argument / `)` | ownership /36 (legacy-owned, matching) | budget /148 |
| VariableDefinitionDirective colon / Name | ownership /37 (legacy-owned, matching) | budget /149 |
| Root Field variable-reference name / `)` | boundary /58 | None; remains unowned |

The 27 historical Other rows comprise 24 mismatches and three matching controls.
The 48 lexical-capture controls comprise 40 mismatches and eight matches. Every
one remains present, with its original bytes and complete observations. Exactly
24 historical and 40 lexical expectation labels are promoted; all 11 matching
control labels remain baseline. Their semantic envelopes remain identical even
where the source-selected owner changes.

The captured lane retains its original 104 lexical promotions and adds 40 grammar
promotions, for 144 pending oracle labels and 53 matching baselines. Each of four
12-row lexical groups has six short and six over-budget documents. The new grammar
promotions are 16 ordinary plus 24 before-limit rows; the eight other short
selection controls already match. None is relabeled as a lexical failure.
Historical inventories retain three mismatch-labelled residuals and six matches;
the three residuals belong to the separate stage08 executable-punctuation work.
These counts are metadata expectations, not native outcomes.

## Evidence and consumers

Before composing expectations, the preparation verifies complete assessment and
fixture preimages, including reference/legacy/companion envelopes. It separately
rechecks the original capture seal
`c2f5193760f889a37bcb2e2e4a52a2c15bc1ca768617dff0a535a8ff8cb08c96`,
transport manifest, every relevant exact input/argv identity, and all 4,974 sealed
raw receipt files through the inherited metadata verifier. No old fixture or
capture bytes are rewritten, and no native candidate output is invented.

The new fail-closed composer accepts only exact preceding stage07 historical
inventories or the independent exact 104-promotion lexical inventory. It changes
only designated labels and rejects unknown, partial, wrong-order, repeated,
mutated-input, envelope or policy compositions. The metadata suite includes
51 guarded tests per pinned Python. Product imports and process launches are
blocked inside that suite. See receipts for the actual outcomes.

The staged native controls cover all 75 captured rows' exact typed first failures,
independent policy decisions, original diagnostics, explicit excluded call sites,
legacy variable ownership, inert valid neighbors, original Unicode/newline
coordinates, first-failure tails and comment/token-limit boundaries. Existing
negative matrices retain every old input and now compare the actual prefix's
first failure and policy. Still-unowned alias/subsequent/generic/non-field prefixes
have separate explicit negative matrices. Existing lexical first-failure tests
retain all 48 rows and require the earlier grammar diagnostic.

Local/HTTP/borrowed-owner consumers retain their complete loops and append only
these exact expectation selections. Query-only off/on, cancellation, transaction,
TEMP-schema, total-change and recovery controls remain in place. Direct admission
and CLI `--file` remain separate. These Rust and complete-process tests have not
been compiled or run for this candidate.

## Semantic and architecture preservation

The only production edit is boundary.rs. Location/error formatting, lexer,
string decoding and original boundary entry points remain byte-identical.
Twelve recognizer methods are byte-identical; caller methods are identical after
erasing only the added private tags and the local first-argument marker.
Neither token reads nor grammar/first-failure stopping order changes. The
original policy suffixes remain unchanged, including ordinary-only field-value
EOF. No AST, recursion, budget, public Error API, schema, profile, mapping-runtime-v2,
security or external integration changes occur.

## Deliberate stage08 composition impact

Stage08 is not included. Its sealed sibling map is
`6bf998bb8a2c14003b1db5261ba13c10889fb9c1808f5a6cc4b186413d4f9485`;
seal `df5748fbee94df8314f4e70e9a3eb169167774e5af299f7184db52fc8f4a42fd`.
The shared production symbols are Failure::owned and Failure::before_limit.
Stage09 also changes GrammarSite, adds SelectionName, extends ArgumentContext,
and changes Recognizer::{name,name_at,value,arguments,variables,selection_set}.
Stage08's Found/From<Lexeme> changes are untouched here.

Both versions edit the scoped Python expectation helper and historical Rust
integration consumer. Stage09 additionally updates the separate scalar consumer
and composed-stage07 tests; these are not changed in stage08's sparse delta. Both composers
require exact stage07 preimages, so they must not simply be chained. A later
source-bound composition must retain both disjoint label deltas, bind the combined
before/after hashes, and deliberately reconcile the common production policies,
consumer imports and assertions. Stage09's lexical 40-row extension is separate
from stage08's three historical rows. The separate UnsupportedScalarPrecedenceTest
consumer must receive both scalar /21 (stage09) and scalar /25 (stage08) labels
in that deliberate composition; stage08's delta does not update this consumer. Existing stage08 bang/spread assertions
must be retained when reconciling the older bang-negative assertions here.

## Package and qualification

The overlay contains only changed/added source files. The virtual manifest
inherits all other files from the sealed base. The deterministic unified patch
is verified forward and backward entirely in memory against exact preimages.
No full source copy, hardlinks, target/cache access, Cargo, oracle, product process,
new capture, real catalog, push, Motion or cloud task was used. Source allocations
remain below a few MB. The 2,000,000,000-byte floor is checked before validation.

Future qualification must separately admit an exact materialized source map,
exclusive target ownership, current tool/source/input fences and adequate free
space. Compile/run the staged typed and direct local/HTTP/owner tests, then the
exact complete-process historical/captured lanes, before considering adoption.
The source-only checks cannot substitute for those results. The two preparatory
assertion corrections and initial/final check receipts remain recorded; preparers
are historical authoring steps, not an alternate qualification route.
