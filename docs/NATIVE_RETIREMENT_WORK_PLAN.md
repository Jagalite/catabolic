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
