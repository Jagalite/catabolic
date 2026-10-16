# Typed lexical ownership, stage05

## Implemented scope and evidence boundary

The lazy iterative recognizer independently owns `MissingDigit`,
`InvalidContinuation`, and `SingleQuote` in ordinary and before-token-limit
policies. `InvalidUnicodeEscape` is newly eligible before the token limit; its
ordinary ownership still comes from the existing lexer flag. The implementation
uses typed reasons only. Lexer dispatch, diagnostic construction, source
coordinates, grammar provenance, traversal, token counting, limits, and the
public `Error(pub String)` API are unchanged.

This candidate is **uncompiled and unexecuted**. Metadata and formatter checks
do not establish parser or complete-process behavior. Fresh stage04 helper
captures and subsequent source-bound stage05 qualification remain required.

`LeadingZero` and `InvalidEscape` retain both policies. `Unterminated` retains
ordinary-only ownership; `DanglingBackslash` remains excluded. `GrammarSite::Other`,
directive grammar, EOF, and executable/required-value punctuation gain no
ownership. Existing variable-definition exceptions remain independent. Every
walk still stops at its first failure, including an earlier unowned failure or
a successfully lexed over-budget token.

## Exact composition contract

`tests/parity/graphql-parser-lexical-ownership-v1.json` contains exact preimages
of 13 historically mismatching rows, with complete frozen, archived legacy and
archived companion envelopes:

- budget-precedence-v2: cases 117, 118, 119
- diagnostic-ownership-v1: cases 22, 23
- unsupported-scalar-precedence-v1: cases 22, 23, 29
- boundary-ownership-v1: cases 19, 22, 27
- required-value-precedence-v1: cases 40, 43

The new contract binds all five historical fixture files byte-for-byte, their
case-array digests, each exact pointer/name/input/arguments/envelope, the sealed
source-only assessment, and the stage04 source manifest/map/seal. Original
fixtures are never edited. Frozen envelopes remain historical captures; archived
native envelopes are not fresh stage04 observations.

Composition changes only the expectation label for these 13 rows and preserves
the previous four ordinary promotions and budget case 120's slash promotion.
The consumer requires explicit raw or stage04-composed input, validates exact
pre- and post-composition arrays, returns a deep copy, and rejects unknown lanes,
partial or repeated composition, reordering, duplicates, and preimage drift.
Forty historically mismatch-labelled rows and six matching controls remain
baseline-classified. This is verified metadata accounting, not a runtime result.

## Staged controls

Rust tests retain all existing test identifiers and unrelated negatives. They
add typed numeric branch, quote-context, Unicode-escape and inert-neighbor
controls; independent-policy checks; original 13-row frozen diagnostics;
first-failure tails; comment positions 3,999/4,000/4,001; and successful token
4,001 stopping before later lexical failures. New branch tests assert typed
provenance and ownership rather than inventing uncaptured diagnostic messages.

The old numeric/apostrophe unowned matrix members become explicit owned inputs
and later-error controls. Earlier empty selection, object-colon, argument-name,
and directive failures still stop before every new lexical category. Four new
lexical prefixes join the existing local/HTTP source-byte, variable-object,
variable-size, and timeout admission matrix without changing its expectations.

The existing complete-process suites compose the new contract. The existing
Rust integration suite selects only the exact 13 preimages and preserves local
and HTTP preflight, cancellation precedence, query-only on/off, borrowed-owner
transactions, TEMP state, total-change counts, and recovery checks. These runtime
assertions have not been run on stage05.

## Pending capture and qualification

The accompanying source-bound candidate evidence includes a 217-case named
capture plan with exact UTF-8 inputs/hashes or lossless recipes and no generated
expectations. It covers minus/decimal/exponent branches, continuation variants,
initial/noninitial quotes, field/directive contexts, invalid and valid Unicode
escapes, first-failure ordering, earlier valid variable rewrites, Unicode and
LF/CR/CRLF coordinates, token/comment boundaries, and outer admission. Over-limit
variables require a verified supported full-process file route before capture;
their direct local/HTTP JSON control is already exact.

Capture requires explicit approval and immutable freshly qualified stage04
helpers. Record source/tool identities, exact stdin/argv, raw stdout/stderr/exit,
parsed envelopes, and disposable-catalog hashes. Reuse old frozen evidence only
after exact identity checks. Never synthesize expected envelopes from source
reasoning or candidate output. Keep known dangling-backslash/deep-recursion crash
inputs excluded; preserve every original/failed check. Maintain at least
2,000,000,000 free bytes. No Cargo, target access, wheel installation, product or
oracle execution is part of this source-only stage.

Strict unknown-profile admission and mapping-runtime-v2 decisions are unchanged.
Unqualified WAL/restore/destructive-publication/interval effects remain
fail-closed. No real catalogs, schema review, publication, commit, or push occurs.
