# Independent consumer JSON byte-decoder review

2026-10-11. Narrow, opt-in development evidence for sharing the existing PIN
response decoder with Jellyfin discovery. This is not a broader consumer,
network, credential, or migration-gate qualification.

## Result and source scope

No production defect was found in the new extraction or its two Jellyfin call
sites. The PIN decoder algorithm was moved without semantic changes; its
wrapper still maps all decode failures to `plex_login_failed`. Jellyfin still
maps failures to `invalid_response`. Raw response bounds stay with the callers.
No canonical, file, request-body, or dependency-feature change was made.

The reviewer did not modify the three production files. Their SHA-256 values
remain the owner's reviewed values:

- `crates/catabolic-runtime/src/consumers.rs`:
  `35377975d41064105cfffa9db6df74a46297882a601cc8f7b146dbdf0c1ab704`
- `crates/catabolic-runtime/src/consumers/json_wire.rs`:
  `d212483bf19560933d119dc531f0bc9b6c403ab1c37b012ed023d56c4fef22f7`
- `crates/catabolic-runtime/src/plex_login.rs`:
  `7cf5a8358c88eeb166710a1bf2ba0964af0f7b249d1b55d1910c30a20d1c9ebb`

## Added independent regression coverage

`crates/catabolic-runtime/tests/consumer_json_wire_review.rs` imports the exact
private production decoder into a test crate. Its three independent tests,
plus the imported helper's three existing unit tests, pass. They cover:

- All **65,793** byte strings of length zero, one, or two, compared with pinned
  CPython 3.14.6 `json.loads(bytes)`: 213 accepted and 65,580 refused
- **498** additional oracle cases, comprising 180 valid and 318 malformed wire
  cases: ten encoding/BOM forms, Unicode scalar and supplementary boundaries,
  truncated units and prefixes, repeated BOMs, finite floating-point extremes,
  signed zero/underflow, integers beyond 64 bits and a 1,000-digit integer,
  escaped duplicate keys, nested values, and private-looking number keys
- Explicit retention of native rejection for unpaired surrogates and non-finite
  numbers, and unchanged canonical-codec refusal of a UTF-8 BOM

The checked-in oracle is
`crates/catabolic-runtime/tests/fixtures/consumer_json_python314.json`, generated
by the pinned interpreter without resolving its executable symlink. Its
SHA-256 is `0416167bd572fe75e2a04b977c55f4f4003ace63bf9417144fe2bcc97979c301`.
The generation driver and result are retained under
`.local-tests/consumer-json-review/`.

`tests/test_rust_consumer_json_review.py` adds three owner methods with **17**
response scenarios:

- Ten malformed or wrongly shaped second responses, after a valid identity
  response, retaining the read-only catalog and error contract
- Two valid responses at exactly **8 MiB raw UTF-16**, one at each Jellyfin
  endpoint, whose decoded UTF-8 exceeds 8 MiB. This proves the extraction did
  not silently introduce a decoded-size bound
- Five encoding forms with escaped duplicate selected keys, private-looking
  number objects, and finite/integer number boundaries

Those three methods plus the owner's five methods give **eight methods and 48
response scenarios**, passing both through the copied historical development
CLI and the installed development extension. The installed probe checks every
wheel payload except RECORD before and after, verifies the wheel/CLI/extension
hashes, and verifies import from the installed package and frozen reference.

## Evidence and limits

The initial historical run used the shared development snapshot documented in
`RUST_MIGRATION_CONSUMER_JSON.md`, including CLI `cde3d542…2949` and extension
`aca9cd54…6e9a0c`. This is evidence for immutable artifacts and selected unchanged
sources, not current whole-source proof after unrelated changes.

Evidence is retained in `.local-tests/consumer-json-review/`, including Rust
unit output, Clippy output, copied-CLI and installed-extension output, installed
payload hashes, selected source fences, and the oracle generation result.
The first Rust harness attempted to load private-looking number objects with
raw serde_json; its two fixture-reader failures are retained. The reader was
corrected to use the existing hardened core decoder. The first test-only
Clippy warning is also retained and corrected. Neither required a production
change. The successful budget tests can log the fixture's expected connection
reset when the remote body is refused above its raw bound.

Frozen source reference: `453fca983222c6665a48775eef67c489c96b527e`, with pinned
CPython 3.14.6. Installed probes use its fixture/dependency environment. They
are not clean release-wheel qualification, strict paired-helper acceptance,
real-service acceptance, macOS proof, or M9/M10 closure.

The already documented Python surrogate-pass, native finite-number, and
raw-over-limit error-classification differences remain explicit and unchanged.

## Final shared development replay

The final eight-method/48-scenario suite also passes on both explicit
`phase4-shared-dev-3` surfaces. The copied-CLI and installed-extension reports
are `build3-cli.json` and `build3-installed.json` in the review evidence folder.
The wheel was installed with `--no-deps --no-index` into a separate reviewer
venv. The selected decoder, dependencies, and test source hashes, every
installed wheel payload except RECORD, the wheel itself, and the copied CLI
were verified before and after the selected probes.

- Owner build source digest, unchanged across build:
  `6f9230216379d0a8a74007b8ea3d807b855681cbfc46802901fd6887d1a8371e`
- Copied CLI:
  `07f177aa32ff32f8633a411a96a4f967ac31ba7f708b6f009c72f67afe27331f`
- Installed extension:
  `b915a026371968526a79c307f57a5b3c84961b7579c75aa862c33294bf38e0f6`
- Wheel:
  `922f7ec922275752abd42e7f4103eca6097f5b051b40a460d9bbb5018f4daf0b`
- Final independent Rust test:
  `00ecbc705d30d403628887841a1b2d1a0cdff2854eb3ef8720b60dd6e46a669f`

The independent Rust test target passes Clippy with `-D warnings`; the Python
regression module passes Ruff lint and format checks. `review-report.json`
contains the combined findings, selected source fence and evidence hashes.
No production fix was required. The development-only and narrow-scope limits
above still apply to this final replay.
