# Opt-in preserved CLI adapter checkpoint

Reference: `453fca983222c6665a48775eef67c489c96b527e`; oracle Python **3.14.6**.
This is an implementation checkpoint, **not full CLI parity or a default switch**.
The Python `catabolic` command and companion `catabolic-native` remain unchanged.

## Entry points and ownership

Build `cargo build -p catabolic-cli --bin catabolic-legacy-native --locked`.
The new executable accepts the preserved command tree. It reads `CATABOLIC_DB`
and `CATABOLIC_PROFILE`, refuses implicit database selection, implements the
legacy global-option scope, and renders `--json`, text, and `--machine` v1 replies.
The ordinary binary never executes Python or loads a legacy business owner.
Unsupported business adapters return an explicit error with no automatic fallback.

The development-only `legacy_parse` Cargo example accepts JSON argv arrays on
stdin and returns parsed namespace or exact parser stdout/stderr/exit status. It
never calls dispatch. It is a qualification artifact, not a user command or a
replacement for command tests.

`resources/legacy-cli.json` embeds interface metadata, documentation, frozen
schema artifacts and presentation tokens. `scripts/generate_legacy_cli.py`
requires Python 3.14.6 with `PYTHONHASHSEED=0` and verifies every reference `src/catabolic/` checksum
against the frozen inventory before extraction. Generation is development-only.
The resource records baseline, interpreter, hash seed and source hashes and has a license
sidecar. The normal source-content build fence includes these crate resources.

Terminal help uses native wrapping of frozen argparse presentation tokens at
runtime `COLUMNS`; there are no per-tested-width help snapshots. Domain owners
continue to enforce typed bounds after argument conversion. Frozen Python permits
non-finite float tokens at the parser boundary; owners still reject non-finite
operation budgets before mutation.

## Coverage accounting

[The explicit owner ledger](../tests/parity/legacy-cli-owners.json) maps every one
of the **237 frozen leaves plus five dynamic query variants**. Regenerate it with
`scripts/legacy_cli_owners.py`. Parser coverage includes all **271 nodes**.
`adapted_unqualified` means a real native owner is wired, not that every flag,
output byte, mutation effect or failure schedule has passed qualification.
`partial`, `safety_gated` and `missing_adapter` rows remain visible blockers.
The current ledger has **203 adapted but unqualified**, **28 missing adapters**,
**10 partial leaves** and **one safety-gated leaf**. Counts describe routes,
not passed behavioral qualifications.
The independent terminal review adds exact entrypoint mappings for 50 reviewed
routes; 192 rows remain explicitly marked as family-level owner associations.
Embedded offline resources are identified separately from native business owners.

The independent parser suite `tests/test_rust_legacy_parser.py` records exact
scenario IDs/argv and compares the actual frozen argparse implementation with the
compiled native probe. The current local parser run passed 20,170 comparisons,
including all help/usage nodes, valid leaves, options, abbreviations, missing and
conflicting arguments, Unicode integer/float conversion, integer digit limits,
non-finite float tokens, environment defaults, sentinel/positional precedence,
and terminal widths 40, 79, 80, 81 and 120. This is parser-only evidence.

`tests/test_rust_legacy_cli.py` separately uses pinned oracle processes and
independent disposable catalogs. Named `LC-*` scenarios cover offline docs/spec,
missing-database error channels, native catalog reads and mutations, persistent
reopen, SQL tables/machine envelopes/typed bounds/incomplete results, saved-query
variants, unknown manifest fields, signal exit 130, and explicit owner/gate
refusals. It checks exact bytes except the clearly marked unknown-field
round-trip scenario, which checks complete JSON structure and large integer
preservation, and mapping capabilities, whose frozen adapter order is derived
from randomized Python sets. Native capability generation is pinned to hash seed
zero; arbitrary-seed byte-order parity is explicitly not claimed.

Additional `LC-*` scenarios compare native processing admission/run, selection
conflicts and typed budgets, fallback enable/disable/history and durable settings,
HTTP-operation local controls, copy-plan reads and rendition decisions. A
1,001-file processing selection crosses the 1,000-ID admission batch boundary:
only generated job UUIDs and enqueue timestamps differ; every durable source
snapshot, option, cache fingerprint and state is compared. The separate
`tests/test_rust_legacy_plex.py` qualifies metadata preview/error/incomplete/apply
exit codes **0/2/3**, exact envelopes, remote effects and persisted events against
isolated frozen and fake-loopback states, plus signal exit **130**. Only event
UUIDs/timestamps are normalized in its effect comparison. Fresh aggregate artifact-bound evidence is still required.

## Shared native boundaries

- `catabolic_store::public_reads`: streamed four-MiB bounded artifact, recipe,
  output, processing and binding reads reused by terminal and persistent facades
- `catabolic_store::public_controls`: bounded local HTTP/copy reads and transactional
  fallback/rendition controls; existing production owners retain policy admission
- `catabolic_store::work_inbox`: profile-fenced inbox evidence and retained-link reads
- `catabolic_runtime::public_processing`: same-owner selection and 1,000-ID batches;
  native artifact/process owners handle runtime work and cancellation
- `catabolic_runtime::source_events`: explicit preserved supervise route
- `catabolic_runtime::public_scan`: existing Application.scan orchestration moved
  out of the PyO3 terminal boundary without changing Session/store ownership
- SQL/GraphQL and saved queries call the production query/evaluation owners
- ItemWorkflow, curation, tagging, definitions, layouts and programs call their
  existing native owners; documents preserve unknown fields
- Plex login/import/matching/metadata use their native owners; no real accounts
  or remote services are used in these tests

The legacy executable is never registered as a companion worker. Native helper
operations retain the explicit `CATABOLIC_NATIVE_BINARY` override and installed
companion registration contracts; there is no ambient PATH lookup or Python
fallback. Helper-dependent legacy workflows must be qualified with the actual
companion artifact, separately from parser tests.

## Remaining limits

The ledger, rather than parsing success, is authoritative about missing adapters.
Aggregate maintenance/watch loops, legacy refresh and identification,
API administration/secret-file contracts, fallback worker and the remaining
projection/mapping orchestration adapters need closure. Supervise, inbox,
processing, artifact and Plex owners are linked, with selected independent
scenario evidence rather than blanket leaf acceptance.

Exact dynamic-object key ordering, arbitrary unknown extension insertion order,
all Python exception subclasses/diagnostics, every rule/report output shape,
all signal timing windows, and every adapter's mutation/recovery behavior remain
qualification gaps. Known native public-scan continuation, multi-source barrier
and scan-specific cancellation gaps are not closed by moving the function.
The [independent terminal/controls review](RUST_MIGRATION_LEGACY_CLI_REVIEW.md)
records fixes for float rendering, Unicode diagnostic escaping, selected populated
page column order, and bounded local reads. Unpaged binding/fact reads and local
HTTP-operation backing definitions now refuse records or aggregate results beyond
the four-MiB canonical row budget. This intentional native safety-limit difference
does not silently return a partial success and is not frozen-Python byte parity.
Invalid UTF-8 Unix argv is refused with exit 2 before parsing; Python's
surrogate-escaped byte-path behavior is not implemented by this adapter.
A fresh isolated helperless development wheel passed both installed Plex import
signal tests, including `Application.scan` setup, custom Python signal exception,
writer progress and cancellation rollback/retry. Its explicit companion and
wheel hashes, installed file hashes and unchanged scoped scan/embedding hashes
are recorded in `.local-tests/legacy-installed-dev/development-evidence.json`.
The moved scan body also matches clean checkpoint `c69859e` after the module-path
relocation. This is development evidence only: the final helper-paired installed
lane must still run after the shared source freeze.
The recorded wheel and companion bytes were rechecked during the independent
review. The current scan source still matches that record, but the embedding module
has since changed for query forwarding, so those two installed tests are historical
development evidence rather than current-source installed qualification.

Existing symlink/hardlink/fallback mutation and recovery restrictions remain.
Legacy `api serve` refuses explicitly; disposable native HTTP use remains on
`catabolic-native api serve --experimental-native-http`, with its loopback gate.
No real catalogs, credentials, remote writes, releases, commits or pushes are
part of this checkpoint. No M4–M11 exit gate is declared complete here.
