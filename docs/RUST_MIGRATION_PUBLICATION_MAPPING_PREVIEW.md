# Version-two publication mapping preview

This is an ordinary M9 adapter slice from `f58624bb3431740bb0524ab237cb9df8dbb038f8`.
It does not qualify M6 or change the default engine, supported platforms, release
status, existing filesystem owners, or recovery journals.

## Implemented boundary

The preserved `projection mapping-preview` version-two route calls
`catabolic_store::publication_mappings::run_with_account`, supplying the runtime
account-identity callback for Immich bundle previews. Admission retains strict integer
version/removal-budget and boolean `allow_empty` typing, exact definition and
operation-option key sets, bounded nonempty text, supported destination/preset
membership, saved query lookup and selection-only requirement.

Folder preview uses the existing production owners:

- `Session` owns the same ordinary writer and rollback-only staging scope
- `definitions` owns saved query revisions, preset layout definitions and catalog
  projection bindings; `targets` supplies all seventeen frozen presets
- `public_layouts::call(run)` borrows that Session to evaluate the complete saved
  selection, prepare its sealed desired-mapping plan and stage it through the
  existing layout owner; display JSON is never used as an applied mapping set
- `public_reconcile::call(preview)` returns the complete read-only reconciliation
  report, including removal budget and blocked sources

The report includes the entire frozen folder plan: portable input definition,
database/profile/query identities, scope, staged layout and reconciliation, staged
projection/mapping/output state, safety and removals. Canonical encoding supplies
the reviewed plan ID. Python's zero-denominator integer removal percentage is
retained. Query revision and definition-option changes therefore change identity.
The layout's `applied: true` describes temporary SQLite staging; the outer mapping
report remains `applied: false`, matching frozen Python.

Success, staging failure, detail-limit refusal, cancellation and callback unwind
all leave cleanup with the existing Session. The adapter clears the active SQLite
interrupt callback before unconditional rollback, and never converts a cleanup
error into a report. Source and output filesystem writes, event writes and journal
writes are absent. A nested caller's preexisting uncommitted state is preserved.
Returned plans are bounded to 4 MiB and 1,000 layout/action details exactly at this
adapter boundary; complete selection evaluation is delegated before truncation
can become a success.

## Effect gates and bundle previews

All version-two apply and recovery calls fail closed with the explicit M6
publication/recovery gate. Apply refuses before database admission or any staging.
Recovery may take the existing CLI coordination lock and resolve an existing
event through the read-only history owner before refusing; it never opens a writable Store or changes journal/event state. This is
intentional early nonparity, not completion of the frozen apply/recovery contract.
Private fixture options and expected-plan strings cannot relax the gates.

Export/import previews now compose complete read-only plans through the existing
manifest, export, source-snapshot and import-preflight owners. Manifest selection
borrows the caller's Session and snapshot, then unconditionally rolls back. Every
selected source retains its full recorded occurrence and validated ctime revision.
The existing export owner's preflight was factored out of publication; previews
cannot reach mkdir, link, or artifact-write steps. Verified export history retains
full immutable specification, signature, parent/output identity and exact artifact
and link comparison. Unrelated or changed destinations are refused.

Import previews retain the full manifest and per-group subsets, transmitted-field
signatures, submitted-history deduplication, destination-parent/root identity and
Calibre/Calibre-Web shared scope. The import owner supplies source snapshots and
CLI discovery without executing the official application. Its new Immich identity
adapter performs only a bounded users/me GET, with credentials confined to the
private helper environment. The existing process owner supervises cancellation
and the 15-second overall deadline; response bodies are limited to 1 MiB, proxies
and redirects are refused, and no returned user details beyond user_id enter a
plan. URL-only historical submissions are refused before the GET. Changing the
credential owner changes scope, rather than inheriting another account's history.
The generic store entry point requires an explicit account-identity callback for
Immich; the preserved CLI supplies the runtime owner, never a fabricated identity.

Canonical plan identity removes generated_at only from the same manifest positions
as frozen Python. It retains definitions, revisions, all source/destination/account
identities, exact rendered export content and history-derived state. The outer
report remains applied=false. The document, aggregate import groups and final plan
all retain the 4 MiB admission limit.

Destructive folder/replacement/removal publication and all unqualified import or
export publication remain closed. No new recovery journal states are introduced.
V1 and V3 owners are separate. CLI integration with the parallel v1 owner requires
retaining this numeric-v2 branch and mapping its resolved-v2 recovery fallback to
`publication_mappings::PUBLICATION_GATE`.

## Evidence and limits

`tests/test_rust_publication_mapping_preview.py` compares independent frozen
Python/native processes over paired disposable stores, confirmed-missing source
metadata and empty temporary source/output directories. It compares JSON values
recursively with scalar types, canonical plan IDs, errors and exit statuses, and
checks every SQLite table and source/output contents before/after. Scenarios cover
populated and repeated previews, all seventeen presets, strict scalar/options
admission, missing query/catalog and foreign projection admission, definition and
query revision identity, item/file/association selections and paginated GraphQL,
empty-selection/allow-empty, pending-event/profile isolation, and read-only
nonfolder admission. It does not claim arbitrary JSON object insertion-order or
terminal byte-layout parity.

The original five focused native adapter tests cover nested caller rollback, a
complete 1,001-file selection's post-staging detail-limit refusal, staging-write
failure, malformed-history admission, and pre-effect gates. Four new tests cover
bundle callback failure/unwind, nested rollback, cancellation, 4 MiB refusal and
malformed import-history admission. These are narrow composition evidence, not a
fresh review of the delegated owners. Corrupt-history guards return explicit
native diagnostics rather than claiming parity with incidental Python built-in
exceptions for malformed stored JSON.

Run in the provided offline environment:

    source ../catabolic/.local-tests/rust-migration/env.sh
    export CARGO_BUILD_JOBS=1 CARGO_NET_OFFLINE=true CARGO_INCREMENTAL=0
    export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
    python scripts/qualify_publication_mapping_preview.py --report .local-tests/bundle-preview/proof.json

The runner builds only the local legacy executable and focused store test target,
pins the frozen reference commit and Python 3.14.6, fences source/harness/binary
bytes and loaded subprocess selections before and after, and refuses a passing
receipt for failures, skips, expected failures, optimized assertions or identity
drift. Evidence is local source/artifact-bound, not installed-wheel or cross-
platform qualification. Exact executed results and independent review are recorded
separately after they finish.

No broad previously platform-blocked review was retried, rephrased or delegated.
No known blocked source/publication race paths were inspected or retested. There
were no real catalogs, media outputs, remote services, new installations, pushes,
releases, default switches or changes to the M6 gate.

## Historical folder-only source-bound checkpoint

On 2026-10-12, the final runner passed against source commit
`c91355c99f7fdc5bcd39fdd7006f1bfd878851b1`:

- Nine paired frozen Python/native cases, including seventeen preset subcases
- Five new adapter unit tests
- Seven existing mapping-history unit regressions
- Two existing legacy CLI refusal regressions
- Zero failures, skips, or expected failures in that run
- `cargo fmt --all -- --check`, scoped Ruff checks, and `git diff --check` passed

The unchanged runner receipt is checked in as
`tests/parity/native-publication-mapping-preview-checkpoint.json`.
Its source fence is
`ba0ddba8323c1f83cb551734541c37b26b351c17b71d04224e0a50806542dedd`
and tested binary SHA-256 is
`1574f97e22af7cfee7a0950c6ea851378c1e5532ef80ff56c0614ce58f0b96ca`.
The receipt's own SHA-256 is
`7d3115d374841719fb11a9ce9e15bd125ebe540201ff0245fbac9d021d4f4311`.
The later checkpoint commit changes documentation/evidence only.

Independent review found one adapter admission gap: malformed later v2 history
could be skipped behind a prior event or missing scope. The adapter now validates
all decoded history, then scope fields, then matching-state fields before choosing
the first unresolved event. A focused malformed-history test covers the ordering.
The reviewer re-read the fix and reported no additional blocking source finding.

The independent frozen-binary rerun also passed all nine paired adapter cases.
A separate ordinary present-source baseline/repeat used three deterministic toy
files and paired observation metadata. Both previews were safe and complete,
remained unapplied, and matched the full frozen plans with one prepare and three
create actions. Tables, source bytes and empty output stayed unchanged. The
reviewer verified the same binary hash before and after, performed no build or
product-source change, and reported no remaining blocking finding.

Review logs are retained locally under `.local-tests/publication-preview/review/`:

- `publication-preview-independent-review.log`, SHA-256
  `e3a95939467d9237b5aad9a79b89287c5be8553a70b4fb3dd6957b7e96160bfe`
- `publication-preview-present-source-review.log`, SHA-256
  `c0099df5f1930a259343647955dccba3e8dedbc115cc84a3894a81ad40beff60`

The first review log includes a corrected review-only assertion that initially
forgot the expected prepare action; the second log records the corrected baseline
and repeat passing. It was not a product mismatch. The extra fixture ran inline
and is not counted among the checkpoint's nine automated adapter cases.

## Bundle-preview continuation

This continuation starts from 1d7b4a3 in an isolated worktree. It changes only the
new composition, related existing import/export read-only entry points, the
preserved CLI dispatch, and focused evidence. The inactive prior build cache was
moved for exclusive reuse after preserving the original CLI SHA-256 above and
store test executable SHA-256
3cb358303ebefe06236deec498e2e608df7fcf88676590b816b59cd9e8f467c1.

The paired bundle suite compares independent frozen Python/native subprocesses,
full typed JSON plans and canonical IDs, with only manifest generation timestamps
excluded from value comparison. It snapshots every SQLite table and all toy
source/output/destination contents. Ordinary toy files, manually assembled bundle
history, and a numeric-loopback identity server with fake credentials provide
coverage without invoking publication, official import tools or recovery.

Initial development evidence is retained locally. The first 12-case bundle run
had one diagnostic mismatch: frozen structured HTTP redirects map to
ConsumerError temporarily_failed. Native classification was corrected while
retaining redirect refusal. A separate cancellation test initially placed the
global --machine argument after the subcommand and failed before any request;
that test invocation was corrected. No passing receipt is issued for either run.
The first full development run passed its tests but started before the source
commit succeeded, so its receipt recorded an older commit label; that preliminary
receipt is retained separately and is not the checkpoint. The runner now requires
a clean committed tree and refuses commit drift. M6 and the default engine remain
unchanged.

## Executed bundle-preview checkpoint

Final product and harness source is commit
`e91e72056944427745b9c0fac44115b262faa0b8`. On 2026-10-12:

- The pinned Python 3.14.6 run passed 25 cases: nine existing folder cases and
  sixteen bundle/import cases, including the native-only effect/cancellation checks
- The same exact artifact passed all 25 cases against the full pinned Python
  3.11.15 oracle, with a separate source/artifact/harness-fenced replay receipt
- Nine focused native adapter tests, seven existing mapping-history regressions,
  and two existing legacy CLI gate regressions passed
- Final runs had no failures, skips or expected failures
- `cargo fmt --all -- --check`, scoped Ruff checks, and `git diff --check` passed

The product source fence is
`b5f946c39eb2a6adccce79f1a04e5fae397876e801bc5d1168c6be0d2662addb`;
the exact tested executable SHA-256 is
`a72da146bed9fffd5002b084a7e05765967478db28c45e1a4fcbe195723e7af9`.
The unchanged receipts are checked in as:

- `tests/parity/native-publication-bundle-preview-checkpoint.json`, SHA-256
  `c21d0ac7772ae3d06bb4f1211447aa0762994ef05a255f7b88d40960662ca00f`
- `tests/parity/native-publication-bundle-preview-python311-checkpoint.json`,
  SHA-256 `71c38b8ab22bcc391ff546cded6f5d8a89f9391fedefd2d875b7371467fdf7c1`

The Python 3.11 replay requires the passing 3.14 build receipt and identical source,
harness and executable bytes. It performs no build or dependency installation:

    python scripts/replay_publication_preview_python311.py \
      --build-receipt .local-tests/bundle-preview/proof.json \
      --python ../catabolic/.local-tests/rust-migration/ci-harness/python311-reference/bin/python \
      --report .local-tests/bundle-preview/proof-python311.json

Fresh independent review reported no correctness finding in the new composition.
Against the unchanged executable it passed all sixteen focused bundle/import
cases, nine additional ordinary checks, and two CLI gate regressions. Those checks
independently recomputed canonical plan/manifest/export/group hashes, exercised
immutable bundle changes and ownership, alias/account-scoped deduplication,
credential/input error precedence, all three plan-size limits, proxy bypass,
response limits and cancellation. A stalled GET timed out after 15.264 seconds.
No product edits or builds were performed by the reviewer.

The unchanged review receipt is
`tests/parity/native-publication-bundle-preview-review.json`, SHA-256
`b0abe86e1802f30ee43f868bcfa114860c140166a2457552f98d7e121deb6d8b`.
Raw review logs, scripts and before/after byte fences are retained under
`.local-tests/bundle-preview/review/`; the receipt lists their hashes. The initial
independent-check run contained two corrected review-only fixture mistakes: an
unsupported origin value and an assumption that book NFO produces sidecar files.
The original failed log is retained; neither was a product defect.

The later checkpoint commit changes only documentation and evidence. These are
local read-only composition results on this Linux environment. They do not
qualify installed wheels, alternate platforms, the delegated broad owners, M6,
full M9, publication/recovery effects or a release. No previously blocked safety
review was reopened. The complete source snapshot and all effect gates remain
owned by their existing components.
