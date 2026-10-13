# Native GraphQL CLI file admission

This narrow follow-on corrects the preserved CLI's `graphql --file` read limit
from 1 MiB to the frozen 131,072-byte limit. The shared reader, GraphQL engine,
companion protocol, HTTP and Python wrapper are unchanged. File/stdin admission
now reports `input exceeds 131072 bytes` before variable parsing, as the frozen
CLI does. The engine's distinct `query document must be at most 131072 bytes`
error remains appropriate at its own boundary.

The [original failing witness](../tests/parity/graphql-file-input-remaining-gap.json)
retains its document recipe, hash, frozen response, prior native response and
immutable `b11666f` executable identity. Only its status is updated. This fix is
outside the earlier document-validation checkpoint and does not rewrite it.

## Bounded evidence

The frozen CLI is commit `453fca983222c6665a48775eef67c489c96b527e`, executed
with the existing pinned Python 3.14.6 reference environment. Tests use isolated
catalog copies, ordinary temporary files and stdin. No service or credentials
are involved. The new native implementation does not invoke Python.

`tests/test_rust_graphql_file_boundary.py` compares 55 complete typed results:
file and stdin, default/JSON/machine modes, ASCII/two-byte/four-byte UTF-8, and
131,071/131,072/131,073-byte inputs, plus oversize-before-malformed-variables
precedence. It compares complete stdout values, exact stderr and exit codes,
and checks catalog, executable and frozen-source identities. The immutable
baseline has 36 matching and 19 differing cases; all 55 match after the fix.

All 18 native legacy CLI unit tests, strict all-target CLI Clippy, formatting,
scoped Python lint and the bundled-guide freshness check pass.

The existing document-validation byte-boundary test also passes, retaining
the separate companion engine-limit result and unchanged catalogs. These
checks do not establish invalid-UTF-8, universal-newline or every CLI input
diagnostic's parity, installed/wrapper/platform qualification, or any M4–M11
hard gate. Source changes are limited to the CLI read-limit argument and tests.

Using the existing pinned environment and explicit native artifacts:

```sh
python -W error::ResourceWarning -m unittest \
  tests.test_rust_graphql_file_boundary \
  tests.test_rust_graphql_document_validation.DocumentValidationTest.test_document_byte_boundary_on_both_native_cli_surfaces -v
cargo test --locked --offline -p catabolic-cli --bin catabolic-legacy-native
cargo clippy --locked --offline -p catabolic-cli --all-targets -- -D warnings
```

Full local responses, retained failing baseline, build and test logs live in
`.local-tests/graphql-file-boundary/` in the isolated implementation worktree.
The independent narrow review passed on production commit `6957244`, rerunning
all 55 comparisons and the companion control with unchanged source, oracle,
interpreter and executable identities. Its extra matrix has **14 matches and two
retained failures**, not an all-pass result: malformed `--variables '{'` yields
frozen `JSONDecodeError` detail versus native `CatabolicError: invalid JSON`.
The same discrepancy occurs for `{profile}` (nine bytes), at the file cap, and
in the immutable prior `b11666f` binary. It is an existing variable-input
diagnostic gap, unchanged by this file-limit correction. The original failed
control run and complete responses remain preserved.

The [source-bound receipt](../tests/parity/native-graphql-file-boundary-checkpoint.json)
records both author and independent results, exact identities and evidence
hashes. This later documentation/receipt update changes no Rust or test code.
