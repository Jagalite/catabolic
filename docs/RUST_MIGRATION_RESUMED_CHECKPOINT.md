# Resumed Rust migration work log

**Current status:** [stopped again on October 13](RUST_MIGRATION_STOP_2026_10_13.md).
The chronological entries below retain their original source-bound outcomes.

Implementation resumed on 2026-10-12 from the unchanged published feature branch
`codex/rust-migration-m4-plus`, commit
`6c5e3f5bbc2fcf32b79b6b232d1673bac5b5b25d`. The [paused handoff](RUST_MIGRATION_PAUSED_HANDOFF.md)
is a historical record, not a fresh qualification receipt. Python remains the
default and all M4–M11 hard gates and prerequisite gaps remain open.

## Restored environment

The earlier cloud filesystem was no longer available. The committed repository
was restored from GitHub and verified at the exact saved commit. Earlier ignored
logs, wheels, toolchains, diagnostic scripts and local history bundles are not
available in this replacement workspace. Published source and tracked receipts
are intact; no fresh result is inferred from those historical reports.

- Remote feature HEAD remains `6c5e3f5`; main remains `c6471145`
- No open PR or intervening feature-branch edit was found during resumption
- No repository AGENTS.md or .agents/skills instructions were present
- Isolated Rust 1.99.0, exact Python 3.14.6/3.11.15, hashed Python dependencies,
  Maturin 1.13.1 and Node 22.23.3 have been restored
- Frozen oracle is again its own Git checkout at
  `453fca983222c6665a48775eef67c489c96b527e`
- Historical TypeScript client 1.2 and 1.7 generation/compilation passed during
  tooling setup; this is not end-to-end native HTTP acceptance

## Initial resumed corrections (historical scope)

1. Prepared-plan release now has a Store-independent native release handle and
   an owned PyO3 lease through Python decode/wrapper construction. Cancellation,
   callback exceptions, reentrant drops and failed delivery release capacity;
   explicit failure cleanup also handles exceptions retained by tracebacks.
   Invalid evaluation identity is checked before invoking its callback
2. SQL refusal tests now compare exact frozen diagnostics and preserve ordinary
   query errors, direct private-table denials and columnless-count distinctions.
   Production query admission and diagnostics were not changed
3. CI routing moves four exact native-prerequisite cases to a strict installed
   lane. All 1,010 original reference scenarios remain selected. Source roots are
   canonicalized consistently for macOS aliases, and repeat-journey snapshots
   wait for the independently enabled refresh queue. The evaluator is unchanged
4. CLI ownership generation now preserves the existing catalog-refresh watch
   mapping and records API worker's exact entrypoint. All eleven administration
   leaves remain missing. A non-writing `--check` detects stale/missing ledgers
5. The API-worker adapter restores `CatalogBusy` for the exact singleton-refusal
   error. No unrelated error is reclassified. Duplicate-worker plus invalid
   profile still has a documented native/frozen admission-order difference
6. Legacy refresh alone uses urllib-compatible Latin-1 credential header bytes;
   characters above 255 fail before network. Modern consumer/Plex behavior is
   unchanged. Clean truncated fixed-length responses still differ because the
   current transport conflates clean EOF and reset; response handling remains
   fail-closed pending a separate compatible design
7. Source-import bytecode is excluded from wheel payloads. Both builder and
   installed verifier reject any archive `__pycache__`, `.pyc` or `.pyo` payload;
   normal caches generated after installation remain allowed

Saved CI evidence: [run 38186046812](https://github.com/Jagalite/catabolic/actions/runs/38186046812).
Its Python wheel, HTTP Linux/macOS and Jellyfin lanes passed. The macOS 3.11
reference lane and messy-collection acceptance failed, other matrix entries were
cancelled, and release verification failed. This run did not execute the opt-in
native qualification workflow and does not qualify native platform parity.

## Fresh focused evidence

These counts apply to the recorded focused sources, not complete milestone gates.
Ignored diagnostic evidence is under `.local-tests/rust-migration/` in the
replacement workspace and has not been published as a complete release bundle.

- SQL: seven Python methods, one persistent Rust Store regression and a separate
  parent replay passed; frozen oracle and diagnostic binary hashes are recorded
  in `sql-diagnostics/`
- Fallback: 24 copied-debug cases passed separately on Python 3.14.6 and 3.11.15;
  14 MemoryError/KeyboardInterrupt fault variants retain their tracebacks and
  recover all 256 handle slots, and nine independent probes pass. Seven source
  contracts and targeted Clippy passed. Both the first correction and follow-on lease delivery received fresh
  independent review, with no unresolved lost-owner finding. A completed plan
  explicitly retained by return-hook instrumentation remains reachable ownership
- API worker/revision: 13 CLI/source cases, four presentation unit checks,
  targeted Clippy and three revision source-only checks passed. An installed
  revision diagnostic passed on the first wheel, but that artifact failed its
  packaging preflight and is **not qualified**
- Legacy refresh: 16 integration methods, nine independent edge checks, encoding
  and cancellation/reaping unit checks, targeted Clippy and lint passed. Tests
  use numeric loopback, disposable catalogs and explicitly fake credentials
- CI harness: 56 focused checks passed on both interpreters. A real disposable
  installed-Python media journey reproduced the refresh snapshot failure before
  correction and passed after; the independent retained-artifact evaluator passed
- Packaging/ledger: 42 focused harness tests passed. Independent narrow review
  found no blocking issue and also verified both facade drivers still accept
  ordinary install-generated caches with a clean archive
- CLI ledger: 242 rows comprise 209 adapted-but-unqualified, 22 missing adapters,
  ten partial and one safety-gated. These are ownership labels, not parity counts
- The completed source-stable aggregate and reference outcomes are recorded below.
  Two earlier aggregate preflights failed before scenarios ran (nested oracle
  admission, then a saved module-order rustfmt difference). Both causes are
  corrected; failed logs remain retained rather than overwritten as passes

## Failed first artifact and next cohort

`resumed-cohort-1` built successfully at unchanged crate source fence
`3a375aae98df9370d79ec9d0a9346d7255ab95335342ce4ca552495efc58cdf0`.
Its wheel SHA-256 was
`a955173ef5de7e48a2e711df057cd3d02bf9af5a64b46f525be46f2a51b1dee4`.
Installed qualification failed: source-only imports had left two `.pyc` files
that entered the wheel, and the uv installer added metadata outside the strict
pip inventory expectation. The four-ID prerequisite lane executed **zero** cases
because its preflight refused the artifact. No success is inferred from the
successful build or the separate diagnostic owner probe.

## Qualified selected installed checkpoint and current replay

Local source checkpoint `7c0cb4fc5a2168930ad0623fdfba38afef7ee186` preserves
these corrections. It is not pushed or a milestone-completion claim.

`resumed-cohort-2` successfully rebuilt with the original source caches still
present and passed clean pip-installed Python 3.14.6 and 3.11.15 gates:

- Full payload/helper qualification before and after the installed suites
- Exact four prerequisite IDs, including the frozen printable and content revision
  cases and independently reviewed owner-boundary case
- Baseline embedding: 19 cases, 82 paired groups, 69 selected inventory rows
- Persistent queries: 10 cases, 77 groups, ten rows
- Artifacts/Processing: ten cases, 74 groups, 24 rows
- Components/Policies: 13 cases, 37 groups, ten rows
- CatalogRefresh: 16 cases, 43 groups, eight rows
- Fallback: 24 cases, 27 paired groups, 15 rows and two separate publication-refusal
  witnesses; the refusals are not counted as cross-language publication parity
- Independent query/processing checks and installed Plex import, matching,
  metadata and XML owner/review suites
- Thirty release-output refusals retained the closed safety gate

Its wheel SHA-256 is
`788be0ffb2b1867cd9fe92ccf4f74f164854aab3cd21e96c126e9f4e029640ac`.
Receipts/logs are in `resumed-cohort-2/`, including the two `installed-*` folders.
The CLI, extension and scripts are hash-bound by their individual gates; selected
callable coverage is not exhaustive contract qualification.

A subsequent formatting-only module-order correction changed the source-content
fence. `resumed-cohort-3` therefore rebuilt instead of reusing the earlier wheel
as current-source evidence. Its source-before/after hash is
`5ef60906ac2b3c13177861467fa2de66c98017d9717f3be23e09ae0cbffa32b6` and wheel hash is
`4e84d8f4abe84a4b49bac76b9876464dfd7cbe93bce60228cd7dcf38cd8342a8`.
The native helper, legacy CLI and extension happen to retain exactly the previous
binary hashes, but source metadata and archive bytes differ. Fresh complete selected installed
replays passed on both Python 3.14.6 and 3.11.15. Each retained before/after
payload verification and its own receipt; success is not inferred from binary
identity. All thirty release-output refusal checks passed with the safety gate
closed.

The cohort-three disposable native HTTP/worker/generated-TypeScript journey has
completed: 49 commands, no catalog-busy retries, exact artifact hashes unchanged.
Its report explicitly leaves release qualification false, including the private
unregistered/orphaned sidecar alias blocker and remaining platform/full-contract
gates. This is numeric-loopback fixture acceptance, not live service qualification.

## Reference and aggregate outcomes at the first resumed checkpoint

The first reference replay ran 1,128 tests successfully with one expected
case-insensitive-filesystem skip. Two later harness tests were added during that
run, so it is not final-source qualification. A second replay at unchanged
`7c0cb4f` source ran 1,130 tests with **one error and one skip**. Its full source
fence before and after is
`a004fc5416c43185464fd9fd5e17cb93f2dab3676692aaad0ce76f21ec32c943`.
The error is frozen Python's `stale_or_missing_resolution_plan` in
`test_component_fallback_publication_and_noop`. That method passed in a separate
one-test diagnostic; this does not turn the failed aggregate into a pass. The
cause is still under investigation. Exact reports, traces and fences are in
`resumed-validation/reference-final-*`.

The native aggregate completed: 397 Rust checks passed, then 877 Python-native
tests produced five failure entries and two errors with no skips. There are 872
successful method outcomes; failure entries include subtests, so these counts
must not be naively added as distinct methods. Source, all five native artifacts,
the frozen oracle and historical fixtures remained unchanged. The report's
`outcome_coverage_complete` and `passed_without_skips` are both false. Models,
script release rebuild/refusals and final doc/spec stages did not run after the
failed Python gate. The separate cohort-three installed release-refusal gate did
pass; it is not substituted for those unrun aggregate stages.

The retained [checkpoint receipt](../tests/parity/native-resumed-checkpoint-7c0cb4f.json)
records hashes, exact failing IDs and selected installed receipts. Failures cover
the saved API-worker SIGINT cleanup fixture, a shallow-clone historical-object
assumption, an optional-provider test using ambient Python, a frozen component
preview/apply stale-plan rejection, and natural token expiry before a controlled
helper was observed. The latter two timing-sensitive fixtures subsequently received the bounded
test-only correction below; production refusal behavior has not been weakened.

For the API-worker fixture, a separately reviewed correction uses exact process/profile readiness and a selected frozen idle-loop boundary,
while retaining strict output, cleanup and immediate-restart assertions. It also
records a deterministic possible Python cleanup-interruption schedule without
claiming to explain the earlier historical failure. That correction and the
independently reviewed shallow-history/provider-isolation repairs were integrated
locally after this aggregate; they require a new replay.

## Reviewed local integration after the first resumed checkpoint

These additions are integrated locally after `7c0cb4f` and are **not** covered by
its failed aggregate or passing installed cohort-three receipts:

- `898ef7b`: API-worker admission/signal fixtures, local administration owners,
  public Watchers/source invalidation, four immutable import values, and the
  shallow-history/provider-isolation fixtures. Each received narrow independent
  review. Workspace Clippy passed at that integration snapshot.
- `4b70f4c` and `f0b586b`: three typed administration functions and nine local CLI
  leaves, followed by explicit business/native error provenance. Independent
  tests covered 351 SQL-error-message/owner/rollback-depth combinations on each
  Python version. The real audit-sequence exhaustion fixture reaches mutation,
  rolls back, and recovers after repair. Native SQLite exception-class parity
  remains incomplete.
- `340cd70`: reviewed test-only probe/expiry timing correction. Forty-eight
  scoped tests and separate deterministic old-failure/admission/cleanup controls
  passed. The earlier aggregates' original causes remain unproven; their failed
  receipts are unchanged. See [the timing evidence](RUST_MIGRATION_QUALIFICATION_TIMING.md).
- `44bb236` and `d06583d`: nine public Layouts/plans/PRESETS rows and the reviewed
  cleanup/type/order correction. Thirteen installed scenarios and 27 exact
  paired groups passed on both Python 3.11.15 and 3.14.6. Poisoned-scope cleanup,
  delayed option callbacks, exception identity, rollback and Store reuse passed
  independent adapter review. See [scope and artifact evidence](RUST_MIGRATION_LAYOUT_FACADES.md).
- `e2f2d61`: eight Reconciler rows; six installed cases and 25 type-aware paired
  groups, plus narrow independent lifecycle review. The author and reviewer
  checked one strict release-artifact installation; this is not a two-interpreter
  or final-integrated-source gate. Apply/recover remain unconditional pre-effect
  refusals, not successful mutation parity. See [the boundary](RUST_MIGRATION_RECONCILER_FACADE.md).

Strict installed administration, Layouts and Reconciler invocations are required
by the Linux CI workflow, including source/build/oracle inputs and receipt
uploads. Wiring is not an executed CI pass. The earlier mandatory ignored-admin
and strict Watchers wiring (`f47a63f`/`99ea547`) separately passed independent
identity/skip/failure-fence review and 49 source/harness checks; that review did
not duplicate the full installed release wheel.

Shared receipt bootstrap correction has completed independent review. Its first revision
fixed repeated malformed report options, but review found that an earlier unlink
failure could leave later receipts stale, and its filename lexer differed from
argparse. The final correction cleans every canonical destination, preserves the
first error and uses the complete parser's filename classification. Independent
normal/optimized mutation checks passed on both interpreters. Strict installed-CI
wiring also passed a separate review, including conditional/continue-on-error,
shell, source-path and receipt-upload failure controls. These are narrow harness
reviews, not an executed hosted-CI result.

Ordinary consumer-watch, identify/TMDB, command-local spec ordering, partial
maintenance and exclusive private-secret adapters have subsequently passed
scoped independent review in isolated branches. They are being consolidated for
a separate replay and are not covered by checkpoint `721b80f` below. Maintenance
retains independently reproduced full JSON/machine scan-report differences;
token create/rotate retain path and diagnostic differences. No exact-parity
claim is inferred from adapter ownership.

The administration owner tests deliberately preserve a narrow compatibility gap:
frozen Python accepts numeric/boolean token grant IDs, whose Python dictionary
key equivalence can reuse cached membership across distinct textual IDs such as
`1` and `1.0`. Native authorization continues to reject non-string grant IDs.
The disposable witness and ordinary string-ID controls are explicit refusal
and parity evidence respectively; the divergent witness is not counted as exact
parity. No cross-ID cache collision was added to native authorization.

A fresh Watchers adapter review found and corrected a cancellation-bridge gap
between borrowed query evaluation and baseline completion mutation. Targeted
inline/threaded exception-identity, rollback and reuse checks pass, including a
second independent review of the product correction. Unsupported watcher
families and destructive publication remain refused and unqualified.

## Second integrated checkpoint: `721b80f`

The [machine-readable checkpoint](../tests/parity/native-resumed-checkpoint-721b80f.json)
records separate outcomes, artifact hashes and retained report hashes. No M4–M11
or prerequisite hard gate is closed by these results.

- Full reference replay: **1,191 tests, zero failures/errors, one skip** requiring
  a case-insensitive filesystem. Before/after source snapshots are identical at
  `02ddc9b96b707297bd481db7d7978a0eec019ce03675621ffabe2963c8545740`.
  Command: `python scripts/run_reference_tests.py --report PATH` inside the
  retained source-fenced wrapper. Evidence: `resumed-validation-2/reference-*`.
- Native aggregate: **failed preflight; zero Python-native scenarios executed**.
  `scripts/check-native-mutations.sh` passed its Rust format/Clippy/build/test
  prefix and six exact mandatory ignored witnesses, then refused the reviewed
  local `tests.probe_timing` helper as non-frozen oracle bytes. The report is a
  started marker, not a passing receipt. Python scenarios, models, script release
  rebuild/refusals and final doc/spec stages were not run. The failed log remains
  in `resumed-validation-2/native-aggregate.log`; earlier `7c0cb4f` artifacts and
  report were separately hash-verified and preserved before this attempt.
- Installed cohort four: **all selected gates passed independently on Python
  3.11.15 and 3.14.6**, including the newly integrated Watchers, administration,
  Layouts, Reconciler and immutable-import gates, baseline/query/processing/
  components/refresh/fallback and selected Plex gates. Each interpreter retains
  20 successful non-attempt JSON receipts, including before/after payload checks.
  Counts overlap across gates; they must not be added into a unique parity total.
  Reconciler apply/recover remain refusal-only witnesses.
- Cohort-four wheel SHA-256:
  `7dcafdd6e753918507c65174e2e1faeaa7bd5793aae572fcfa0bf2dd582d9119`.
  Its source-content fence is
  `eed842d8c08badc1b6e8318ed526ff3d786dbb9e902ed05625f9bc439f8d1794`.
  Build provenance is source/toolchain evidence, not hermetic reproducibility.
- Thirty release-output refusal cases passed with the safety gate closed. The
  numeric-loopback HTTP/worker/generated-TypeScript journey passed 41 commands,
  zero busy retries and unchanged artifacts. Its receipt explicitly retains
  `release_qualified=false`, the private unregistered/orphaned sidecar-alias
  blocker and remaining platform/full-contract gates.

The first cohort-four installation attempt correctly failed immutable-import
isolation because its base standard library lived under the source checkout.
The gate was not weakened. Existing pinned runtimes were copied byte-for-byte
outside the checkout, and fresh isolated, local-wheel-only installations passed
the complete selected replay. Both the failed attempt and copy provenance remain
under `resumed-cohort-4/`; final receipts are in `installed-isolated-311/` and
`installed-isolated-314/`.

The independently reviewed aggregate preflight repair separately declares the two reviewed timing
fixture adaptations and their helper, while preserving frozen application and
unchanged fixture provenance. Its narrow real-media checks use the archived
`7c0cb4f` binaries and do **not** qualify current product artifacts. Its first
integrated wrapper also refused existing npm executable symlinks before starting
the test script. A separately reviewed schema-two snapshot now retains every
historical file/hash entry and additionally binds only canonical, lockfile-declared
`.bin` link identity. Unknown, escaping, chained and changed links still refuse.
Neither failed preflight is a passing aggregate.

## Third local checkpoint: `24f5f45`

The [tracked receipt](../tests/parity/native-resumed-checkpoint-24f5f45.json)
records this checkpoint separately from earlier failed runs.

The reviewed identify, consumer-watch, command-local roundtrip, maintenance and
private-secret slices are integrated. Their earlier combined source-stable
development replay passed 60 CLI methods, 20 Rust checks and 23 contract/receipt
checks. The 60 include two explicit maintenance nonparity witnesses; they are not
60 fully qualified contracts.

`resumed-cohort-5` passed all selected installed gates on Python 3.11.15 and
3.14.6, with 21 successful non-attempt receipts per interpreter and before/after
payload verification. The added private-secret gate covers four cases, 17 paired
groups and one public row. Four additional CLI methods passed against the copied
release executable. Named-user/HOME expansion, exception-envelope and broader
platform/failure-schedule gaps remain; these successes do not close those gaps.

The wheel was built in the isolated integration branch at `e6ae242` and reused
only after explicitly verifying identical crate/config source content in the
feature checkout. Its source-content SHA-256 is
`d6aae69c1c28ec4b05ac1351b48b06c1a177759c1cb32bbd0cdaf6f3b489d7f5`;
wheel SHA-256 is
`3b09cc1eb694cb5ef2f8bb5002ea75a083d6194b4d5c70c8f16d5792b8265e92`.
The adoption manifest records every copied artifact hash. This is source-content
provenance, not hermetic build reproduction.

Thirty release-output refusals passed. The numeric-loopback HTTP/generated-client
journey passed 52 commands, zero busy retries and unchanged artifacts. The command
count includes polling; it is not 52 unique contract cases. Release qualification
remains false with the same sidecar-alias and platform/full-contract blockers.

The full aggregate **failed workspace Clippy before Rust or Python test execution**:
the private-secret file placed its test module before a production item.
`resumed-validation-3/failed-clippy-receipt.json` records exit 101 and identical
whole-source fences before/after. The old live native-results marker is explicitly
not this run's result. A byte-preserving item-order correction subsequently passed
full-workspace Clippy and the four writer tests in the isolated next branch; a
new complete aggregate is still required after integration.

Further scan-watch, fallback-worker, HTTP-v3 mapping and read-only mapping-history
adapters have passed scoped independent review and are being consolidated for
their next combined replay. Scan/watch reports, automatic output refresh,
destructive recovery/publication and several diagnostics remain partial. The
returning Python CLI boundary, fail-closed fixture ASGI transport and remaining
projection orchestration are still in progress. None is covered by this receipt.

## Boundaries retained

The earlier broad definitions/curation/programs/plans/interchange/manifest/layout
review and broad filesystem/hardlink/source-access/observation/remount/watcher
safety review remain blocked. No resumption or narrower adapter review reopens
those lanes. M6 dangerous output paths remain default/release gated. No real
managed output, live service, credential persistence or release qualification is
implied. Actual macOS, external-service, performance and rollback hard gates stay
separate from ordinary Linux fixture work.

## Isolated returning Python CLI callable checkpoint

The local `codex/native-python-cli-facade` branch adds
`catabolic_native.cli.main(argv=None)` with a returning Rust-owned legacy CLI
transport and separate packaged legacy-helper provenance. Product/gate checkpoint
`8da98b2` passed strict outside-checkout paired-wheel callable and full payload
checks on Python 3.14.6 and 3.11.15. Each callable gate covers 14 exact selected
groups, 111 calls per engine and 145 native protocol children reaped. Fresh narrow
review corrected error-handler exception identity, standalone progress-error
compatibility and the complete caught error tuple before accepting this source.

See [the callable evidence and explicit gaps](RUST_MIGRATION_PYTHON_CLI_CALLABLE.md).
The Python `cli.parser()` object factory remains unadapted; internal native parser
use does not imply that contract. Oversized SQL's existing diagnostic mismatch is
a retained negative witness. The companion exec launcher, Python default, partial
CLI leaves and M6 restrictions stay unchanged. This isolated source/artifact
checkpoint is not a consolidated aggregate result or a release.

## Ordinary Publisher adapter, local development slice

The [seven-row Publisher adapter](RUST_MIGRATION_PUBLISHER_FACADE.md) now shares
the real native scan publication owner and ordinary Python scan callbacks. Eleven
copied-debug cases and 24 frozen paired groups pass on Python 3.11/3.14; these
are development-only results. Strict installed qualification is wired but awaits
the unified release build. Explicit non-generated missing entries remain a
pre-effect refusal, and mutable-buffer/raw-snapshot/full-scan-report gaps remain
open. Independent narrow review and all existing safety/platform gates remain
required. No blocked broad review was retried.

## Current consolidation and unfinished work (2026-10-12)

The next local integration branch combines the reviewed returning Python CLI,
fixture-only ASGI transport, rollback-only projection preview, ordinary Publisher
callbacks and scan-report/resume changes. These slices are not covered by the
older aggregate or installed-cohort receipts. Fresh combined artifact validation
is in progress; no M4–M11 hard gate is closed.

- Publisher review reproduced a child-directory callback losing the real native
  flush failure detail. Correction `2b268a0` preserves that error and claim cleanup;
  independent replays passed eleven development cases and 24 exact frozen groups
  on each pinned Python. The strict installed gate awaits the combined release
  wheel. See [Publisher evidence](RUST_MIGRATION_PUBLISHER_FACADE.md).
- Scan-report fields now derive from durable native rows or actual resource/header
  samples. Resume omits an adapter-created empty exclusion argument but retains
  recorded exclusions. Independent review passed 28 methods, seven runtime tests,
  twelve CLI units and eight additional controls. All five full-report diagnostic
  comparisons still fail: real batch counts, committed-byte accounting and the
  stricter native freshness floor differ. See [scan reports](RUST_MIGRATION_SCAN_REPORTS.md).
- The callable CLI's merged watch progress now uses its ordered stream transport.
  Independent callback/error/reaping checks passed. Its strict installed scenario
  set increased from fourteen to fifteen groups; the historical fourteen-group
  wheel receipt does not qualify this addition.
- Projection SIGINT fixtures now wait for the frozen selection progress callback
  after constructor/migration admission. Independent replay retained the exact
  frozen SQL-interrupted/2 versus native 130 difference. A merge omission in
  projection-preview contention error classification was corrected and checked
  against existing history/HTTP branches. The earlier combined failed run remains
  failure evidence, not a retroactive pass.
- The ASGI gate routes exactly 21 full methods on Python 3.14 and six minimal smoke
  methods on Python 3.11, with exact source/wheel/import fences and explicit pending
  aggregate prerequisites. Full Python 3.11 paired HTTP coverage remains open.
  Strict replay still needs eleven exact lock-hashed HTTP dependency archives and
  the combined release wheel. The bounded official-PyPI download approval is
  pending; no network-dependent step has been attempted while it is pending.
- Combining the ASGI deferral with the accepted aggregate fixture/npm provenance
  repairs required preserving both receipt state and all prior import/source
  fences. Ninety combined controls passed on Python 3.14 after correcting one
  stale exact marker expectation. Independent integration review is still open.

The main feature checkout remains frozen at `2c123ce` while its full native
aggregate runs. One component-media error has been recorded; a separate one-case
replay passed, which does not erase the aggregate failure or establish its cause.
The latest completed full reference replay is still the earlier 1,191-test source.

Concrete implementation work continues separately on external operation-definition
admission, bounded `rule run --worker` orchestration and v1 destination mappings.
Version-two publication mappings, production ASGI compatibility, full diagnostic
and lifecycle parity, blocked broad reviews, M6 destructive publication safety,
macOS/live-service/performance and two-way rollback qualification remain open.


## Completed combined runs and next corrections (2026-10-12, 20:30 UTC)

The source-stable main `2c123ce` native aggregate finished **failed**: 1,015
Python-native scenarios, 1,013 passed, one failure and one error, zero skips.
The preceding Rust invocations passed (455 passing executions across 53 summaries,
including six required ignored witnesses; this is not a count of unique parity
contracts). A stale shell test expected four ignored fixtures after two required
admin witnesses were added. The other error occurred in the frozen component
packaging enqueue path before native execution: `stale_or_missing_component_package_plan`.
Five later exact-artifact instrumented isolated trials passed with 90 usable real
probes and matching enqueue plans. Its original cause remains unknown. Source and
artifacts stayed unchanged; the models/release/docs script tail did not run after
failure. The failure receipt and exact debug artifacts are retained under the main
checkout's `.local-tests/rust-migration/preserved-2c123ce-m4-plus/`.

The separate `f58624b` full reference run also **failed**: 1,249 tests, twelve
failure entries, one error and one case-insensitive-filesystem skip. Its whole-
source fence remained `a6343145f0d9a82edf7d2c8bdd79564b8be147353d28e3958f8fad3bc516c55c`.
The failures were stale source contracts: eight literal CI-header expectations,
two credential-ledger subcases, a Rust test-module source-guard false positive,
and the facade exception-import inventory. Corrections retain exact pinning and
negative mutation controls. The error was the frozen profile-identity fixture's
stale-resolution-plan admission. A real delayed healthy probe demonstrated its
omitted 1000-ms default budget can change preview/apply identity; that mechanism
is not proof of the original run's cause. Its narrowly reviewed fixture adaptation
now uses the existing 10000-ms healthy-path budget, with independent default-timeout
and wrong-plan/no-publication controls. Production and frozen source semantics are
unchanged. Four final cases and 25 fixture-provenance controls pass per interpreter.

Selected **installed cohort six passed** on `f58624b`, independently of those
aggregate failures. Source-content SHA-256 is
`ab4356b699a5074937a9c005d448b4aee82fa284ecbf03694918ee897f0e386e`;
wheel SHA-256 is `78f0ba8d461a42d9cf4d10fa4fb0002c53846e7d5e3c25996c34c4c5d79aead8`;
build manifest SHA-256 is
`53cc79c548ace979314b7312af31fce0c875ecc70d891ca98a94fcc1b2c99922`.
The local wheel was installed outside the checkout with isolated/no-index/no-deps
flags. Python 3.11.15 has 24 successful selected receipts and Python 3.14.6 has 23,
including the 15-group callable CLI and eleven Publisher cases/24 paired groups.
The extra 3.11 receipt is the six-case minimal fixture-ASGI gate. Full paired ASGI
qualification still awaits the bounded dependency approval. The payload/helper
checks passed before and after. These counts are receipts, not complete features.

That same combined release CLI passed 39 selected scan-report, maintenance, watch
and projection-run methods (including explicit nonparity witnesses), 30 release
output-gate refusals, and a 37-command HTTP/worker/generated-TypeScript journey.
The HTTP report still says `release_qualified: false`. Sources, harness and tested
binaries stayed unchanged. Exact commands, logs, fences and receipts remain in the
next checkout under `.local-tests/cli-next/resumed-validation-6/`,
`resumed-cohort-6/` and `combined-f58624b/`; their results do not cover later M9
source changes. The earlier aggregate/ASGI provenance merge independently passed
review and 90 combined controls; shell-six-fixture review rejected all missing,
duplicate and wrong-admin-identity mutations on both pinned Pythons.

The next integration adds independently reviewed bounded M9 adapters:

- [V1 destination mapping](RUST_MIGRATION_DESTINATION_MAPPINGS.md): 36 paired
  methods on each pinned Python, 29 existing regressions and 27 native controls
- [V2 folder preview](RUST_MIGRATION_PUBLICATION_MAPPING_PREVIEW.md): nine paired
  cases, five new native adapter tests, seven history and two CLI gate regressions;
  rollback-only with all apply/recovery paths explicitly M6-gated
- [External operation/rule orchestration](RUST_MIGRATION_EXTERNAL_RULE.md): eighteen
  author methods plus independent additional cases, selected successful CLI
  receipts and explicit frozen -15/native 130 SIGTERM nonparity

These slices require a fresh combined replay. V2 export/import preview composition
is in progress separately. Full diagnostic and lifecycle parity, production ASGI,
private sidecar-alias protection, destructive publication safety, blocked broad
reviews, installed/platform/live-service/performance and bidirectional rollback
qualification remain open. Python stays default; no hard milestone is completed.


## Fresh reference replay after corrections (2026-10-12, 20:51 UTC)

The full source-stable `ccf130ee4fcb63cc601ccb537c1a144c2bd98229` reference replay
passed: **1,255 tests, zero failures or errors, one genuine case-insensitive-
filesystem skip**. Its whole-source fence remained
`426a5d763da8352dfb635eb38709c3ccb19d3d64640c7dc8ca1eae4fa1b3f98c`.
The wrapper elapsed time was 1,096.367 seconds. The compact
[checkpoint](../tests/parity/resumed-reference-checkpoint-7.json) binds the exact
full report/log; local evidence is `resumed-validation-7/` in the next checkout.
This supersedes the failed full-reference run only for its recorded corrected
source; it does not erase that failure or qualify native scenarios/platforms.

Narrow source-contract review found and closed test-guard holes involving trailing
Rust production items and indirect Python namespace access. The final literal
contracts passed 21 methods on each pinned Python and all supplied negative
witnesses. They are regression guards, not arbitrary-language sandboxes.

Independent M9 integration review verified owner bodies against accepted commits
and found one test-only merge error: a v2-only fixture still zipped against v1/v2
labels. The original-source failure was retained; the corrected two gate tests
pass against the identical debug legacy artifact
`7381a9f38f547656a0ed5a08128a1e337c6b1b809157bbc8b0fb4d2fff661ade`.
Four additional composition controls passed. Three all-target Clippy warnings in
the v1 adapter's tests were corrected separately. The combined native script ran from the stable corrected `c1c1efa` source.
Its all-target lint and Rust prefix passed, but Python-native qualification then
failed during import provenance admission; see the next entry. Original failure and corrected
review receipts are preserved under `resumed-validation-7/independent-integration-review/`.


## Bundle previews, scan accounting and preflight corrections (2026-10-12)

The `c1c1efa` native attempt ended with a **preflight failure**, not a passing
aggregate. Its Rust prefix contains 54 passing summary lines and 492 passing test
executions, including all six required ignored fixtures; this is not a count of
unique contracts. **Zero Python-native scenarios executed.** Exact frozen-byte
admission refused the newly reviewed `tests.scan_report_support` helper, which
had not yet been registered in the fixture adaptation manifest. The final source
fence was unchanged. The models/release/docs tail did not execute. Original logs,
source fences and failure receipt remain in the `catabolic-m9-test-clippy`
checkout's `.local-tests/combined-m9/` and `.local-tests/rust-migration/m4-plus/`.

The helper is now pinned byte-for-byte to reviewed commit `2784bd4`, using the
existing per-module adaptation mechanism without changing admission logic.
Independent review verified its hash and reproduced refusal with its entry
removed. It also caught nine duplicated discovery IDs caused by importing a
borrowed TestCase class into the new bundle module. A module alias fixes discovery
without changing any scenario body; a regression fails when the original import
is restored. All 26 provenance/discovery controls pass on both pinned Pythons.
Discovery and import checks are preflight evidence only, not native test passes.

The combined source now includes separately reviewed read-only version-two
folder/export/import previews: 25 paired cases pass on each pinned Python, with
nine native adapter tests, seven history regressions and two CLI gate controls.
See the [bundle checkpoint](../tests/parity/native-publication-bundle-preview-checkpoint.json)
and [review receipt](../tests/parity/native-publication-bundle-preview-review.json).
All version-two apply/recovery paths remain M6-gated.

Separately reviewed [ordinary scan accounting](RUST_MIGRATION_SCAN_ACCOUNTING.md)
now matches committed payload bytes and ordinary flush/batch boundaries in its
bounded paired fixtures, including the final empty flush. Full reports still
have timing-derived serialized-length differences and a stricter freshness
boundary. The five complete-report diagnostics remain failures; these corrections
do not qualify complete report parity. The [review receipt](../tests/parity/native-scan-byte-accounting-review.json)
records independent checks and their source/artifact identities.

These changes require a fresh combined runtime replay and installed wheel. The
latest full-reference pass remains the earlier `ccf130e` receipt above; cohort six
also predates these changes. Installed ASGI dependencies, platform/live-service
acceptance, M6 safety, blocked broad reviews and all milestone exit gates remain
open. Python remains the default.


## Selected signal cleanup and installed adapter gate (2026-10-12)

The independently reviewed [selected SIGTERM adapter](RUST_MIGRATION_LEGACY_SIGTERM.md)
now preserves frozen standalone rule-worker, fallback-worker and watch exit/output
behavior **after native cleanup**. Its source-bound Linux debug evidence includes
49 focused methods, 31 related regressions, seven Python 3.11 harness checks and
14 unit functions (including one internal subprocess fixture). Both harnesses use
the pinned Python 3.14.6 oracle. Repeated-signal cleanup, durable receipt retention,
interrupted lease restart, and unchanged callable 130/envelope behavior have
separate controls. Arbitrary signal windows, platforms and installed/release
artifacts remain unqualified. Frozen helper leakage is intentionally not copied.

The [strict installed adapter gate](RUST_MIGRATION_LEGACY_ADAPTER_INSTALLED_GATE.md)
now fixes 79 existing methods and requires source/build/wheel/helper/oracle fences,
exact-once execution and zero skips. Fresh wiring review found that pinning only
PYTHONPATH allowed a nested frozen child to import a cwd shadow. Explicit safe-path
and no-user-site flags now bind descendants as well; independent controls prove
unguarded shadow activation and guarded refusal. Seventeen focused and 97 related
controls pass on both Pythons. Integration also preserves module-alias discovery
without bypassing the declared fixture-import boundary. Wiring controls alone do
not qualify the 79 product scenarios; a fresh combined installed wheel and the
20-minute CI lane budget still require measurement.


The [independent bundle/scan integration review](../tests/parity/native-bundle-scan-merge-review.json)
at `8d3b8a7` passed 24 selected integration checks and 52 provenance controls across
the two Pythons. Its immutable combined debug CLI and eleven narrow source/harness
hashes stayed unchanged. That receipt predates signal integration. The original
review index and all eleven retained evidence files were copied with hash checks
to `resumed-validation-8/independent-merge-review/`; the review does not qualify the
full aggregate, installed artifacts, or either blocked broad safety-review lane.


## Failed reference replay and subsequent corrections (2026-10-12, 21:40 UTC)

The full `343d7e8` reference replay completed **1,271 tests with two failures,
one error and one case-insensitive-filesystem skip**. Source content remained
`429a843ab3b4f15769b9cba217a213f401913099a81356ed3147a8dee61f920b`;
wrapper elapsed time was 1,268.676 seconds. The
[failed checkpoint](../tests/parity/resumed-reference-checkpoint-9.json) retains
exact report/log hashes. This is not a passing latest reference result.

One ledger test still required obsolete SIGTERM-difference wording. The other
failure and error came from mock installed-gate controls observing legitimate
Python business modules preloaded by earlier reference tests. Their production
import fence correctly refused that process; the mock harness had not isolated
its synthetic installed-process state. Both issues have separately reviewed
corrections: the ledger still requires partial scope and M6 refusal, while the
mock harness restores exact original module objects and continues to reject an
actual new business import during a fixture. Runtime gate bytes are unchanged.
Six ledger controls plus ten negative mutations pass across both Pythons; the
harness fix passes 19 focused and 99 related controls per Python with real frozen
modules preloaded. These controls do not turn the failed full run into a pass.

The next isolated integration also includes separately reviewed
[mapping-event display ordering](RUST_MIGRATION_MAPPING_EVENT_ORDER.md) and
[external-worker diagnostics](RUST_MIGRATION_EXTERNAL_WORKER_DIAGNOSTICS.md).
The former preserves validated values and original container order from one Store
snapshot, with cancellable rendering and no extra query. The latter aligns two
ordinary refusal messages without changing request policy, byte limits, receipt
reducers or cleanup. Their receipts describe selected local evidence and remaining
limits. The frozen `343d7e8` native and installed runs are continuing separately;
they do not cover these later product changes.


## Installed cohort seven and reviewed next integration (2026-10-12, 22:19 UTC)

The paired release wheel from the `343d7e8` source cohort built locally with
unchanged crate-content SHA-256
`9f812ca7fc957372b7ebbfe2c761e1d3b8dd5c0dc2dc24d09a88fde80e587f19`.
Wheel SHA-256 is
`01711411dc3514ee4cd52651b10177da77b33df88f16671b5d436e1bbb6f3a2f`;
build-manifest SHA-256 is
`4aff9771773f202407b5fc49d6939067a38801c31826928c4f49a557f942cb3b`.
Fresh outside-checkout environments installed only that local wheel, with isolated,
no-index, no-dependency flags. No network dependency installation occurred.

Python 3.11.15 completed the selected sequence in **1,683 seconds**, including
**79/79 strict M9 methods**, six minimal fixture-ASGI checks and before/after
payload checks. Python 3.14.6 failed that M9 gate: **78/79 passed**, with no errors
or skips. Its first frozen recovery-contention call returned `ConsumerError`
instead of `CatalogBusy`; native recovery had not run. All postflight fences held.
The later sequence tail did not execute. Separate one-case diagnostics reproduce
mechanisms but do not establish the historical cause or replace the failed gate.
See [the exact failure and diagnostic evidence](RUST_MIGRATION_LEGACY_ADAPTER_INSTALLED_GATE.md#recovery-contention-failure-observability).

The next integration combines independently reviewed mapping-event ordering,
external-worker diagnostic alignment, ledger/mock-harness corrections and
assertion diagnostics. A narrow merge review passed 17 source controls and eight
selected runtime controls on each pinned Python against immutable debug CLI
`7e9038ab2a6378d606d1338349682abad7c68f3b187e2f433c7e65e8bed33981`.
The diagnostic-message change separately passed 20 controls per interpreter,
preserving all fixture assertions, deadlines and behavior.
[The source-bound checkpoint](../tests/parity/native-wave10-composition-checkpoint.json)
binds original receipts and hashes, including failed evidence.

The installed job budget is now exactly **45 minutes**, based on the measured
whole local sequence; this is not hosted timing qualification. The reviewed
strict inventory expands from 79 to **105 unique methods**: eleven history, six
ordering and nine diagnostic cases were added. One native-only history refusal
is explicitly nonparity. Source-only controls passed 164 methods per Python with
17 real frozen business modules preloaded and restored. The new complete
105-method product gate and combined sequence still require fresh same-source
wheel execution and timing.

The `343d7e8` full native aggregate is still running. It has reproduced two stale
legacy source-contract failures, covering the accepted supervised-worker
entrypoint and scan-body evolution. Their correction is separate test-only work;
neither the ongoing aggregate nor a focused correction is a passing full replay.
Full ASGI dependencies still await bounded download approval. M6, both denied
broad review lanes, actual platform/live-service/performance/rollback acceptance
and every milestone exit gate remain open. Python remains default.


### Narrow legacy source-contract correction

The two reproduced native source-contract failures have a reviewed test-only
correction, recorded in
[the source-contract checkpoint](../tests/parity/native-legacy-source-contracts-checkpoint.json).
The original extracted scan-body digest is retained. Exact accepted request/
resume and callback-transport hunks from `bc762bf` and `a8c5a8e` are inverted
before checking that original digest. The entrypoint guard permits only the
exact accepted native supervised-worker arm. It still rejects unrelated helper
launches and source drift. No production file is changed.

Both pinned test runners pass 16 controls, including 30 in-memory negative
variants, with independent review reproducing all results. Resource regeneration
and the original extraction identity remain checked. Both runners deliberately
use the pinned Python 3.14.6 oracle for resource generation. Original failing
outputs and the unchanged debug-CLI identity are retained; these source-only
results do not convert the old or continuing aggregate into a pass.
