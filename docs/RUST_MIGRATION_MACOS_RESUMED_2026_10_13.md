<!-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors -->
<!-- SPDX-License-Identifier: MIT -->

# Native checkpoint 14: macOS resumption and retained failed evidence

**Resumed at the user's request on October 13, 2026 UTC.** This entry records
the imported source and the first macOS qualification attempt through
`f1b2b2c5b9e61658241d8d8ff7b01c5e30135757`. The native aggregate was interrupted
after failures; this is not a passing checkpoint. A separate section records the
subsequent `f8c2759` process-cleanup and notification-fixture repairs, their review
and bounded validation. Its 28-method native CLI lane and both models pass;
its focused native replay fails, and its completed full aggregate records
491 passing executions, 89 failures, six ignored tests and one discovery failure.
The later `cd70ac7` replay completes all 53 compiled artifacts with **579 passing
tests, five failures and six ignored tests**; five available mandatory witnesses
pass separately, with OCR blocked and the Linux entry-budget witness excluded.
All 11 identity checks pass. This later aggregate also remains failed.
The subsequent test-only `5e912f1` corrections pass formatting, strict Clippy,
both focused test compilations and all four selected tests. This separate result
does not replace the failed full aggregate.
The first attempt's results remain attached to its original source and artifacts.

The [October 13 stop handoff](RUST_MIGRATION_STOP_2026_10_13.md) remains the
historical record of the earlier environment. Its stopped/publication statements
describe that boundary, not the resumed workspace. Its failed receipts, excluded
parser WIP and milestone restrictions remain unchanged.

## Imported source and publication

The supplied `catabolic-source-0063dc5-2026-10-13.zip` snapshot was imported as
`0d13caafed7790c4c7c69e203d4d6132372c3c7c`, tree
`456a540de3e16a96d696dbaee0325f80d066a730`. Archive contents were committed as an
import; the historical commits named inside the archive are not substituted for
this checkout's new commit identity.

At the **2026-10-13 01:55 UTC** read-back, `git ls-remote` reported remote feature
branch `codex/rust-migration-m4-plus` at
`492214d99ab945d75aa397445a2ad3c8e71bea76`, which includes the imported snapshot,
`f62124c`, `81563ab`, `f1b2b2c` and the subsequent test-formatting correction.
This is a point-in-time publication record, not a claim about subsequent pushes.

A later **2026-10-13 02:45 UTC** read-back verifies the same remote branch at
`cd70ac7330f45e0345999cc64d11a142fc270cb9`, including the macOS fixture corrections
described below. Publication succeeded for that source. Its new Rust validation
has passed formatting, strict Clippy and test compilation. Direct execution of
the compiled tests completed with five failures; no passing checkpoint is claimed.

[GitHub CI run 38332824573](https://github.com/Jagalite/catabolic/actions/runs/38332824573)
completed with **failure** for the earlier published `f62124c` source. Ubuntu Python
3.14 completed its reference tests and lint, then failed `ruff format --check`;
the other three test-matrix jobs were cancelled. Wheel, both Linux/macOS
acceptance jobs, both HTTP jobs and Jellyfin passed. Release verification failed
its requirement that every validation lane succeed. These are results for the
existing distribution workflows; they do not qualify the later local native
artifacts or establish a standalone native release.

`492214d` subsequently reformats only `tests/test_rust_legacy_review.py` and
`tests/test_rust_notifications.py` with pinned Ruff **0.16.6**. Scoped lint and
format checks pass, and the full `src tests scripts` format check reports
**527 files already formatted**. The retained `ruff-format-repair.log` records
those commands. This correction does not change the failed hosted run's result
or establish a passing replacement CI run.

[GitHub CI run 38336843434](https://github.com/Jagalite/catabolic/actions/runs/38336843434)
subsequently completed with **failure** for `f8c2759`. Its macOS Python 3.11
reference lane ran **1,276 tests in 649.250 seconds**, reporting **23 failures,
eight errors and 95 skips**. The other three reference lanes were cancelled;
wheel, Jellyfin, both HTTP jobs and both acceptance jobs passed. Release
verification failed the aggregate requirement. Comparison with the earlier
`492214d` run `38334331581`, normalizing only ephemeral temporary paths, found
the same 31 failing scenario/subtest identities and the same known path/receipt
diagnostics. Evidence remains in `ci-triage-38336843434/`; this comparison does
not erase either failed run or qualify their skipped/cancelled cases.

The replacement [CI run 38338151813](https://github.com/Jagalite/catabolic/actions/runs/38338151813)
for published `cd70ac7` was **still running at 03:12:48 UTC**. Nine jobs had
passed: both macOS reference lanes, Ubuntu Python 3.14, wheel, Jellyfin, both
HTTP jobs and both acceptance jobs. Ubuntu Python 3.11 was still executing its
reference tests, and release verification had no observed result. The three
completed reference reports each discovered 1,278 tests: both macOS lanes had
95 skips and Ubuntu Python 3.14 had 93 skips, with no failures or errors.
These are existing reference/distribution checks, not native qualification.
The point-in-time receipt is
`ci-monitor-38338151813-20261013T025747.556078Z/final-observed-job-summary.json`,
SHA-256 `63e2e763a5a5f608246e613ef7b96924e87e890497e08ea9da047dbea0791b65`.
That receipt remains the historical pending observation. The same hosted run
subsequently completed with **success** for exact source
`cd70ac7330f45e0345999cc64d11a142fc270cb9`: **all 11 jobs passed**, with no failed,
cancelled or skipped jobs. Ubuntu Python 3.11 completed at **03:17:00 UTC** and
release verification at **03:17:05 UTC**. Each of the four artifact reports
verifies that exact source in its metadata and records **1,278 unique methods,
zero failures and zero errors**. Each macOS lane skips 95 tests and executes
1,183 non-skipped tests; each Ubuntu lane skips 93 and executes 1,185. Every
reference report separately defers **115 native files and four native
prerequisite scenarios**. Passing jobs do not convert those cases into passes.

The final receipt is
`ci-final-38338151813-20261013T032142.845068Z/final-summary.json`, SHA-256
`08e2fd41268e73945fb2c34e61389c270e6761ac42ed4f5d4341febd14dfea7c`;
full jobs, reports and artifact metadata are retained alongside it. This
successful existing reference/distribution run does not qualify the failed
local native aggregate or the later test-only commit.

## Bounded repairs before the native attempt

- `f62124c` moves the standalone workflow's target-directory assignment into
  its runner step using `GITHUB_ENV`, retains a negative contract for dropping
  that assignment, and resolves macOS temporary paths physically in the shell
  fixture. Receipt cleanup controls accept both directory-unlink error types
  while verifying preservation of the first error and protected files.
- `81563abc21352827a26b5e4f05758459e760c52e` widens filesystem block counts and
  fragment sizes before saturated multiplication, and compares filesystem mode
  values at a common width. It adds a counter-width regression. This addresses
  actual macOS compilation errors; it is not full filesystem qualification.
- `f1b2b2c` retains the non-Linux native API worker refusal without unreachable
  code or unused-variable warnings, and adds a regression verifying refusal
  before catalog or supervisor creation. It does not enable that worker on macOS.

The local source-contract/HTTP-fixture run first reported **23 methods with one
failure** because the 1.7 generated client was absent. After generating and
typechecking that client, the same selected run reported **23 passing methods**.
Both `source-contracts.log` and `source-contracts-client-built.log` are retained;
the first failure is not rewritten as success.

## Kyoto build and lint evidence

All Rust compilation in this resumption was submitted through
`/Volumes/seed1/Projects/kyoto/kyoto.py`, using the exclusively assigned target
directory `/Volumes/seed1/Projects/catabolic-builds/native-checkpoint14`.
The observed toolchain is Homebrew Rust/Cargo **1.99.0**, target
`aarch64-apple-darwin`, rustc commit `b940084d7eb6a299eb4bfeb8e34901bc051e7ac4`.
Native commands exclude `catabolic-python`.

| Kyoto job | Result and scope |
|---|---|
| `b661423e75b74bf6959e744d17d3be41` | Initial native test compilation cancelled; no completed build receipt |
| `204ab52fe87747b188b81aab4f899036` | Replacement native test compilation cancelled, exit -15 |
| `c699df298fdc4fbfae2e670ff3b81bf5` | Strict Clippy failed, exit 101, on platform-dependent filesystem integer widths |
| `80da39933e3c4cb881a2e42bb45e9a96` | Strict Clippy failed, exit 101, on the non-Linux worker branch's unreachable code and unused variables |
| `08c19c64bfb24c81b02faf449461c68e` | Formatting check passed at the worker-fix snapshot |
| `05e3d22a5f36438eafd59f0320a1434e` | Strict all-target native-workspace Clippy passed at the worker-fix snapshot |
| `d7a451caafc044328e235eaf8612e55d` | Native all-target `cargo test --no-run` compilation passed; Cargo JSON identified 53 executable test artifacts |
| `66c9d849010c4e37b9d06876631ddaf5` | Separate binary/example build cancelled, exit -15, after pressure-related deferrals; not a completed standalone build |
| `5417bcac87d34fe5a677760221e9a57b` | Locked dependency fetch passed; dependency availability is not runtime qualification |

Earlier formatting jobs `d1df490c2c2246abacdd40eae07e4d3e` and
`fd451ff51bc84bf08647f255ab353998` also passed for their respective earlier
snapshots. The failed and cancelled job logs remain in the Kyoto state directory;
later successful commands do not change their outcomes.

## Interrupted native execution

The local runner consumes the successful Kyoto Cargo JSON, then directly invokes
each test executable from its package directory with one test thread. It does
not invoke Cargo to build or run tests. The separate supplemental-notices witness
may use its existing read-only offline Cargo metadata command.

The `f1b2b2c` aggregate attempted **10 of 53 test artifacts**, leaving **43
unstarted**. Its completed harness summaries contain **67 passing executions,
six failures and one ignored test**, broken down as follows:

| Harness | Completed result |
|---|---|
| Legacy CLI units | 22 passed |
| Core units | 25 passed |
| Distribution units | 17 passed, one ignored supplemental-notices witness |
| HTTP library units | Two passed |
| Artifact publication integration | One passed, six failed |
| Companion CLI, parser example and HTTP executable harnesses | Zero tests each; not counted as passing contracts |

The notification-provider harness produced no terminal test summary. Its partial
output contains one `ok` and four `FAILED` lines before the deadline fixture
stalled in `server.join()` while its listener waited in `accept()`. The owned
test process group was terminated with SIGTERM at 01:49:00 UTC; its recorded exit
is -15. Three separate exact notification diagnostics also exited 101. Those
diagnostics reproduce failures and do not replace the failed aggregate.

Artifact publication reported six failures, including repeated
`cannot reap command: Operation not permitted (os error 1)` errors. The runtime
library then emitted 52 `ok`, five `FAILED` and two `ignored` lines before the
aggregate was deliberately interrupted at **01:50:41 UTC** to repair the
reproduced cleanup defect. That partial library output has no terminal test
summary. Its process exited -15; the aggregate receipt records **interrupted,
exit 130**. Partial progress lines are not added to the completed-summary count
or treated as a passing library suite.

All 53 recorded test-artifact hashes and the Cargo JSON log hash stayed unchanged
through this attempt. This runner receipt does not establish an independent
whole-source/oracle/fixture/client postflight fence. It covers no Rust doc-tests.
The fresh mandatory-witness lane, lifecycle/playback models, complete CLI
comparison, coordinated native HTTP/client journey and standalone candidate
qualification were not completed by this attempt.

## Reviewed cleanup repair at f8c2759: bounded results

Commit `f8c27596ccbc52476a61c613f79ec5b9b0eb26e5`, tree
`713ea77ba8e80baf57e8e3055dee4c02fb3fe453`, changes only the native process runner
and notification-provider fixture. Its recorded source inventory is
`source-f8c2759-before.json`, SHA-256
`de4511bffb8b405bee974b95aa2cba3ce5c0554fbbcafd5b3e7b123dfa20a153`.
Independent review found no blocking issue in the cleanup proof or fixture
repair. The new cleanup regressions and artifact-publication integration pass
in the focused replay below; other fixture failures remain explicit.

The observed Darwin error has a specific kernel explanation: the process-group
signal path filters zombies, then can return `EPERM` when no eligible member was
found. See [Apple XNU killpg implementation](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/kern/kern_sig.c#L1612-L1621).
The repair accepts this error only on macOS, after observing that the owned
leader has exited, and only when a group-membership snapshot contains exactly
that leader. The snapshot covers live and zombie lists under the process-list
lock and returns copied bytes; a two-PID buffer makes a full or truncated result
fail the exact-one-PID condition. Query errors, zero bytes, malformed sizes,
another PID and additional members are rejected. See [Apple XNU process listing](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/kern/proc_info.c#L337-L457)
and [group-membership locking](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/kern/kern_proc.c#L4140-L4151).

`WNOWAIT` keeps the owned leader unreaped through signalling and the snapshot,
reserving its PID/PGID. A real signalling failure still returns the original
error. An already-exited leader is reaped on that failure path; a live process
whose termination was denied is not waited on indefinitely. Marking the owner
retired after reaping prevents a later cleanup attempt from signalling a reused
group identity. The existing `ESRCH` handling is retained.

Five process regressions were added for successful exited-leader cleanup,
exact snapshot acceptance, live-group permission denial, reaping after another
signalling failure, and rejection with a surviving descendant. A separate
notification-fixture self-test requires an absent client to time out. All six
previously blocking fixture accepts now have a three-second deadline; accepted
streams return to blocking mode for the existing read deadlines. The existing
200ms outer-deadline scenario, required request and socket-close checks, and
no-extra-fanout assertions are unchanged. The five new process regressions and
absent-client fixture self-test subsequently passed in the focused replay; the
full containing suites did not all pass.

A disposable OS-level reproduction on macOS **26.5.2 arm64** at
**2026-10-13 01:55:35 UTC** exercised the underlying APIs separately from the
Rust repair:

| Owned group | Two-slot snapshot | Actual `killpg(PGID, 0)` / `killpg(PGID, SIGKILL)` |
|---|---|---|
| Sole exited leader | Four bytes, exact leader PID | Both returned `EPERM` |
| Live leader | Four bytes, exact leader PID | Both succeeded |
| Exited leader with live descendant | Eight bytes, both PIDs | Both succeeded |

All three cases recorded leader reaping, EOF on the owned liveness pipe, and
`ECHILD` on a second wait. The receipt is `darwin-killpg-reproduction.json`,
SHA-256 `82a37485f35562450e9342f06eac4aaf9467b024673f643adfee1d45b1cf5b7e`.
It establishes the local OS behavior; it does not execute the Rust regressions.

The new source has separate Kyoto submissions using the same exclusive target
directory and native-only package scope. All four completed successfully:

| Kyoto job | Result and scope |
|---|---|
| `c4327195834c4808b925b1b19559df00` | Formatting check passed, exit 0 |
| `39563455a34344fe8b4cb128faef92d2` | Strict all-target native-workspace Clippy passed, exit 0 |
| `c7e9c232d9714415a480c109aa36e3cb` | Native all-target test compilation passed, exit 0 |
| `9353308ca276472380f66e6ccb4fa1c6` | Native binary/example build passed, exit 0; this debug build is not a standalone release candidate |

### Native CLI and lifecycle/playback models

The separately fenced native CLI/model run completed at **02:24:11 UTC** on
`f8c2759`. All **28 selected GraphQL differential methods** executed exactly once
with **zero failures, errors or skips**. They use the standalone legacy and
companion binaries against the frozen oracle, including preserved explicit
known-difference controls. No extension, installed wrapper or HTTP-worker
method was selected. This is bounded CLI acceptance, not complete GraphQL parity.

Both model commands passed: lifecycle exhausted **52 states and 364 transitions**;
playback exhausted **five states and 1,200 transitions**. Both report zero skipped
checks. The run copied five exact normal executables out of the build cache to
the seed1 evidence directory. All before/after source, copied-artifact, oracle,
historical-fixture, generated-client, interpreter, runner/helper and Cargo-log
fences held; all 372 native input hashes matched the supplied prebuild snapshot.
The content fence is not a hermetic-build attestation.

Evidence is
`/Volumes/seed1/Projects/catabolic-builds/evidence/native-cli-models-f8c2759/`.
Its `result.json` SHA-256 is
`2b934188dbc3bb0964b478390fe6c26fe0b2118ffbd378ad414615c9e8928cad`;
the unchanged full source-input digest is
`434ee0021e9e514f2e5f623981f4ac7c528c6c200d88923ea7c3fcc0cc5ddefe`.
Per-method outcomes, complete selected case receipts and both model outputs are
retained there.

### Focused failures and unchanged-artifact path diagnostics

`focused-f8c2759/result.json` records **38 passing executions and six failures**,
with unchanged identities. The selected process tests report 26 passed and two
failed; notification providers report five passed and four failed; all seven
artifact-publication tests pass. This closes the first attempt's demonstrated
cleanup failure only for these focused cases and artifacts. It does not turn
the focused run or either aggregate into a pass. Receipt SHA-256 is
`2b6bb34dccc7ee5156de4b2269c38d5f7b16fcccb436efd13c02587ecd0bfea5`.

The remaining process failures are the inherited cancellation and timeout
fixtures, which can expire before the shell publishes its PID. Notification
failures cover the bounded provider protocol, explicit helper failure, outer
deadline and executable-replacement cases. Local diagnostic work also found
Linux-specific `/bin/true` and `/bin/false` assumptions, startup-sensitive
fixture budgets, and accepted sockets inheriting nonblocking mode on macOS.
Corrections require their own source review and fresh replay; they are not
included in the passing `f8c2759` CLI/model result.

A separate replay changes only `TMPDIR` to a physical, owned path beneath
`/private/var/...`, avoiding the macOS `/var` symlink alias. With the same source
and test executables, **28 of 28 tests** pass: fallback consumers 11, media
workflows eight, execution ledger four and maintenance orchestration five.
All recorded source, executable and Cargo-log identities hold. Evidence is
`physical-temp-replay-f8c2759-20261013T022955.681284Z/result.json`, SHA-256
`73f85ea75fa4272a856bf2c63881e434cd1b7c4db3858f93225b98f35f9fac76`.
This is environment diagnosis of the original failures, not a source fix or a
replacement aggregate receipt.

The related `runtime-canonical-tmpdir-f8c2759-20261013T022814.493073Z` diagnostic
passes all **12 selected tests**: six processor and six public-processing tests.
Its runtime executable hash stays unchanged; its receipt SHA-256 is
`465fc2aa1cc79b6e47ce859300939db1e31fddfdce1cfc0d0e5e45a542df8d87`.
This smaller diagnostic records an executable fence, not the full fence set of
the 28-test replay. Both diagnostics preserve their original failures and leave
production symlink refusal intact.

### Completed full f8c2759 aggregate

The full `native-suite-f8c2759` run completed with **failed status, exit 1**.
It attempted discovery for all **53 artifacts** and executed **52 harnesses**.
Complete execution summaries contain **491 passed, 89 failed and six ignored**
tests. No test-execution command timed out. The separate discovery command for
`public_read_budget_review` timed out after its 60-second allowance, with empty
stdout/stderr and exit -15; that harness's tests were not executed by this run.

The aggregate receipt is `native-suite-f8c2759/result.json`, SHA-256
`4218338048995a4b57ff4ac562c7ccf54446fe0f7b19e37b18c3d39db6278d2d`.
All 53 recorded artifact hashes and the Cargo log hash stayed unchanged.
The separate `native-suite-f8c2759-source-fence.json` confirms the expected
`f8c2759` HEAD and no changes among **1,179 build/test input files**; its SHA-256
is `0dea7a9c9b357d28bcbc6fa383d69db571b0f89de7bec28c7c096d74a6da60ba`.
This source fence has its recorded scope, rather than the wider oracle/client
fence set of the CLI/model run.

One bounded replay of the unchanged `public_read_budget_review --list` command
subsequently completed in **0.302 seconds**, listing four unique tests. Source,
executable and Cargo-log identities held, and the child environment was
unchanged. Its external receipt is under the owned physical temporary directory
`catabolic-public-read-discovery-unkoloau/result.json`. A separate launch sample,
`query-discovery-launch-sample.txt`, captured `query_document_validation` entirely
at `_dyld_start`; it does not sample the public-read executable. These observations
support a transient host-launch problem. The retry did not execute the four tests
or replace the aggregate's discovery failure.

### Fixture corrections after the completed aggregate

The next source changes address demonstrated fixture and environment failures:

- Process cancellation waits for a newline-terminated PID file from the owned
  shell before setting cancellation. It retains the two-second command wall
  assertion, three-second readiness deadline and `ECHILD` reaping assertion.
  The timeout fixture allows one second for startup while retaining its
  three-second wall bound; executable controls use `/usr/bin/true`.
- The notification deadline fixture allows one second for pre-exec descriptor
  setup, retaining its two-second command wall bound, timeout result, empty
  diagnostics, actual request and owned-socket EOF checks. Failure and replacement
  controls use `/usr/bin/false`, which exists on both tested host families.
- The destination-mapping fixture restores accepted sockets to blocking mode
  and sets five-second read and write timeouts. Its existing protocol and
  captured-request assertions remain in place.
- The standalone runner supplies an owned physical `$run/tmp` to its children
  and records the path. Shell contracts begin with a symlinked incoming temp path,
  check child inheritance, and require a runner mutation without the export to
  fail. The same eight provider witnesses remain mandatory.
- The normal Python CI reference-test step creates a fresh physical directory
  under `RUNNER_TEMP` and sets `TMPDIR` only for that test command. An unchanged
  external-temp replay of eight implicated modules cleared 22 of the 31 original
  failure/error outcomes; its receipt is
  `ci-triage-38334331581/python-physical-external-kwy1_ihx/result.json`.
- Python cleanup controls accept precisely Linux `EISDIR` or Darwin `EPERM`,
  while new controls preserve the first error and continue cleanup of later
  requested receipts. Legacy fixture discovery checks its explicit refusal when
  the mandatory Linux process-tree witness is unavailable; that witness is not
  silently omitted or counted as executed.

These corrections are committed and published as
`cd70ac7330f45e0345999cc64d11a142fc270cb9`. Independent review found no blocking
issue in the Rust fixture changes. The applied shell suite passes **six workflow
mutation contracts and 25 runner cases**, including the missing-export mutation.
Actionlint **1.7.12** passes both affected CI workflows with exit zero and empty
diagnostics. Logs are `applied-standalone-contract.stdout` and
`applied-workflow-actionlint.stdout`; their combined receipt is
`applied-fixture-wiring-validation.json`, SHA-256
`ff28c669375019f2c661ff1bafefa6f8f25ff9c175738bc54c09d8155a8c85c2`.
These checks invoke fake shell tools or inspect YAML; they do not compile Rust
or qualify the corrected source's native runtime.

The applied Python harness controls pass **83 of 83 methods on Python 3.14.6**
and **45 of 45 on Python 3.11.15**, with zero failures, errors or skips. The
3.14 lane covers eight named modules; the 3.11 lane covers the three changed
portability modules. Before/after selected input hashes hold. These executions
preceded the commit while the reviewed changes were applied over `f8c2759`;
the receipt retains that original HEAD and dirty-worktree identity. It is
`applied-python-portability-hfv18wq6/result.json`, SHA-256
`acbdf24742be4a171620df0c6c0dd8e618b310b4dd094165470b0ab302c2210a`.
Full Ruff lint passes, and the full format check reports **527 files already
formatted**. These are harness/source checks, not installed-wrapper or native
product qualification.

### Corrected-source compilation passed; runtime aggregate failed

At the **02:45 UTC** observation, Kyoto reported **OS memory pressure: warning**
and admitted zero concurrent jobs. Admission subsequently resumed. The source freeze is
`source-cd70ac7-before.json`, SHA-256
`2935515e47729bc2792dd9df51a04e899f4bde35103ec944815103bb3e9f56d4`.
The current command results are:

| Kyoto job | State and scope |
|---|---|
| `1874085ce65f4c3cb8cb79dfed38b440` | Formatting check passed, exit 0, 1.59 seconds |
| `aeb8ee698cac4272972983dcc555279d` | Strict all-target native-workspace Clippy passed, exit 0, 40.65 seconds |
| `33c85b24b48b41dfb12ad920c1d65cc5` | Locked offline native all-target test compilation with `--no-run` passed, exit 0; 53 executable test artifacts; final attempt 247.70 seconds after one memory-pressure deferral |

The completed jobs are retained in `kyoto-cd70ac7-jobs-final.json`, SHA-256
`2518437df8dffc162109be6fb9886288cd5a82fe70b917ccf592849afc551a1e`.

The earlier mutating formatter job `8e0b879dd0ee43c0ac2e1d947353ab97` was
cancelled **before it started**. It produced no formatting changes. Its cancelled
status is retained alongside the replacement check-only submission in
the earlier `kyoto-cd70ac7-jobs.json` queue snapshot. The final receipt above
records the successful replacement check and compilation.

The direct-execution replay completed through
`.local-tests/native-checkpoint14-macos/run_native_postrepair.py`, using a
physical, owned internal `TMPDIR` only for its children. Its evidence directory is
`/Volumes/seed1/Projects/catabolic-builds/evidence/native-postrepair-cd70ac7`.
The wrapper accepted the successful Kyoto compilation and matched **1,159**
tracked build/test inputs to the prebuild manifest. All **53 artifacts** completed
discovery and execution. Their summaries total **579 passing tests, five failures
and six ignored tests**. There was no discovery failure, aggregate timeout or
interruption. The suite and wrapper return exit 1; the failed aggregate remains
failed.

The five failures are retained with their original logs:

| Case | Observed failure and remaining focus |
|---|---|
| `both_native_executables_dispatch_bounded_provider_protocol` | Notification fixture did not receive the expected connection within its startup bounds |
| `cancellation_kills_stalled_http_and_prevents_later_fanout` | Pre-connection startup and elapsed-time assertion failed; the separate unchanged-artifact diagnostic below passes, without replacing this failure |
| `sleeping_child_times_out_and_is_reaped_before_slot_reuse` | The 200 ms timeout expired before the child PID file was available to the test |
| `every_available_fixed_preset_validates_actual_output_and_preserves_source` | Required `image-webp` preset unavailable because FFmpeg lacks `encoders:libwebp` |
| `observation::tests::regex_strict_end_backtracking_budget_and_admission_bounds` | The simple strict-anchor assertion received `regex_filter_timeout` under the production 10 ms wall-clock budget |

All **seven mandatory witness statuses** were recorded separately from the
ordinary suite's totals:

| Mandatory witness | Result |
|---|---|
| Real PDF text extraction and page overflow | Passed |
| Python/native publication intents recover in both directions | Passed against the frozen oracle |
| Frozen access-owner differential | Passed |
| Inherited scalar-cache confusion remains fail-closed | Passed |
| Stage five notices from the existing offline cache | Passed |
| Real single-image OCR | Blocked: Tesseract unavailable |
| Entry-budget exhaustion cannot restart for later sources | Excluded on macOS: Linux-only witness |

The wrapper finished at **03:13:02 UTC**, with all **11** before/after fences true:
HEAD, selected source files, executable artifacts, Cargo log, fixtures, oracle,
oracle interpreter, prebuild manifest, qualification helper, test runner and
wrapper. Its terminal `result.json` SHA-256 is
`169079f7adaa2593011f0690d12c759781a9b07d1105d131735f661275a33626`.
The source fence excludes ignored dependencies such as `node_modules`; this
Rust-only replay does not execute generated clients. Five passed witnesses do
not qualify the two unavailable witnesses or close the failed aggregate.

### Separate unchanged-artifact diagnostic

The four-case diagnostic completed against the same `cd70ac7` source and
executables, with all source and artifact identity fences unchanged:

| Case | Diagnostic result |
|---|---|
| Both native executables dispatch the bounded provider protocol | Passed, 1.405 seconds |
| HTTP cancellation kills the stalled helper and prevents later fanout | Passed, 0.468 seconds |
| Sleeping fallback child times out and is reaped | Failed again, 1.739 seconds: the original 200 ms fixture still had no PID file (`ENOENT`) |
| Strict regex anchor, backtracking budget and admission bounds | Passed, 0.142 seconds |

This is a single bounded diagnostic, not a replacement aggregate. None of the
four cases required sampling or hit its outer timeout. The receipt is
`/Volumes/seed1/Projects/catabolic-builds/evidence/native-postrepair-cd70ac7/diagnostic-once-2rpa78ec/result.json`,
SHA-256 `119216f8b905939c361e7aed4e413e87e00acd3dd46e838b474d7743383860ef`.
The original five failures remain unchanged in the full-suite receipt.

### Test-only follow-up and focused qualification passed

Commit `5e912f11b06ead8d53e3f3aa5b6a858158c47675` changes two tests:

- The strict-anchor assertions use the regex constructed by `ScanFilter::new`
  directly, separating production anchor translation from scheduling under the
  10 ms wall-clock budget. The pathological pattern still goes through
  `ScanFilter::matches` and must return exactly `regex_filter_timeout`; all three
  admission-rejection checks remain. Production regex and timeout limits are
  unchanged.
- The sleeping fallback fixture gives its shell child **1,000 ms** rather than
  **200 ms** to reach timeout. It still requires exactly `probe_timeout`, a total
  wall time below three seconds, a dead child PID and zero occupied supervisor
  slots. Production timeout behavior is unchanged.

The new prebuild source freeze is `source-5e912f1-before.json`, SHA-256
`63e1216f7b86adab4b603d839eda7e4179e862667bf61881d36c0a1a601ee2a9`.
Its Kyoto jobs are:

| Kyoto job | Observed state and scope |
|---|---|
| `d3a18995c9cb49309c07be4cc7e421de` | Formatting check passed, exit 0, 1.39 seconds |
| `02bef7131b9b43eeb9726d6d04649ec6` | Strict all-target native Clippy passed, exit 0; final attempt 145.33 seconds after two memory-pressure deferrals |
| `7d74ec4eb5184d96852ce77a18cdd9a6` | Store library test compilation passed, locked/offline `--no-run`, exit 0; final attempt 155.61 seconds after one deferral |
| `98f382eda6af4329a0f0315a9ff407b7` | Fallback-probe test compilation passed, locked/offline `--no-run`, exit 0; final attempt 116.31 seconds after three deferrals |

After all four Kyoto jobs succeeded, the local runner directly executed the
exact regex case and all three fallback-probe cases. **All four tests passed**,
with zero failures, ignored tests or outer timeouts. The regex harness took
54.653 seconds including a long silent startup; the fallback harness took
4.828 seconds. Each process had a 120-second outer bound. These results do not
qualify cold-start latency. The fallback test retained its own strict
three-second operation bound and child-reaping assertions.

The run used a physical owned `TMPDIR` outside the checkout, with all before/after source,
executable, log, helper and manifest checks unchanged. The focused package
builds use a different Cargo feature graph from the earlier full-workspace
build; their exact executable identities are recorded in Cargo JSON and the
receipt. These are four focused tests, not a rerun of the full native aggregate.
The selected final job receipt is `kyoto-5e912f1-jobs-final.json`, SHA-256
`67919ef0923de01bc58ec3d5dd7df751efcf0496d7ef6a1f2a5f7932b96bcfca`.
The focused result is `focused-5e912f1-tyjbr36c/result.json`, SHA-256
`89d52b43bac34c3dc0b53568b21b3cb1a8df12e661d6ca72e3301117cf5f69fe`.
The completed CLI/model evidence remains bound to `f8c2759`; every original
interrupted or failed receipt is preserved. Hosted CI success above is for
`cd70ac7`; it does not establish hosted results for this later source.

## Environment and platform boundaries

The frozen oracle source is `453fca983222c6665a48775eef67c489c96b527e`, located at
`/Volumes/seed1/Projects/catabolic-builds/reference-453fca9/src`. Its checked
inventory SHA-256 is
`81b697a595556ded943bef59c167314a6ffbbfc68706ed134cfea66afe012cc2`.
The local oracle interpreter reports Python **3.14.6** and graphql-core
**3.2.12**, but its Expat is **2.7.4**. The Plex encoding generator requires
Expat **2.8.2**; its environment gate remains unsatisfied. Do not regenerate or
weaken the pinned reference to hide this mismatch.

Tesseract is absent, so the mandatory real OCR witness is blocked. The entire
`source_events_independent` target, including its mandatory entry-budget witness,
is Linux-only; macOS cannot supply that witness. A zero-test target or platform
exclusion is not successful execution of the required case.

The available FFmpeg lacks the **`libwebp` encoder** required by the image-WebP
acceptance fixture. The native render-boundary test reports the missing
capability explicitly. That environment does not qualify the full image preset
matrix; omitting the case or substituting another encoder would not establish
the original fixture's acceptance.

The native API worker still refuses non-Linux execution because parent-death
supervision is unqualified there. Its complete worker-dependent HTTP/client
journey therefore remains unavailable on this macOS host. Passing local HTTP
library units or the existing distribution's hosted HTTP jobs does not remove
that native platform gate. The native playback worker retains its corresponding
non-Linux parent-death-supervision refusal as well.

## Evidence and next checkpoint

Local evidence is under `.local-tests/native-checkpoint14-macos/`. The immutable
interrupted aggregate receipt is `native-suite-f1b2b2c/result.json`, SHA-256
`c67f6342e4954f138500216fa599d9b67fb5a5bdfafb199eb9b8b95a171e1adc`.
Its Cargo JSON log SHA-256 is
`c4acdfd9572dc8e01bfe0cb032f19a635f9f9b6dc2116ad119ebffa2646f721e`.
Per-harness stdout/stderr, interruption records, diagnostic runs and source,
oracle, historical-fixture and generated-client inventories remain alongside it.
These ignored local artifacts are not claimed to be files in the pushed tree.

Next, resolve the missing WebP capability and remaining aggregate qualification
with separate evidence. Preserve process reaping, exact failure
propagation, bounded production deadlines, regex limits and production symlink
rejection, along with both failed full-aggregate receipts.

Freeze any newly corrected source before a fresh combined native replay with explicit
before/after source, oracle, fixture, client and executable fences. Route
compilation through Kyoto and retain failed attempts. Complete available exact
witnesses while recording the missing OCR/WebP capability, pinned Expat and
Linux-only lanes separately. Build and review a same-source standalone candidate
separately; the distribution preparer itself invokes a build and must also
respect Kyoto routing. The completed bounded CLI/model lane remains tied to its
own unchanged `f8c2759` inputs until replayed for a later source.

Thin-wrapper work remains last. The excluded parser WIP is not integrated by
this resumption. The previously blocked broad reviews, M6 safety, production
HTTP/content protection, real-service, resource/performance and rollback gates
remain open. No M0–M11 hard gate is closed, no native default is enabled and no
Python implementation is retired by these results.
