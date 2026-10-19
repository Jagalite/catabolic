# GraphQL intake review: presence and typed falsey values

This is a focused follow-on to
[the generalized input checkpoint](RUST_MIGRATION_QUERY_EMBEDDING.md).
It does not qualify every GraphQL boundary, installed wheel, or release.

## Package-root typed facade correction

`catabolic_native.execute_graphql` now substitutes `{}` only for `None`.
It no longer silently converts `[]`, `False`, `0`, or `""` into an empty
variables object. Those values reach the existing native validator and reject.
The public signature and Rust validation are unchanged.

Seven paired cases against frozen commit
`453fca983222c6665a48775eef67c489c96b527e` with Python 3.14.6 verify:

- Omitted `variables`, explicit `None`, and `{}` succeed with identical complete
  response values
- `[]`, `False`, `0`, and `""` reject on both implementations
- Rejection is not exception-class parity: the package-root extension raises
  `RuntimeError`, while frozen Python raises `CatabolicError` for the first
  three invalid values. Frozen Python treats `""` as JSON text and raises
  `JSONDecodeError`; native rejects it as a non-object

The durable source-level regression test
`tests/test_native_graphql_typed_presence_review.py` has three methods covering
the exact forwarded JSON, falsey values, nested insertion order, arbitrary-size
integers, and unchanged call options. It isolates the actual facade function
with a recording native stub; it is not an installed-runtime test.

## Measured per-surface distinctions

These measurements use rebuilt snapshots of the final frozen Rust sources and
the corrected Python package-root facade:

- Direct raw PyO3, companion CLI, and persistent raw PyO3 accept omitted
  variables and `{}`. Explicit raw JSON `null`, `[]`, `false`, `0`, and `""`
  reject. Frozen Python's query function accepts explicit `null`, so that raw
  null difference remains documented rather than normalized globally
- The persistent public `graphql_query.execute_graphql` facade accepts omitted
  variables, `None`/JSON `null`, and `{}`; invalid arrays, booleans, numbers, and
  empty strings reject
- Actual HTTP POST `/v1/query/graphql` accepts omitted variables and `{}`.
  Frozen and native both reject `variables:null`, arrays, booleans, numbers,
  and strings with HTTP 422 `invalid_request`. Generated request IDs differ
- The corrected legacy CLI accepts omitted variables, explicit JSON `null`,
  and `{}`, matching frozen legacy CLI. Both reject `[]`, `false`, `0`, and
  `""`. This adapter behavior is distinct from the raw companion interface

All HTTP requests used numeric loopback, disposable catalogs, and credentials
issued only for those fixtures. No live catalog or service was accessed.

## Evidence and reproduction

Local evidence is recorded separately so the earlier input-coercion receipt
remains unchanged:

- `.local-tests/graphql-intake-empty-root-final/presence-results.json`: seven
  presence/value cases on seven surfaces, with full frozen/native outcomes
- `.local-tests/graphql-intake-empty-root-final/verify_presence.py`: paired
  runner; asserts all seven corrected typed-facade acceptance/rejection cases,
  identical successful values, HTTP null rejection, and raw null guards
- `.local-tests/graphql-intake-empty-root-final/results.json`: 14 original intake vectors
  plus one all-skipped-root vector across six surfaces; all 180 checks pass
- `.local-tests/graphql-intake-empty-root-final/receipt.json`: source/artifact
  hashes, command results, and scope

The additional skipped-root guard returns an empty data object and standard
zero-record extensions, including HTTP 200. That guard did not qualify nested
empty-object/list completion; the subsequent [focused completion correction](RUST_MIGRATION_GRAPHQL_REVIEW.md#present-objects-with-every-selected-field-skipped)
adds separate evidence for those two residual witnesses.

Earlier receipts remain unchanged under
`.local-tests/graphql-intake-independent-final/`,
`.local-tests/graphql-intake-independent/`, and
`.local-tests/graphql-typed-presence-review/`; those use explicitly labeled
earlier snapshots and do not establish final legacy adapter behavior.

Run the durable source gate with:

```sh
python -m unittest tests.test_native_graphql_typed_presence_review -v
ruff check tests/test_native_graphql_typed_presence_review.py \
  crates/catabolic-python/python/catabolic_native/__init__.py
ruff format --check tests/test_native_graphql_typed_presence_review.py \
  crates/catabolic-python/python/catabolic_native/__init__.py
```

The paired runner uses a copied development extension rather than an exact
installed wheel. Its immutable extension SHA-256 is
`8be8364e4e144af065047e17413d27116bdc1f763cc857850dcd0d04d61d53d1`;
the companion/HTTP executable is
`55a3e3fb905c4c7da32ff617895d1379b63fab2e9e82d972f83bc826ba9aee35`;
the legacy CLI executable is
`a910f56501903a604938dc40e1cc5541608cca8f2531b5e6a07e1ec5d2e92e09`.
There is no new wheel or release qualification claim.
