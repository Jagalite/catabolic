# Consumer JSON response byte decoding

2026-10-11, opt-in implementation and development-fixture evidence. This narrow
change does not close M9, M10 or any other migration hard gate.

## Change

Jellyfin discovery's `/System/Info` and `/Library/VirtualFolders` responses now
use the same UTF-8/BOM, UTF-16 and UTF-32 byte detection previously implemented
for Plex PIN JSON responses. The shared helper is
`catabolic-runtime/src/consumers/json_wire.rs`; the PIN wrapper preserves its
existing `plex_login_failed` error classification. Actual JSON values still
pass through the existing hardened core decoder, retaining integer lexemes,
last-key-wins handling and ordinary objects with private-looking number keys.

This helper is restricted to remote JSON responses. Canonical serialization,
digests, private credential-file decoding, HTTP request bodies, GraphQL input
ordering, and the dependency feature set are unchanged. Existing raw response
limits remain 8 MiB for consumers and 64 KiB for PIN requests. This change does
not introduce a separate decoded-size restriction.

## Development evidence

`tests/test_rust_consumer_json.py` declares five tests with 31 response scenarios:
21 valid cases and 10 malformed/over-limit cases. They compare frozen discovery
against the native owner through numeric-loopback HTTP fixtures. Valid cases
cover non-ASCII text, supplementary characters, BOMs, both endiannesses, mixed
encodings between requests, duplicate keys, large integers and private-looking
object keys. Tests check that discovery leaves the catalog unchanged and does
not enqueue a scan. The three new Rust tests additionally cover short scalar
encoding detection, 80 valid value/encoding combinations, malformed units and
unchanged canonical UTF-8/BOM refusal.

Frozen Python: `453fca983222c6665a48775eef67c489c96b527e`, using the
pinned CPython 3.14.6 environment. Evidence is retained in
`.local-tests/consumer-json/`:

- `pre-fix.log`: seven non-UTF-8 response failures on the unchanged phase-three
  native artifact, establishing that this was a pre-existing gap
- `pre-fix-budget.log`: unchanged limit-classification behavior on that artifact
- `regressions.log`: an initial 49-test run had 48 passes and one missing explicit
  extension precondition; it is retained as a failed qualification attempt
- `shared-regressions.log`: all **49 tests pass** with the explicit copied CLI
  and extension, comprising five new tests, 24 consumer tests and 20 PIN tests
- `shared-units.log`: three new decoder tests and nine existing PIN tests pass
- `shared-clippy.log`: runtime all-target Clippy passes with `-D warnings`

The successful development run used the source-fenced shared build manifest in
`.local-tests/phase4-shared-dev/build-manifest.json`:

- Crate source: `0e0bad440cd231bbabb8fe61ff2a84ab986dde299fed8ca3f2fc41cb6c16c0c8`
- Native executable: `cde3d542e87f08cee7d525c068c243f8ce86afb76e8381cd8ff08db537ff2949`
- Extension: `aca9cd54e7ee3f89b4df305f31cbfdb4ef9102192cdca0d0189603d18b6e9a0c`
- Helperless wheel: `efac9db4a6cffe01a7eccdad325f7522aed4a7d0751a8a1b6e08b6a3356052fb`

Subsequent unrelated CatalogRefresh edits make that a historical shared-source
snapshot. The decoder files remain unchanged. Independent review subsequently passed; see
`RUST_MIGRATION_CONSUMER_JSON_REVIEW.md` for the final build-3 replay. Integrated
source-fenced aggregate qualification is still pending.

## Explicit limits

- Frozen Python JSON byte decoding can accept lone surrogates using its
  surrogate-pass behavior. Native strings contain Unicode scalar values only;
  this pre-existing representation difference is not resolved here
- Frozen Python may accept non-finite constants that the native hardened JSON
  decoder rejects. This pre-existing difference is unchanged
- Above the raw response limit, frozen subprocess transport reports
  `temporarily_failed`, while native reports `response_limit`. Both refuse; the
  test preserves this explicit pre-existing diagnostic difference
- These are disposable loopback/development checks, not real Jellyfin or Plex
  service acceptance, release-wheel qualification, macOS proof, or broad
  transport/credential safety review

The five new scenarios also passed through the installed development extension's
in-process `execute_operation` adapter. `installed-probe.json` records all checked
wheel payload hashes before/after and the exact five case IDs. This replay uses
frozen fixture dependencies from the reference environment; it is not a clean
release installation, and repeats the same five scenarios rather than adding five
new distinct contract cases.
