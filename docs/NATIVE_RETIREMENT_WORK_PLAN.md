# Native retirement work plan

On October 16, 2026 the user asked for all remaining coding work toward
retiring the Python engine. That includes the metadata integrity gap,
production behavior gaps, upgrade and rollback rehearsal, packaging and
distribution, and the remaining reviews. This plan lists that work in order.
It does not record evidence: each item records its evidence in its own
receipt, and the [decision record](NATIVE_RETIREMENT_DECISIONS_2026_10_16.md)
gates still apply.

## User decisions, October 16

- **System tools:** do not install them. FFmpeg `libwebp`, Tesseract and the
  pinned diagnostics interpreter stay recorded as missing prerequisites.
- **Platforms:** macOS arm64 only for now. Linux comes later.
- **Distribution:** the native engine becomes the `catabolic` distribution
  itself, built with maturin with the thin compatibility layer. The version
  becomes 1.0.0.
- **Python API:** support the CLI and the 159 forwarded names. Deprecate the
  remaining public names, with a documented list.

## Working rules

- Kyoto compiles every Rust target. Tests run separately, with the pinned
  oracle variables and a physical `TMPDIR`.
- Every change is reviewed independently by GPT-6.1-Sol. Findings are fixed
  and re-reviewed before the change is committed.
- Each item lands as its own commit on `codex/rust-migration-m4-plus`.
  Nothing is pushed or published, and the default is not switched, without an
  explicit instruction.
- A new native safety behavior that goes beyond Python parity is documented
  as a versioned change. A ledger row is closed only with evidence tied to an
  exact commit.

## Reviews

The `schema`, `broad_definition` and `filesystem` lanes previously stopped
because the reviewer tool returned an automated platform error. Nobody
declined them. The user's October 16 instruction authorizes running them.
Their findings feed the tracks below.

## Tracks

### A. Integrity and content protection

1. Record each filesystem's type and timestamp granularity alongside the
   existing volume identity, using `fstatfs`.
2. Refuse source and artifact revisions recorded within one timestamp tick of
   their mtime (a racy-revision rule). Callers must rescan, or verify the
   full hash.
3. On coarse or unknown filesystems, require the full hash before reusing an
   artifact or serving it for the first time. Cache the verified revision.
4. Hash full GET responses while streaming them, as a final check. HEAD and
   Range requests stay cheap.
5. Add `ctime_ns` to source revisions. This is a schema step and goes through
   the schema review.
6. Close unrecognizable orphaned WAL/SHM sidecars, once the filesystem review
   has settled the design.

### B. Upgrade, restore and rollback

1. Add operator commands for native inspect, plan and upgrade, taking a plan
   file, quiescence assertions and failpoints.
2. Add an identity-preservation checker.
3. Build a rehearsal harness that runs on legacy and scale fixtures. It
   interrupts the upgrade at every durable boundary, resumes, verifies
   identities, restores to a new path, and reopens the restored copy with
   frozen Python.
4. Add an offline WAL-to-rollback-journal conversion procedure.
5. Record the post-upgrade write limitations as asserted refusals.
6. Design and implement controlled restore with incarnation rotation.

### C. Production safety and recovery (M6/M8)

1. Build a durable-boundary failpoint matrix and a crash harness.
2. Design and implement a safe symlink and hardlink replace/remove protocol,
   using the filesystem review's findings.
3. Implement reconciler apply and recover.
4. Implement publication-mapping v2 apply and recover.
5. Enable the dependent publication paths: projection execute, maintenance
   rules and sync, and refresh from watch.
6. Enable fallback publication.
7. Qualify handover in both directions: Python journal to Rust recovery, and
   Rust journal to Python recovery.

### D. Named behavior gaps

- GraphQL: parser residuals, field-conflict and merged-field nulls, recorded
  Int output, and variable-JSON precedence.
- CLI: `api token` `~user` expansion and the HOME-unset fallback, program
  import kinds, plan evaluation, SIGTERM alignment, and external rule
  transport.
- Status of the first two bullets after the October 16 track D pass (see
  [track D status](#track-d-status-october-16) below).
- Notifications: the unsupported option matrix, implemented provider by
  provider.
- Observation: regex dialect constructs, scan and maintenance report fields,
  and scan continuation.
- Ledger rows: legacy item query adapters, contribution resolution, tag
  suppression, and durable command evidence.
- Supervision: macOS parent-death and grandchild handling.

### E. Packaging and distribution

1. Move build-time assets (the OpenAPI release data, migrations, interchange
   files and guides) out of `src/catabolic`.
2. Run the macOS arm64 standalone lane, the wheel lane and the compatibility
   lane locally, recording receipts.
3. Disposition the public API, set the distribution identity and entrypoints,
   update the release pipeline, and build the artifact matrix. These need the
   user's packaging decisions.
4. Rewrite installed acceptance against the native artifacts.
5. Flip the ledger and docs, then remove the shipped Python engine. The frozen
   reference and rollback artifacts are kept separately.

### F. Qualification

- Rerun the full aggregate on each committed checkpoint.
- Run the storage, Jellyfin and experience acceptance lanes where they are
  available locally.
- Record a performance baseline and its deltas.

## Needs outside resources or user decisions

- System tools: FFmpeg with `libwebp`, Tesseract, and the pinned diagnostics
  oracle interpreter, which is Linux-only.
- The supported platform list and Python versions, Linux CI hosts, and PyPI
  publication.
- Real-service acceptance against Plex, Jellyfin and notification services.
- Motion integration, which the user deferred on 2026-10-14.
- The final default switch and removal of the shipped Python engine.

## Track D status, October 16

Status of the first two track D bullets. Results come from a Kyoto build of the
workspace on macOS arm64, with test executables and Python suites then run one
at a time against the frozen oracle (Python 3.14.6, reference `453fca9`).

| Item | Status | Commit | Evidence |
|---|---|---|---|
| `api token` `~user` and HOME-unset | Closed; the not-found `RuntimeError` presentation is a structured error, **pending user confirmation** | `ecce11d` | private-secret 6/6, access-admin 6/6, store unit 17 |
| GraphQL variable-JSON precedence | Closed | `065d6f2` | precedence 1 method (45 envelopes), admission 8 (242 comparisons), variable-errors and file-boundary 2 (153 + 55 pairs) |
| Program import kinds | Closed (external processors) | `267a621` | `test_rust_programs` 8, `test_program_bundle` 9 |
| SIGTERM alignment | Closed for all standalone legacy families; Linux lane still needed for three harnesses | `69f6e0b` | legacy unit 34, fallback-worker 10, consumer-watch idle TERM paired with the oracle |
| External rule transport | Closed for the listed cases; Latin-1 credential bytes and a few parser edges remain recorded differences | `6295930` | runtime unit 12, worker diagnostics 12 (one Linux-only skip), external rule 18 (one Linux-only skip) |
| GraphQL field-conflict | Closed (suite passes) | earlier checkpoints | overlap 77, document validation 55, coercion 182 pairs, including the former known-gap vectors |
| GraphQL merged-field nulls | Closed (suite passes) | earlier checkpoints | runtime completion 59 pairs plus runtime inputs |
| GraphQL recorded Int output | Closed (suite passes) | earlier checkpoints | 42 recorded-Int CLI triples |
| GraphQL parser residuals | **Not closed** | none | see below |
| Plan evaluation | **Deferred to track E** | none | see below |

**Parser residuals.** Every live native/oracle parser comparison passed: 250
parser pairs, the 156 captured residuals, budget precedence v2 (150) and v1
(68), diagnostic ownership (38), boundary ownership (61), required-value
precedence (50), lexical capture (197), token-budget locations (144), source
locations (58), literal diagnostics (158), string diagnostics (56) and
unsupported-scalar precedence (31 controls). The suites still do not pass
locally, for these reasons:

- Retained `baseline` controls where frozen Python itself crashes (for
  example `dangling-backslash-residual`, which hits a frozen `IndexError`)
  embed the capture host's traceback paths, so `test_rust_graphql_parser_budget_precedence`
  and `test_rust_graphql_string_diagnostics` each fail one subcase on path text
  alone.
- Three evidence-archive checks need `/workspace/shared` capture seals that
  are not present here.

Those crash controls are intentional non-parity: native returns a controlled
syntax error. Separately, `test_rust_graphql_empty_completion` was not
runnable here: it needs a provisioned extension root and the
`catabolic-http-experimental` binary.

**Plan evaluation.** Watcher runs on the CLI already evaluate query,
fallback and projection plans natively and are compared with frozen Python in
`test_rust_watcher_projection.py`. What remains is the companion and Python
facade `plans evaluate` entry point, which refuses fallback and projection
plans. Frozen `Resolver.resolve` needs a writable session and probes, so this
moves to track E with the thin-wrapper boundary.

**Not runnable on macOS (Linux lane needed):** `test_rust_legacy_api_worker`
(native refuses API-worker parent-death supervision on this platform),
in-flight consumer-watch TERM (Linux `ps --ppid`) and `test_rust_legacy_sigterm`
(it observes helpers through `/proc`). Logs are under
`/Volumes/seed1/Projects/catabolic-builds/evidence/track-d/logs/`.

### Supervision: macOS parent-death and grandchildren (track D, October 17)

**Closed.** Each tool started by `catabolic_runtime::process::command_output`
gets a guardian process in its private process group. The guardian is forked
after `setsid` and detached from the tool by a double fork. It keeps only the
read end of a close-on-exec lifeline pipe; the spawning worker owns the write
end. When the worker exits for any reason, including SIGKILL, the guardian
reads EOF and SIGKILLs the whole group, so the direct tool and its
grandchildren die on macOS as well as Linux. Normal retirement still kills the
group, guardian included. Linux keeps `PR_SET_PDEATHSIG` as an extra guarantee.
The API worker and playback worker `run`/`tick` no longer refuse on macOS.

Test harnesses are portable: `tests/process_tree.py` reads the kernel process
table through `ps -A` (procps and BSD ps), replacing `/proc` scans and
`ps --ppid`. `test_rust_playback` now also runs on macOS when FFmpeg is present.

Results on macOS arm64:

- Runtime unit tests: 149 passed, 2 ignored, including the new portable test
  that SIGKILLs a worker and checks that its tool and a grandchild die, and a
  test that no guardian outlives normal or failed commands. One fixture hit a
  shared-budget timeout under load and passed when rerun.
- Python suites:
  - `test_rust_legacy_api_worker`: 5
  - `test_rust_legacy_consumer_watch`: 12
  - `test_rust_legacy_sigterm`: 4
  - `test_rust_legacy_external_rule`: 18, no skips
  - `test_rust_legacy_external_worker_diagnostics`: 12, no skips
  - `test_rust_playback`: 19, on the second full run. The first full run, under
    heavy host load from another lane, had six 30-second CLI timeouts; that log
    is retained.

Limits: a descendant that deliberately leaves the session (its own `setsid`)
escapes, as before. The lifeline pipe is created and then marked close-on-exec
in two steps, because macOS has no `pipe2`. A fork by an unrelated spawner
between those steps could inherit the write end; spawns through this module
mark every descriptor close-on-exec. The fallback probe helper keeps its
existing stdin-EOF handoff and was not changed.

#### Review round 1 corrections (October 17)

Sol's round-1 review of `2038382` found three defects, now corrected:

1. **No fork inside `pre_exec`.** The guardian was forked twice inside the
   tool's post-fork callback. That runs `pthread_atfork` handlers in a forked
   child, where a host handler can deadlock. The guardian is now an exec'd
   `/bin/sh` (builtins `read` and `kill` only, empty environment), spawned by
   the worker as the leader of a new process group. On macOS it is spawned with
   `posix_spawn` (`POSIX_SPAWN_SETPGROUP | POSIX_SPAWN_CLOEXEC_DEFAULT`), which
   runs no fork handlers and keeps only its lifeline stdin and `/dev/null`
   output. Elsewhere it is an ordinary spawn whose callback only calls
   `setpgid` and marks descriptors close-on-exec. The tool then joins that
   group with `setpgid` in its `pre_exec`. Both callbacks now contain only
   async-signal-safe calls (`setpgid`, `fcntl`, `fchdir`, `umask`, and the
   existing Linux `prctl`/`getppid`).
   - Tools no longer get their own session. They are in a background process
     group of the worker's session, so a terminal's job-control signals still
     go only to the foreground group.
   - The guardian is reaped after the tool, so the group ID stays reserved
     throughout retirement.
   - If the tool fails to spawn, the guardian is killed and reaped.
2. **Close-on-exec race.** Linux now creates the lifeline with
   `pipe2(O_CLOEXEC)`. On macOS it is created under the exclusive side of
   `catabolic_core::spawn_guard`. Every in-workspace production spawn site now
   forks through `spawn_guard::spawn`, which holds the shared side:
   - `runtime::process` (tool and guardian)
   - `runtime::watchers` (supervisor tick child)
   - `runtime::observation` (isolated worker and supervised helper)
   - `runtime::fallback` (probe helper)
   - `store::source` (`findmnt`, Linux)

   Not coordinated: test-only spawns, the separate `catabolic-dist` build tool,
   and an embedding host's own subprocesses.
3. **Lifeline on a standard descriptor.** Both lifeline ends are relocated to
   descriptors 3 or above with `F_DUPFD_CLOEXEC`.

New tests:

- A fixture registers a `pthread_atfork` child handler and requires exactly
  two forks per command (guardian and tool), with none inside a forked child.
- A stress fixture spawns 150 long-lived children from another thread while the
  tool starts. The tool group must still die after the worker is SIGKILLed.
- A fixture closes descriptors 0, 1 and 2 before running a one-second tool,
  which must succeed.
- The group-membership tests now expect the guardian-led group.
- The consumer-watch in-flight TERM test now requires exactly two children
  (the tick and its guardian), both gone and reaped at CLI return.

Results on macOS arm64 with the corrected build:

- Runtime unit tests: 155 passed, 2 ignored. The 37 `process` tests passed
  three consecutive times. Core unit tests: 101 passed.
- Python suites:
  - `test_rust_legacy_consumer_watch`: 12
  - `test_rust_legacy_sigterm`: 4
  - `test_rust_legacy_external_rule`: 18
  - `test_rust_legacy_external_worker_diagnostics`: 12
  - `test_rust_playback`: 19
  - `test_rust_legacy_api_worker`: passed 5 of 7 full runs (5 methods each).
    The two failing runs coincided with host load averages of 9 to 12. One had
    a 30-second CLI timeout in the busy case; the other had a 15-second
    heartbeat wait and a `database is locked` setup error. Both logs are
    retained, and the busy method passed when rerun alone.

#### Review round 2 corrections (October 17)

Sol's round-2 review found two defects in the round-1 design, both reproduced
on macOS:

- **P1:** the shell guardian died from the SIGHUP that XNU sends to an orphaned
  stopped group, so a stopped, HUP-ignoring tool survived the worker's death.
- **P2:** without `setsid`, a tool kept the worker's controlling terminal, and
  opening `/dev/tty` stopped it on SIGTTIN.

The design is replaced as follows. It supersedes the round-1 guardian and the
tool's `setpgid`.

- **Supervisor.** The worker execs a supervisor (the hidden
  `__catabolic-supervise` mode of the Catabolic executable) in place of the
  tool. Every Catabolic binary registers itself first thing in `main`, and
  embedders use the registered native helper. The post-fork callback calls only
  `fchdir`, `setsid`, `umask`, `fcntl`, and the Linux `prctl`/`getppid`.
- **New session.** The tool is therefore a session leader with no controlling
  terminal, as with frozen Python's `start_new_session=True`.
- **Guardian.** In that fresh image the supervisor:
  1. enumerates its open descriptors;
  2. double-forks a guardian into the same session and group;
  3. waits for the guardian's readiness byte.

  Before signalling readiness, the guardian ignores SIGHUP, SIGINT, SIGQUIT,
  SIGTERM, SIGTSTP, SIGTTIN, SIGTTOU, SIGPIPE, SIGUSR1 and SIGUSR2, and closes
  everything but the lifeline. Only then does the supervisor exec the tool,
  keeping the same PID. The worker therefore observes the tool's exact exit
  status (including death by signal) and output directly; nothing is relayed.
  A failed tool exec is reported back as an errno over a close-on-exec pipe,
  so a missing tool is still `unavailable`.
- **Parent death.** On lifeline EOF the guardian SIGKILLs the group.
  `parent_death_supervised()` reports whether a supervisor is available.
  Without one, the API and playback workers keep refusing on non-Linux
  platforms.
- **Descriptor marking.** In the child, macOS now marks exactly the open
  descriptors, read with `proc_pidinfo(PROC_PIDLISTFDS)` into a buffer
  allocated before fork, instead of looping to a soft limit of up to 1M. If
  the buffer may be truncated, it falls back to the full loop. This removes
  the main cost of each tool start.

New regressions:

- A stopped, HUP-ignoring tool and its grandchild die after a worker SIGKILL.
- From a process with a controlling pseudo-terminal, the fixture itself can
  open `/dev/tty`, but the tool's open fails immediately.
- Tools keep exact output bytes, exit code 7, death by SIGTERM (`-15`), and
  session and group leadership; a missing tool is `unavailable`.
- A non-close-on-exec descriptor numbered 9000 or above is not inherited.
- The atfork fixture requires exactly one fork in the worker per command.
- The consumer-watch in-flight TERM test requires exactly one child (the
  tick), whose group has at least two members (the tick and the guardian),
  and requires every member to be gone at CLI return.

Round-2 results on macOS arm64:

- Runtime unit tests: 161 passed, 2 ignored. The 43 `process` tests passed
  three consecutive times, in about 5.4 s each.
- Python suites:
  - `test_rust_legacy_sigterm`: 4
  - `test_rust_legacy_external_rule`: 18
  - `test_rust_legacy_fallback_worker`: 10
  - `test_rust_legacy_consumer_watch`: 12, on the second run (the first, under
    load, missed watch lines four times)
  - `test_rust_playback`: interleaved A/B passed 2 of 2 on the new build and
    1 of 2 on `ee101fe` (that failing run had host load above 7)

Interleaved `test_rust_legacy_api_worker` A/B runs (5 methods each):

| Session | Build | Passed | Failing runs |
|---|---|---|---|
| 1, load 3–5 | `2038382` | 3/3 | none |
| 1, load 3–5 | `ee101fe` | 3/3 | none |
| 2, load 4–6 | `2038382` | 3/4 | 15 s "worker heartbeat not observed" |
| 2, load 4–6 | new build | 3/4 | `database is locked` |

The api-worker stalls therefore track host load and are not caused by these
changes.

**Open: `test_rust_legacy_external_worker_diagnostics` is timing-sensitive.**
Its full-table comparison includes `fallback_epoch.generation`, which change
triggers bump only when a column value really changes. The same-worker GET
poll renews `processor_jobs.lease_until` in whole seconds (both engines use
integer seconds), so whether that renewal changes anything depends on whether
it lands in a new second. The frozen side alone produced 46, 47 and 48 for
identical subcases across runs, and across 22 mismatched subcases native was
higher 8 times and lower 14 times. Pass rates:

- `ee101fe`: 4 of 4 full runs passed.
- New build: 2 of 5 full runs passed, mostly failing GET subcases with
  off-by-one epochs. Its extra exec makes native runs longer, which changes
  how often the two engines' renewals fall in matching seconds.

The assertion is unchanged. Stabilizing it needs a decision: compare the epoch
modulo no-op renewals, or schedule renewals deterministically in the fixture.

#### Review round 3 corrections (October 17)

- **Supervisor admission** (`245c261`). Supervisor mode runs only if the
  process leads its own session and process group and every protocol
  descriptor is an open pipe/FIFO. Otherwise it exits 127 and signals nothing.
- **Fail closed** (`3c38ef0`). The supervisor executable is pinned by identity.
  Once a supervisor or native helper is configured, a removed or replaced
  executable makes every tool spawn fail with `parent-death supervisor
  unavailable`; there is no fallback to direct execution.
- **Guardian liveness.** The guardian holds the only write end of a liveness
  pipe that the worker polls throughout the command. Any event on it means the
  guardian died: the command fails with `parent-death guardian exited
  unexpectedly`, and the group is retired by the tool's PGID. That PGID cannot
  have been reused, because the tool is the worker's unreaped child.
- **The worker-diagnostics fixtures** (`92bb986`) now start each resumed poll
  in a later wall-clock second than the claim, so both engines' whole-second
  lease renewals deterministically change the row. 5 of 5 full runs passed at
  load 2.4–5.6.

Results with all three corrections on macOS arm64:

- Runtime unit tests: 164 passed, 2 ignored. The 46 `process` tests passed
  three consecutive times.
- One full run each of:
  - `test_rust_legacy_sigterm`: 4
  - `test_rust_legacy_external_rule`: 18
  - `test_rust_legacy_fallback_worker`: 10
  - `test_rust_legacy_consumer_watch`: 12
  - `test_rust_playback`: 19
  - `test_rust_legacy_external_worker_diagnostics`: 12
  - `test_rust_legacy_api_worker`: 5

  All passed at load 2.4–3.6.

#### Review round 4 corrections (October 18)

- **Protocol handshake.** The supervisor executable could be replaced between
  its identity check and the exec, for example by an upgrade, another
  version, or a wrapper that execs the tool directly, and the tool would then
  run unguarded. The worker now passes the protocol version
  (`SUPERVISOR_PROTOCOL`, 2) and a random 128-bit nonce for each spawn. The
  guardian writes `catabolic-guardian <version> <nonce>` on the liveness pipe,
  whose only writer it is, before releasing the supervisor to exec the tool.
  The worker accepts the tool only after reading exactly those bytes. Wrong
  bytes, an exec or exec error first, or no handshake within 30 seconds fail
  the spawn with `parent-death supervisor protocol mismatch`, and the tool's
  group is retired. A supervisor given another protocol version refuses with
  exit 127. A deliberately malicious same-user replacement that fakes the
  handshake is out of scope: the same user can already kill or trace the
  guardian.
- **`CATABOLIC_NATIVE_BINARY` pinning.** The override's identity is pinned at
  first use. A changed value, or a replaced file at the same value, then
  fails every spawn instead of running the new file.
- **Tests.** A wrapper that execs the tool directly, and an old-protocol
  supervisor that echoes the nonce with version 1, are both swapped in
  between verification and exec. Both are refused, and no tool survives. The
  override is replaced between commands, and a different value is used after
  first use; both are refused.
- **Fixture timing.** `test_invalid_state_and_receipt_refuse_before_any_publication`
  reuses the job leased by the previous subcase, so it had the same
  whole-second lease renewal drift as the diagnostics fixtures. It failed
  deterministically on this host with both the round 3 and round 4 binaries.
  Each subcase now starts in a later wall-clock second (`after_lease_second`).
  No comparison is loosened.

Results on macOS arm64:

- `process` and `helper` unit tests: 50 passed.
- One full run each of:
  - `test_rust_legacy_sigterm`: 4
  - `test_rust_legacy_fallback_worker`: 10
  - `test_rust_legacy_consumer_watch`: 12
  - `test_rust_playback`: 19
  - `test_rust_legacy_api_worker`: 5
  - `test_rust_observation_filesystem`: 36
- `test_rust_legacy_external_rule` (18) passed three times after the fixture
  change.
- `test_rust_legacy_external_worker_diagnostics` (12) timed out once in one
  subcase, during a 248-second run that overlapped another track's
  450-second store suite. It then passed twice, in 123 and 137 seconds.
