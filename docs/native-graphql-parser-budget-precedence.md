# Scoped GraphQL syntax versus token-budget precedence

The policy descriptions below document the original contracts. The current
[boundary ownership extension](native-graphql-parser-boundary-ownership.md)
supersedes their ordinary lexical/empty-field-argument exclusions and separately
adds required-field-value admission. All original fixture bytes remain intact.

The original v1 correction promotes exactly two historical cases in
`graphql-token-budget-locations-v1.json`: `earlier-missing-value-over-limit`
and `earlier-invalid-character-over-limit`. The historical JSON is unchanged.
The new versioned fixture preserves its exact inputs, frozen process envelopes,
and old native envelopes. Both historical cases also occur in the immutable
string-diagnostic fixture; its runner applies the same checked replacements.

Two additional contracts keep separate inventories:

- `graphql-parser-budget-precedence-v2.json` promotes the six retained
  over-budget failures: empty field-argument list, missing argument colon,
  leading-zero number, executable-document `!`, unsupported `😀`, and invalid
  non-dangling ordinary string escape
- `graphql-parser-diagnostic-ownership-v1.json` promotes three admitted short
  documents: initial `?`, missing argument colon, and leading-zero number

Both preserve the exact old inputs, arguments, frozen complete process
envelopes, and native baseline envelopes. Existing fixture files remain
immutable. Their runners compose exact provenance/input/envelope-checked
replacements rather than rewriting the historical observations. Additive
controls record independently captured frozen and archived-baseline envelopes;
an intended `oracle` expectation is not evidence that modified Rust passed it.

A third independent contract, `graphql-unsupported-scalar-precedence-v1.json`,
replaces the character singleton policy with the frozen lexer's generic
unsupported-scalar fallback category. It adds 31 bounded controls and exactly
promotes the slash control from budget-precedence v2. The v2 fixture and its
historical native result remain unchanged. The six original admission, three
ordinary-ownership, and new scalar-extension inventories stay separate.

## Admission and bounds

Timeout, the 131072-byte source limit, variable-object validation, and variable
byte limits retain their original order. The existing allocation-free lexical
budget scan still enforces 4000 tokens including skipped comments. Only when
that scan reports an overrun does the existing iterative grammar recognizer
perform a bounded, exact lexical walk. A scoped earlier failure can replace
that overrun; otherwise the original budget result is returned unchanged.
The recursive AST parser remains after admission.

The cursor borrows lexemes from the original UTF-8 source and retains no token
list. It reads at most 4001 non-comment lexemes; skipped comments allocate no
objects and the source byte bound limits their scan. Each consumed token can
add only bounded iterative grammar state, never a Rust recursion frame. The
one-token advance matches the frozen parser: lexing happens before the combined
token-count check. Thus `?` reached immediately after skipped comments may win,
while a successfully read token 4001 stops the walk before later syntax.

The existing closing parenthesis immediately after an argument value colon and
the literal invalid-character `?` retain their precedence. The six new failure
categories are selected by typed failure provenance: grammar site, expected and
found token kinds, numeric reason, string reason, or unsupported code point.
No source-string, fixture-name, or diagnostic-message matching is used in the
product implementation. Unsupported-character admission uses typed
`UnsupportedScalar` provenance assigned only after whitespace/BOM, comments,
strings, names, numbers, and recognized punctuation/spread have been dispatched.
Printable ASCII, ASCII controls, and non-ASCII Unicode scalars share this
failure category and retain their original quoted-ASCII or `U+` formatting.
Apostrophe has distinct `SingleQuote` provenance and retains its existing
admission policy and special diagnostic. Unexpected `!` is admitted only in
an executable-document position, not at arbitrary grammar positions.

Ordinary diagnostic ownership is a separate policy. It adds initial lexical
`?`, field-argument colon, and leading-zero number failures while retaining
the existing variable/string/missing-value rules. Empty argument lists,
document-position `!`, and `😀` do not acquire ordinary ownership. An initial
lexical position permits preceding whitespace or comments, but not an earlier
successfully lexed document token.

The existing field-argument-colon ownership also applies to the bounded controls
`{items(first 1)} query($x:Int=$bad){profile}` and
`{items(first 1)} query($x:ID="\uD800"){profile}`: the frozen first failure is
`Syntax Error: Expected ':', found Int '1'.` at line 1, column 14 in both cases.
`{items(first)}` reports `Syntax Error: Expected ':', found ')'.` at line 1,
column 13. These three unit assertions use exact frozen diagnostics rather than
the superseded framework-equality expectation. The same eight-input control
group retains framework-equality assertions for `{profile % items(first:)}`,
`{items()}`, `{items(first:])}`, `{items(first:[)])}`, and
`{items(hasRendition:{kind:})}`. Those five remain explicit frozen-diagnostic
mismatches. This test reconciliation changes neither ownership policy nor any
versioned fixture or token-budget contract.

The cursor returns its first failure. An earlier unclaimed grammar or single-quote failure,
numeric exponent or continuation error, Unicode escape error, or unterminated
string cannot be skipped to find a later eligible failure. In particular,
classification never depends on which later diagnostic might otherwise win.
Successfully lexing token 4001 still stops before grammar; a claimed lexical
failure encountered during that advance can still win before counting.

## Evidence and limitations

The original 68 complete-process controls cover LF/CR/CRLF positions, Unicode comments,
3999/4000/4001 boundaries, early syntax before long token/comment tails, budget
before later syntax, ignored punctuation in strings, input admission, and
explicit outside-scope baselines. Native integration tests exercise local and
HTTP preflight plus borrowed-owner query calls with cancellation pending,
query-only both off and on, an open transaction, a TEMP marker, unchanged TEMP
scope and total_changes, and owner recovery. Additive versioned controls and
direct unit tests extend these checks for the six and three new categories,
including valid neighboring tokens, independent ownership, and first-failure
stops. New integrations run each contract separately against local/HTTP
preflight and disposable borrowed owners with both query-only states.

The scalar-extension fixture records actual transport capabilities. NUL is a
valid scalar in an in-memory document but cannot be passed through OS argv.
Its frozen and legacy complete-process controls use `graphql --file` with a
disposable input file; its companion argv result is explicitly unsupported and
is not synthesized. Direct native preflight/borrowed-owner and unit tests cover
the same NUL document. This adds no new CLI transport. Ordinary unsupported
character ownership is not generalized, and raw isolated surrogate source
values are outside Rust's UTF-8 `str` domain.

The frozen oracle's dangling-backslash IndexError remains a recorded residual.
Its deep recursive-parser failures also remain explicit baseline controls;
these are not relabeled as parity. A separate native unit witness exercises
1500 nested input values without recursive Rust frames. The four excluded
crash/recursion inputs remain unchanged historical baseline controls; no native
crash, traceback, or recursion mapping is implemented. Generic syntax, other
numeric reasons, the dedicated single-quote diagnostic, and unrelated string/budget
precedence remain unclaimed. Checks are focused, not full installed or release qualification.
All catalogs are disposable; no production or destructive effect gates change.
