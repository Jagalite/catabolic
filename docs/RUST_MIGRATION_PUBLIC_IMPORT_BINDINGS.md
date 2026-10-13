# Immutable public import bindings

This is a four-row, opt-in M10 checkpoint in `catabolic_native`. It does not
replace the default `catabolic` package or qualify any callable, query behavior,
HTTP application, layout preset, console entry point, or broader migration gate.
The frozen inventory and its existing classifications are unchanged.

## Exact selection and ownership

The selected `preserve-repository-import-binding` inventory rows are:

- `python:catabolic.component_sql.OCCURRENCES_SQL`
- `python:catabolic.documentation.TOPICS`
- `python:catabolic.graphql_query.SDL`
- `python:catabolic.http.contract.VERSION`

Import each corresponding path under `catabolic_native`. The four Python binding
declarations only reexport native objects. There is no Python implementation or source-tree
lookup for these values, and no optional GraphQL/FastAPI/Pydantic dependency.

`OCCURRENCES_SQL` comes from the existing store resource entry
`views[name=catalog_component_occurrences].sql`. The generator verifies exact
string equality against the frozen oracle before allowing reuse. No SQL changes
are made and this checkpoint does not review SQL semantics.

The existing store `schema.graphql` contains `LOCAL_SDL` (7,921 UTF-8 bytes),
which differs from the public `SDL` (7,803 bytes). A pure `schema_text()` accessor
returns those already-embedded bytes. The binding uses the generated public
prefix length; generation verifies both the complete `LOCAL_SDL` resource and
exact public prefix. It never builds a GraphQL schema or changes the resolver.

`TOPICS` is generated as Rust literal string tuples and converted by PyO3 into
an exact built-in Python tuple of 49 four-string tuples. Both tuple levels are
immutable. `VERSION` is the exact string `1.7.0`, independently of the companion
package version. They are generated declarations, not hand-maintained copies.
The generated declarations cannot open a database, run migrations, access the
network, or start an HTTP application.

## Frozen provenance and generation

`scripts/generate_public_import_bindings.py` requires Python 3.14.6 and an
independent clean Git checkout at
`453fca983222c6665a48775eef67c489c96b527e`. It checks committed package bytes,
rejects extra or linked payloads, verifies the four inventory/source mappings,
and verifies every imported oracle module's origin and source hash. The oracle
executes only in the development/check subprocess, never in the installed
native package.

`tests/parity/public-import-bindings.json` records the selected inventory hash,
source hashes, imported-module provenance, native resource selectors, recursive
value/type hashes, string UTF-8 hashes and lengths, and tuple shape. The sole
new value declarations live in generated `public_import_values.rs`.

Generate, then verify without rewriting:

```sh
/path/to/pinned/python -I -B scripts/generate_public_import_bindings.py \
  --source /path/to/frozen-reference
/path/to/pinned/python -I -B scripts/generate_public_import_bindings.py \
  --source /path/to/frozen-reference --check
python -I -B tests/test_native_public_import_contract.py
```

The source-contract suite requires exactly six distinct successful cases. It
covers inventory/source selection, rejection of changed/missing/duplicate rows,
Python-only reexport structure, exact existing resource bytes, native literal
registration, and qualification accounting/early-failure receipt cleanup.
Missing required arguments, mutually exclusive modes, an unknown option, and
optimized Python `-O` must remove both a prior success receipt and a prior
`.attempt.json` receipt before failing.

## Installed evidence boundary

`scripts/native_public_import_tests.py` requires exactly four paired value/type
groups and eight distinct successful installed cases. No skip, expected failure,
missing case, or duplicate case can count as success. The tests compare complete
recursively typed values from the frozen oracle, check identity of native
reexports, reject outer and inner tuple mutations, verify reloads, and perform a
fresh isolated import with optional engines unavailable. The fresh import denies
Python-audited database/network/subprocess calls and writes, and leaves an empty
temporary working directory/home unchanged. This is bounded import evidence,
not comprehensive operating-system syscall or underlying-owner qualification.

Release mode verifies every installed wheel payload against the exact archive,
the extension and packaged helper, both external CLIs, the paired release build
manifest, and unchanged source/script/inventory fences before and after the
suite. It requires a clean environment without the legacy Python package or
optional engines. Run from outside the checkout, with isolated Python:

```sh
/path/to/clean/python -I /checkout/scripts/native_public_import_tests.py \
  --wheel /artifacts/catabolic_native-...whl \
  --build-manifest /artifacts/build-manifest.json \
  --inventory /checkout/tests/parity/python-api.json \
  --reference-root /path/to/frozen-reference \
  --reference-python /path/to/pinned/python \
  --report /evidence/public-imports.json
```

The explicit `--dev` alternative accepts only a helperless installation and
emits `development_only: true`, without release-artifact claims. It must not be
substituted for the same-source paired-wheel release run. Both modes keep the
frozen oracle in a separate process. The installed CI matrix runs release mode
on Python 3.11 and 3.14 and retains its own receipt.

## Local qualification

On Linux x86_64, the helperless-development and paired-release modes each passed
all eight installed cases and all four exact pairs on separate clean Python
3.11.15 and 3.14.6 interpreters. Each source-contract run passed all six cases.
Generation/check, targeted Ruff, `cargo fmt --all --check`, and diff checks passed.
The final strict environments were populated using pip 26.2 with `--no-deps`
and `--no-index`; they contain only the native companion distribution.

The paired local artifacts are in `.local-tests/public-imports/release/`:

- Wheel SHA-256: `b113e6cd50041668ef03d00623ad123a830123cb89fd388c2600b7e0201e15ac`
- Extension SHA-256: `6060be1be132e523b610793a0654cf82d048ea47a4e46eff2c8c780e96a6958a`
- Native CLI SHA-256: `b64e7fea3b642d595d64b25db1c6f80ea109f8586fcd3efce89e20b667b216cd`
- Legacy CLI SHA-256: `4893d8670c8f515031dc893fceb9e9297ab1bcd31f691c8a3e4940be857bb399`
- Build-manifest SHA-256: `04b1cdaf88e0930bc73a081e5c8e6ab752158c28c135c89d1262731c9cd4c2d9`

`tests/parity/native-public-import-bindings.json` indexes the four exact receipts,
source checks, artifact identity, and a retained rejected installer attempt. An
initial uv installation added `uv_cache.json` to dist-info; the unchanged strict
payload gate correctly rejected it before running the suite. Separate clean pip
installations then passed. No extra-payload exemption or skip was introduced.

Independent review identified a stale-receipt bug on malformed invocations. The
driver now uses the shared pre-parse cleanup, and the negative regressions above
cover that boundary. Both source and all four installed development/release runs
were repeated successfully; wheel, extension, helper and paired CLI bytes are
unchanged. Previous receipts remain in `.local-tests/public-imports/pre-review/`.

This is not a full helper/CLI behavior smoke, macOS check, independent review,
aggregate gate, or release qualification. No artifacts were published and no
Python default was changed. CI configuration is not evidence that CI ran.
