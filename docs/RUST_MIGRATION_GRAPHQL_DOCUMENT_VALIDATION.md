# Bounded native GraphQL document validation

This slice closes exactly the three original witnesses in
[`graphql-validation-remaining-gaps.json`](../tests/parity/graphql-validation-remaining-gaps.json):

- `overlapping-alias-plus-unused-variable`
- `duplicate-input-object-default`
- `duplicate-input-object-discards-variable-use`

Their document text, variables, frozen result and historical native result are
unchanged. New complete frozen evidence lives in
[`graphql-document-validation.json`](../tests/parity/graphql-document-validation.json).
Python 3.14.6, graphql-core 3.2.12 and source `453fca9` are the independent oracle;
production parsing and validation are Rust. This does not add a Python facade,
wheel, runtime fallback, HTTP transport qualification or a default/release change.

## Production boundary

`validation.rs` reports differing field names among direct sibling fields sharing
a response name at selection-set entry. Response groups and their field pairs
retain source order, so overlap diagnostics precede child diagnostics and
operation-exit variable checks. Repeated compatible field names stay admissible;
argument, result-type and cross-selection overlap checks remain framework-owned.
The direct-sibling comparison counter preserves the frozen 250,000-comparison
bound, including its selection-set location and discard of that set's pending
conflicts when the comparison budget is exhausted. Diagnostic accumulation keeps
at most the first 20 errors and the frozen abort sentinel.

`graphql_parse::validation_document` derives an optional validation-only parser
view from already-admitted input positions and the original lexical tokens. It
assigns private collision-free keys to later duplicate input fields, then restores
all AST positions. `validation::locate` reads each original key token, preserving
its order, location and value. Uniqueness errors are interleaved with field/literal
errors at the original traversal point. Defaults, nested objects/lists, directive
arguments, unknown arguments/types/fields, and every variable occurrence remain
visible. A view-construction failure rejects the request rather than executing a
lossy input tree.

Only the GraphQL validation boundary sees this ordered view. The execution AST,
request source, document budgets, catalog serialization and digests are unchanged.
No global `serde_json/preserve_order` feature is enabled. There is no
fixture-keyed response substitution or object-key rewrite in execution.

## Bounded evidence

- 55 complete frozen/native cases on both native CLI surfaces, including the
  three originals, valid neighbors, ordering/caps, source positions, Unicode,
  ignored type tokens, direct-sibling comparison budget, token/expanded-selection/
  depth limits, read-only operation rejection and fragment-cycle rejection
- The same 55 cases through the actual Rust library, both local/HTTP-schema
  preflight contexts, and a caller-owned persistent session under both
  `query_only` states, retaining its transaction and TEMP marker
- Exact 131072-byte document admission on legacy file-input and companion stdio
  surfaces. The 131073-byte refusal is an explicitly non-parity wording control:
  frozen file admission says `input exceeds 131072 bytes`; native says
  `query document must be at most 131072 bytes`, unchanged from checkpoint 11.
  The exact replay recipe, source SHA-256 and full responses are retained in
  [`graphql-file-input-remaining-gap.json`](../tests/parity/graphql-file-input-remaining-gap.json)
- Library document/variable byte limits, timeout and variable-shape controls;
  cancellation precedence, post-cancellation usability, transaction/TEMP state
  and caller-selected `query_only` preservation
- Three separately labeled non-parity controls: cross-fragment conflicts,
  nested merged-selection conflicts and differing arguments when an unused
  variable already produces a preflight error. Their full native failures are
  unchanged from the immutable `b11666f` checkpoint 11 artifact identified in the
  fixture. They are not passing parity counts and cannot report resolver success

Run with explicit native artifacts and the pinned oracle:

```sh
cargo test --locked --offline -p catabolic-store --test graphql_document_validation
python -W error::ResourceWarning -m unittest tests.test_rust_graphql_document_validation -v
cargo clippy --locked --offline -p catabolic-store -p catabolic-cli --all-targets -- -D warnings
```

Full GraphQL validation, generalized field-merging validation, all resource and
platform boundaries, M6/HTTP, destructive publication/recovery and final native
default/distribution gates remain open. The unrelated runtime and malformed-parser
residual ledgers are not changed by this slice.
