# Native GraphQL comment-token budget

The frozen Python 3.14.6 / graphql-core 3.2.12 reference counts comment tokens
toward the existing 4,000-token document limit. Its check occurs only after the
lexer advances past all adjacent comments to the next non-comment token or EOF.
The earlier native lexical scan discarded comments and could accept documents
the reference rejected. The historical `comments-not-counted-native-boundary`
capture in `graphql-token-budget-locations-v1.json` is preserved byte-for-byte.

## Bounded repair

`catabolic-store/src/document.rs` increments the existing scalar token count for
each comment and performs a final count check at EOF. It does not allocate a
token list, parse comment contents, change the 4,000-token threshold, or alter
string/block-string accounting. Checks after a comment run use the next token
or EOF's existing source-position conversion, rather than a comment's position.
The original document-byte, timeout and variables admission still precede this
scan. No schema, resolver, owner, storage, runtime dependency or default changes.

## Additive evidence

`graphql-comment-token-budget-v1.json` records 80 complete frozen-reference and
composed-native baseline responses. Cases cover exactly 3,999/4,000/4,001
combined tokens, leading/middle/trailing and mixed comments, LF/CR/CRLF, EOF
with and without a final newline, Unicode comment contents, delayed rejection
after comment runs, comment-only input, and quoted/escaped/block-string hashes.
The fixture records the composed source/helper hashes and independently counted
lexical totals. The original comment witness is replaced only through the exact
versioned replacement contract; the original file and other historical
precedence or parser differences remain unchanged.

`tests.test_rust_graphql_comment_token_budget` rejects changed provenance,
replacement scope, input order, token counts and complete response envelopes.
`tests.test_rust_graphql_token_budget_locations` replays the 22 prior cases plus
80 added cases against the frozen reference and both explicit native CLI
artifacts. Exit status, complete JSON value kinds, stderr and unchanged catalog
bytes are checked. Rust lexical tests compare all 80 cases; direct pre-context
and persistent-owner tests cover all 34 added rejections plus the original
witness in local and HTTP preflight modes, with owner transaction, TEMP marker,
query-only mode and total-change invariants.

## Limits

This closes only comment accounting in the bounded lexical preflight. Existing
earlier-syntax-versus-budget precedence differences and general parser/EOF
location differences are not relabeled or repaired here. This candidate is
based on the separately qualified diagnostic composition, without the separate
EOF-location patch. An early absent-catalog CLI probe failed because the CLI
requires an existing catalog; that failed receipt is preserved and supports no
absent-catalog preflight claim. Qualification uses initialized disposable
catalogs and direct pre-context calls instead.

Focused source-bound receipts, exact commands, unchanged source/dependency
fences, artifact hashes and retained failures accompany the isolated patch.
They do not establish full native/dynamic aggregate, installed-wheel, platform,
performance, release, milestone completion or primary-checkout adoption.
