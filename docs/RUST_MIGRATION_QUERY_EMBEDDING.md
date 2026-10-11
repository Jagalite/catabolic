# Persistent Python query checkpoint

This opt-in checkpoint adds `catabolic_native.saved_queries.Queries` and the
`catabolic_native.sql_query.execute_sql` / `graphql_query.execute_graphql` module
facades. Their ten selected class, constructor, and function/method signatures
match the frozen public inventory. It is separate from the existing
[69-signature embedding checkpoint](RUST_MIGRATION_EMBEDDING.md); neither gate
qualifies the whole 159-row preserved inventory. Together the two selected gates
cover 79 rows, including previously selected non-preserve service rows.

## Ownership

- `Queries.get/list/validate/put` forward to the production definitions owner
- Selection and execution use the production `Evaluation`, SQL, and GraphQL
  owners. A supplied `_store` lends its existing connection and snapshot; it
  does not reopen a database, spawn a companion operation, or retry in Python
- GraphQL's synchronous context restores that exact Store after normal results,
  errors, cancellation, and unwinds. Evaluation lending similarly returns its
  owner. Failed connection-state cleanup retires the Store rather than handing
  back unchecked state. Existing scoped authority checks are retained
- Native query timeout handlers compose the outer Python signal bridge. Resolver
  entry also polls cancellation before taking its resolver mutex. The original
  Python signal exception is propagated after restoring the native session
- Writes performed through `Queries.put` participate in the caller's transaction
  and nested savepoints. Read-only snapshots, detached/closed owners, thread
  affinity, and lost-transaction refusal remain owned by the existing Session
- Without `_store`, the module functions open one bounded read-only Store for
  that call. Production input validation runs before opening it. The older
  package-root companion functions retain their pre-existing signatures

## Native evaluation adapter and explicit gaps

`catabolic_native.evaluation.EvaluationSession` supports same-owner
`Queries.select/run` calls with a shared native cache and cumulative time, ID,
and byte budgets. It checks Store/profile identity, the recorded fallback epoch,
explicit cancellation, and expiration. Its `check`, `select`, `account`, and
`pause_for_io` adapters use this native state; `close` releases it. No extra
EvaluationSession inventory rows are counted by this checkpoint.

The adapter does not implement the complete frozen EvaluationSession contract:

- Foreign legacy Store/EvaluationSession/Access objects cannot be transferred
- `access` must be `None`; no public native scoped-access context adapter is
  exposed here. Existing native HTTP/scoped-query owners are unchanged
- Python `_context_factory` resolver/access callbacks are rejected explicitly
  before execution, rather than being ignored or run without their authority
- The mutable Python `context` dictionary, arbitrary cache injection, and exposed
  Python `result_bytes` bookkeeping are not provided. Timeout/ID constructor
  values use the native bounded limits, unlike the permissive frozen constructor
- Trusted index-expansion `EvaluationSession.rows` is explicitly unimplemented;
  callers can use saved bounded SQL reports for the supported evaluation route
- GraphQL documents using `_context_factory`, foreign context error precedence,
  every composition/byte/count boundary, and concurrent cancellation by mutation
  of a Python session field during an active call remain unqualified
- Native error classes remain `catabolic_native.CatabolicError`, not imported
  legacy exception classes. SQLite blob/memoryview pagination arguments are
  outside the JSON facade. Selected tests do not qualify all SQLite errors
- Full GraphQL document validation is still unqualified. A follow-on production
  pass closes the mixed scalar/list/enum/input-object coercion vectors and the
  unknown-field plus unused-variable aggregation vector described below. Four
  further verified static/parser differences remain: overlapping aliased field
  conflicts, duplicate input fields in defaults, duplicate input fields that
  cause a variable use to be discarded, and whitespace inside wrapped variable
  types. Exact replay inputs and frozen/native responses are retained in
  [`graphql-validation-remaining-gaps.json`](../tests/parity/graphql-validation-remaining-gaps.json)

## Generalized GraphQL input checkpoint

Variable coercion now runs for the selected operation before resolver entry,
using the same native Store, interruption bridge, and execution deadline. It
handles built-in scalars, enum suggestions, singleton-to-list coercion, nested
non-null errors, input-object field validation and suggestions, variable
presence/defaults, schema input defaults, and the frozen 50-error cap. Integer
IDs retain arbitrary-size decimal lexemes; integral floating IDs, including
negative zero, follow the frozen conversion. Already-validated numeric ID
literals and defaults are normalized before framework execution, so values
beyond u64 remain exact while fractional literals and wide Ints still fail.
Diagnostic inspect formatting has
bounded strings/containers/depth and a frozen Python 3.14.6 Unicode printable
range table. The generator is development-only; execution remains Rust. Its `--check` mode
refuses a missing or drifted table without writing it; disposable tests verify
both refusal and unchanged bytes.

A GraphQL-specific `InputVariables` wrapper carries raw object insertion order
through the legacy CLI, native companion, direct Python extension, persistent
Python Store calls, and HTTP GraphQL request intake. Duplicate JSON keys replace
the earlier value without moving its position. The existing hardened core JSON
decoder remains authoritative, including arbitrary numbers and literal objects
that resemble serde's private number sentinel. No workspace serde features,
canonical encoding, digests, schema, or Python facade signatures were changed.
Existing Rust `Value` callers remain supported but cannot recover input order
that their caller already discarded. Inline selection's Value-only diagnostic
order is not qualified here. Persisted saved-query definitions are canonically
key-sorted by the frozen storage implementation too, so that storage ordering
is intentional.

Document validation now includes source-ordered definitions, nested/transitive
variable uses, undefined/unused/type-position checks, input types and literal
defaults, directives, required/duplicate arguments, fragment type/spread checks,
unused fragments, exact nested locations in covered cases, and the frozen
20-error validation cap. The four residual cases above prevent a claim of full
GraphQL parser or static-validation parity.

Separate frozen evidence for this checkpoint:

- Ten differential methods compare 182 complete legacy CLI result pairs against
  commit `453fca983222c6665a48775eef67c489c96b527e` and Python 3.14.6
- All three vectors in
  [`graphql-composite-coercion-known-gaps.json`](../tests/parity/graphql-composite-coercion-known-gaps.json)
  and the vector in
  [`graphql-document-validation-known-gaps.json`](../tests/parity/graphql-document-validation-known-gaps.json)
  now pass; those files remain byte-identical historical input, including their
  original status strings
- Rust document tests cover 59 frozen invalid documents, 13 valid guards and the
  HTTP schema guard; separate generic unit tests exercise nested required input
  fields/defaults not currently reachable in the flat public input schema
- Same-Store identity, transaction state, ordered raw request handling, exact
  integers, cancellation/deadline handling and unwind tests pass. The original
  14 GraphQL differential methods and eight inbox-review methods still pass,
  including both inbox SIGINT channels
- These source/binary checks do not replace the installed exact-wheel gate below,
  increase its inventory count, or qualify every HTTP/embedding input boundary

Run the additional focused checks with the pinned reference environment:

```sh
python scripts/generate_graphql_printable.py --check
python -m unittest tests.test_rust_graphql_coercion tests.test_graphql_printable_generator -v
cargo test -p catabolic-store --locked --offline --lib graphql
cargo test -p catabolic-store --locked --offline --test query_document_validation --test query_scalar_coercion
cargo clippy -p catabolic-store --all-targets --locked --offline -- -D warnings
```

A paired check exposed a private-table denial wording difference. Native SQL now
retains SQLite's access-denial message for HTTP private-table reads, matching the
frozen direct read-denial branch. The rejection itself and allowed tables have
not changed. This does not resolve or reclassify any previously documented
scoped-query restriction/discrepancy.

## Independent input review

A subsequent [independent review](RUST_MIGRATION_GRAPHQL_REVIEW.md) found and
corrected additional literal-rendering, runtime nullable-argument/directive, and
legacy/typed-facade presence cases. It also retained thirteen additional explicit
parser, location, enum-resolver, field-merging, empty-completion, and introspection-input vectors in
[`graphql-review-remaining-gaps.json`](../tests/parity/graphql-review-remaining-gaps.json).
The four residual vectors above remain unchanged; they are not an exhaustive list
of remaining GraphQL differences. Repeated-field/runtime-null and
introspection-owned/runtime-null combinations preserve their pre-fix strict
errors until their separate execution owners can implement the complete
contract. No blanket GraphQL parity or final-wheel claim follows from either
checkpoint.

## Separate qualification

Source checks:

```sh
python -m unittest tests.test_native_query_embedding_contract -v
cargo test -p catabolic-store --lib embedding::tests --offline
cargo test -p catabolic-store --lib graphql::tests --offline
```

Install the final exact paired helper/extension wheel in an isolated environment,
then run outside the checkout:

```sh
/path/to/clean/bin/python -I /path/to/repo/scripts/native_query_embedding_tests.py \
  --wheel /path/to/exact.whl \
  --inventory /path/to/repo/tests/parity/python-api.json \
  --reference-root /path/to/frozen-checkout \
  --reference-python /path/to/pinned-3.14.6/bin/python \
  --report /path/to/query-proof.json
```

The runner checks frozen source provenance and imported module bytes, runs ten
installed lifecycle tests, and compares 77 frozen/native result groups using
separate copies of one disposable database. It records actual calls and a
method-specific remaining gap for each of the ten rows. Generated query UUIDs
are explicitly normalized; definition digests are verified against each real
serialized definition. Only generated timestamps are removed. Composition
result IDs are normalized, while definition/value/error comparisons stay exact.

The installed tests include actual SQL/saved-report SIGINT and GraphQL custom
signal exceptions during owned execution, nested rollback, snapshots, caller
identity, supported native-session limit/lookup/identity error precedence,
explicit unsupported contexts, input-validation precedence,
read-only/detached/closed refusal, thread affinity, and lost-transaction handling.
Rust tests separately inject GraphQL and evaluation unwinds and exercise
reentrant independent cancellation bridges.

`--dev` can replace `--wheel` for explicitly weaker helperless development
checks. It records `development_only=true`, a null wheel hash, installed payload
hashes, and the test script hash; it makes no wheel completeness or release
claim. The strict `--wheel` verifier is unchanged and rejects incomplete helper
artifacts. Final platform/wheel qualification remains required. The older
69-row / 19-test / 82-group script and its claims are not replaced or incremented
by this separate report.
