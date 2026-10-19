# CatalogQuery scalar-sort diagnostic replacement

This is a bounded follow-up to the eight direct-query witnesses retained in
`tests/parity/graphql-runtime-sort-controls.json`. Replaying the current archived
00950e7 native helper reproduced all eight outcomes before implementation. The
numeric and boolean diagnostics were still different from frozen Python 3.14.6
at 453fca983222c6665a48775eef67c489c96b527e; this is not a historical-only fix.

`CatalogQuery::page` now lets scalar non-string sorts reach its existing
available-sort lookup. Numeric and boolean values are still rejected, using the
same ordered available-sort message as null and unknown text. Omitted/default
sorts, valid text, limit-first precedence, cursor logic, SQL execution and
canonicalization are unchanged. Containers retain the existing native text
rejection. No wrapper, generated client, shared error type or Python adapter is
changed.

## Versioned evidence and verification

`tests/parity/catalog-query-sort-diagnostics-v1.json` is a separate replacement
contract. It retains the complete frozen-owner process envelope, archived native
process envelope and replacement companion envelope for each of 17 cases:

- The original eight controls, retaining their original arguments and results
- Valid year sort and false, zero, negative, fractional and wide-integer sorts
- Invalid-limit precedence with numeric, boolean and dictionary sorts

Seven numeric/boolean envelopes change; nine ordinary controls remain identical.
The seventeenth case is the explicit dictionary residual. The historical fixture
is preserved byte-for-byte and is hash-fenced by the replacement test. Archived
baseline results are retained rather than overwritten with new expectations.

The Python test checks exact stdout text, stderr text and exit code. Frozen owner
results are captured as canonical JSON, including exception class. The native
companion has a different existing transport: success JSON, or an operational
error on stderr with exit 2. Its whole envelope is checked separately against the
frozen message and the versioned target. No normalization discards fields or
coerces scalar kinds. Oracle inventory, interpreter version, helper bytes and
disposable database bytes are fenced across the test.

The Rust replacement tests exercise all 17 cases on one borrowed owner with an
existing transaction, TEMP marker and query-only flag. They also check container
rejections and ordered diagnostic construction from a different available-sort
set. The existing GraphQL-runtime test continues checking the original eight
controls against their current, explicitly updated scalar expectations.

Commands (with explicit native helper, frozen interpreter and reference source
selected in the existing CATABOLIC environment variables):

- `cargo test --offline --locked -p catabolic-store --test catalog_sort_diagnostics --test graphql_runtime_completion`
- `python -m unittest -v tests.test_rust_catalog_sort_diagnostics`
- `cargo fmt --all -- --check`
- `cargo clippy --offline --locked -p catabolic-store --test catalog_sort_diagnostics --test graphql_runtime_completion -- -D warnings`

## Explicit limits

A dictionary sort raises Python `TypeError` with an unhashable-dictionary message.
The Rust store exposes `Error(String)` and retains `sort must be text`; native
containers are not claimed as parity. Preserving the Python exception class would
require broader error-boundary work, which this change does not attempt. The
legacy CLI's text/choice parser cannot express the non-text inputs, so its parser
surface is not presented as evidence for those owner-level witnesses. The Python
extension's exception-class mapping is also outside this bounded claim.

No broader definition-validation, filesystem, schema, scalar-output, GraphQL
conflict-diagnostic, release, production-effect or aggregate qualification claim
is implied. The primary checkout and pre-existing qualification receipts were
not changed while this replacement was prepared in isolation.
