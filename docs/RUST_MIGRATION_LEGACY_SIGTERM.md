# Selected legacy CLI SIGTERM boundary

This is a Linux development checkpoint from `b76e602`. It changes only standalone
`rule run --worker NONEMPTY`, `fallback worker`, and top-level `watch` when the
existing native supervisor selects SIGTERM. Python remains the default. No full
leaf, migration milestone, installed-wheel, release, or platform gate is closed.

## Ownership

The shared `catabolic_http::interruptible` formerly discarded signal identity:
SIGINT and SIGTERM set the same cancellation flag, joined the same operation,
and returned its result. The selected legacy adapters then rendered both as
KeyboardInterrupt/130. Frozen standalone Python exits by SIGTERM with no new
output at these selected boundaries. Its blocked external-rule witness can leave
its HTTP helper alive at CLI return; native cleanup must not reproduce that leak.

The additive `interruptible_with_signal` preserves the selected signal alongside
the operation result. It retains the existing cancellation flag, completion join,
and runtime teardown. The original `interruptible` still returns exactly the
owner result and is used unchanged by HTTP, ordinary native CLI, and unselected
legacy command families. It does not reset signal dispositions or terminate the
calling process.

The three selected adapters record TERM only for a standalone invocation. After
command dispatch returns, the legacy main boundary restores the default TERM
handler, unblocks TERM on its own thread, and raises it before rendering any
success/error envelope. A bounded 143 fallback exists if the OS cannot deliver
that signal; the Linux tests require an actual signal return code, `-15`.

Owner cleanup therefore finishes before process termination: rule HTTP helpers
are reaped, watch processing tools and cancellation bridges are joined, and
fallback tick writers/singletons are dropped. Prior durable metadata remains
committed. Progress already emitted by watch remains visible; termination adds no
new progress or final envelope. Repeated TERM/INT while the selected operation
cleans up do not bypass that join. The first selected signal still determines
presentation, as in the existing supervisor.

The private Python callable transport is a separate boundary. It retains its
acknowledged KeyboardInterrupt/130 result and transport process exit 0 after
cleanup when its helper receives TERM. No Python signal handler, embedded facade,
HTTP transport, publication, recovery, M6 refusal, source authorization, or
filesystem owner is changed.

## Selected executable evidence

- `test_rust_legacy_fallback_worker`: paired frozen/native idle SIGTERM in text,
  JSON and machine modes; no new output, singleton/writer release, immediate
  restart, and the existing exact SIGINT and ordinary/refusal controls
- `test_rust_scan_watch_cli`: paired idle SIGTERM in all three output modes after
  reading the completed-cycle progress line; native blocked-tool TERM reaping,
  no live execution claim, writer release and immediate restart. Existing SIGINT,
  scan-report difference, and public refresh-refusal controls remain required
- `test_rust_legacy_sigterm`: native blocked-HTTP TERM in all three modes,
  retained reclaimable lease, exact same-generation PUT on restart, and helper
  disappearance at return. A valid receipt response held until TERM cannot be
  accepted after helper cancellation. Another control accepts one receipt referencing an
  already-existing tiny fixture file, blocks the next job, then requires every
  catalog table to remain row-for-row identical across TERM. The callable
  control requires the acknowledged 130 envelope and helper reaping
- Legacy binary unit control: TERM followed by repeated TERM/INT during a
  deliberately held cleanup waits until that cleanup is released and completed.
  The standalone boundary then exits by TERM; the original shared API instead
  returns its exact operation error and the test process exits normally

All fixtures are disposable, use numeric loopback only, and signal owned child
PIDs. No physical output production or live catalog/service is exercised. The
callable test has an owned-child watchdog and complete suites run under an outer
timeout. The tracked receipt records artifact/source/log hashes and terminal
counts; detailed logs and immutable executable copies are in the implementation
worktree's ignored `.local-tests/legacy-sigterm/` directory.

The first new restart control incorrectly expected GET after interrupted PUT.
The owner correctly retained `leased`, so restart resends the same-generation PUT
with the original request. The corrected assertion checks that full request;
the failed initial runs remain retained and are not counted as passes.

## Limits

This is selected-boundary observable alignment with stronger native cleanup,
not complete signal equivalence. It does not qualify every completion-vs-signal
schedule, signals before supervisor installation or after dispatch, all repeated
signal orderings, inherited ignored/masked signal policies, panic paths,
uninterruptible kernel work, or every receipt acceptance window. SIGKILL cannot
perform cleanup. Non-Unix and installed/release artifacts are not qualified.
At this checkpoint other legacy command families retained their existing signal
presentation; the track D alignment below extends the boundary to all of them.

The underlying fallback apply/recovery and watch automatic-refresh refusals,
external HTTP diagnostic differences, and scan-report gaps remain unchanged.
Historical checkpoint receipts are retained as historical evidence. Previously
blocked broad safety-review lanes were neither reopened nor worked around.

## Final local result and independent review

The final Python 3.14.6 harness run passes all 49 rule/fallback/watch methods;
31 maintenance/callable/ledger regressions pass separately. The selected seven
TERM methods also pass under the existing Python 3.11.15 harness, which still
launches the pinned 3.14.6 frozen oracle. The legacy unit executable reports 14
passing functions, including its one internal subprocess-fixture function.
No final run has failures, errors, skips, or expected failures. Existing explicit
nonparity/refusal checks are not reclassified as full-parity evidence.

The independent reviewer accepted this selected Linux scope without a blocking
finding. It passed 12 distinct Python methods (including six additional private
callable fallback/watch output-mode cases within one method), the cleanup/shared
API unit, the final pre-acceptance and already-committed receipt controls, and
source/artifact fences. Its report and unchanged evidence manifest are retained
with the local logs. The manifest SHA-256 is
`c1cba8eca2a0a02be9fde07638391e659b739e5e2f987e307fd00308e55137e3`.

Offline build, scoped HTTP-library and CLI/test Clippy with warnings denied,
Rust format, scoped Ruff check/format, ledger freshness and patch whitespace
checks pass. The immutable legacy executable SHA-256 is
`bc98a3d6bbfa0938d642656a77702934642fee43059b8911359fcc660641e772`.
The [tracked checkpoint](../tests/parity/native-legacy-sigterm-checkpoint.json)
contains exact source, artifact, reference and evidence hashes.

## Track D: every standalone legacy family (October 16)

Frozen Python installs no TERM handler anywhere in the CLI, so every command
exits by the signal (`-15`) with no new output. The legacy executable now
applies the selected boundary above to every supervised family instead of
three: GraphQL, query and inbox, projection previews/execution and mapping
operations (including `mapping-events`), maintenance, identify, refresh and
catalog refresh, `api worker`, `consumer watch`, and the other dispatch-owned
supervisors. Each uses the additive `catabolic_http::interruptible_detailed_with_signal`
or the existing `interruptible_with_signal`; the shared `interruptible` APIs used
by HTTP and the companion CLI are unchanged. The executable-only Plex signal
guard (Plex login/import/matching/metadata) now also records whether its first
signal was TERM, so those owners exit by TERM after their cleanup as well.

Cleanup still finishes before termination: helpers are reaped, ticks joined,
writers and singletons released. SIGINT presentation (`KeyboardInterrupt`/130)
and the private Python callable transport's acknowledged 130 result are
unchanged. Unsupervised commands never installed a handler and already exited
by the default TERM disposition.

Native-only tests that previously asserted a `KeyboardInterrupt`/130 envelope for
TERM now assert the frozen `(-15, "", "")` presentation while keeping their
native cleanup and restart checks: `api worker` in all three output modes, and
consumer watch idle and in-flight TERM. The idle consumer-watch case now also
runs the frozen oracle and requires the same presentation. A legacy unit mode
covers the typed wrapper through repeated TERM/INT during held cleanup.

Local macOS arm64 results for this change (Kyoto build of the workspace, then
the test executables run directly): the legacy unit executable passes all 34
functions, including the new typed-wrapper TERM mode; catabolic-http passes 3;
the Plex login runtime tests pass 9. `test_rust_legacy_fallback_worker` passes
all 10 methods, and the consumer-watch idle TERM method passes with both the
frozen oracle and native returning `(-15, "", "")`. Not runnable on macOS, and
failing before any signal is sent for reasons unrelated to this change: every
`api worker` method (native refuses with "parent-death supervision is
unqualified on this platform"), the two in-flight consumer-watch TERM methods
(the harness uses Linux `ps --ppid`) and `test_rust_legacy_sigterm` (it observes
helpers through `/proc`). Those need the Linux lane.

**Update (track D supervision):** the native API and playback workers no longer
refuse on macOS. Every tool group now carries a lifeline guardian (see
`catabolic-runtime/src/process.rs`), and the three harnesses above use the
portable `tests/process_tree.py` (`ps -A`) instead of `/proc` and `ps --ppid`.
On macOS arm64, `test_rust_legacy_api_worker` (5 methods, including TERM in all
three modes), `test_rust_legacy_consumer_watch` (12, including both in-flight
TERM methods) and `test_rust_legacy_sigterm` (4) now pass.
