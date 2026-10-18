# Track C: production publication safety and recovery

Scoping note for track C of the [work plan](NATIVE_RETIREMENT_WORK_PLAN.md),
gate 1 of the [decision record](NATIVE_RETIREMENT_DECISIONS_2026_10_16.md):
"close native production behavior gaps, including publication/recovery". The
base is `a0ebb2f` on `codex/rust-migration-m4-plus`. The frozen reference is
`453fca9` (`reference-453fca9/src/catabolic`).

Rules for every item: existing refusals stay in place until their contract is
implemented, tested and independently reviewed. Nothing here lifts a release
gate on its own. Each item lands as its own commit.

## 1. Failpoint matrix for publication, replace and remove

**Exists.** `filesystem_mutation::failpoint(args, stage)` returns an injected
error, or exits with status 86 when `crash: true`. Symlink stages:
`after_journal`, `after_temporary`, `after_filesystem`, `before_commit`,
`after_commit`. Hardlink stages add `before_retention_rename`,
`after_retention`, `before_link` and `after_link`. The unit test
`crash_boundaries_are_recoverable_and_do_not_change_source_bytes` covers only
the four generic stages, only for symlink creation, and only as in-process
errors. The Python suite `test_rust_observation_filesystem` crashes the native
CLI at some stages and recovers in frozen Python.

**Missing.** No test covers replace, remove, forget or any hardlink kind at
every stage. Nothing checks that every failpoint in the source has a test.
Claim (marker) creation and the staged-hardlink window have no failpoints.

**Frozen Python.** `reconcile.py` and `hardlinks.py` have no failpoints, only
the `after_filesystem` callback and a `checkpoint()` hook in hardlinks.

**Plan.** Add `claim_output` stages `after_marker_create` and
`after_marker_write`, plus the hardlink stage `after_staged_link`. Add
`crates/catabolic-store/tests/publication_failpoints.rs`. For each operation
kind (prepare, create, replace, remove and forget, plus every hardlink kind),
and each stage that kind reaches, a child process applies a one-operation plan
and exits at the failpoint. The parent then recovers, re-applies and checks
these invariants:

- The run converges to a healthy state, or blocks with the documented
  explicit error.
- The journal is empty, and ownership rows match the filesystem.
- Source bytes are unchanged, and retained hardlink data is preserved.
- No private temporary is left behind.
- The publication generation advances exactly once.

The test also scans both mutation sources for failpoint literals. It fails if
any stage is missing from the matrix, or if any crash point in the matrix is
never reached.

**Implemented.** `filesystem_mutation::PUBLICATION_FAILPOINTS` lists 14
crash points. A crash point here is a boundary after an effect that changes a
published name, a private name, an ownership marker or the journal. Parent
directory creation is excluded, because it tolerates `EEXIST` and every
recovery repeats it.

`tests/publication_failpoints.rs` runs 59 cases: every operation kind, for
both link modes, at every stage that kind reaches. A copied test executable
exits at the failpoint, and the parent recovers and re-applies. A stage that
is not reached fails the case.

Two stages block explicitly instead of converging: `after_marker_create` and
`after_retention_marker_create`. The crash leaves an empty ownership marker,
which ownership checks refuse rather than delete. The test asserts the
refusal, the preserved marker and the pending intent, then performs the
documented operator step (remove the empty marker) and checks convergence.

A second test parses every `failpoint(args, "...")` literal in both mutation
sources. It requires the literals, the declared list and the matrix to agree.

### Failpoint injection is test-only

Before this change, request fields `failpoint` and `crash` were honoured in
every build. A caller could make a release `catabolic-native outputs apply
--options '{"failpoint":…,"crash":true}'` exit mid-operation. Frozen Python
has no such fields, so no compatibility contract keeps them.

They are now behind the `catabolic-store` feature `failpoints`, which is off
by default.

- **Who enables it.** Only `catabolic-cli`'s dev-dependency enables the
  feature, so it is on in the workspace test build. Shipped artifacts
  (`cargo build --release -p catabolic-cli`, the maturin wheel and
  `catabolic-dist`) build without dev-dependencies, so it is off there.
- **Without the feature.** A request that carries either field, even as
  `null`, is refused with `failpoint_injection_unavailable: …`. The refusal
  comes before any journal row or filesystem effect.
- **Where it is checked.** The check runs at every symlink and hardlink
  publication entry point and in manifest writes. The failpoint hook itself
  repeats the check as a backstop.
- **Before admission (Sol round 2 fix).** Fallback projection admission used
  to commit membership, history and generation changes before the executor
  refused the field. Every runtime entry point that can reach publication
  (fallback projection run/store/prepared apply, projection, public fallback
  and its tick, public watchers, watcher workers and execution, catalog
  refresh) now refuses first, so a refused request leaves the database and
  output byte-identical.
- **Fallback forwarding.** The fallback runtime now forwards these fields
  whenever they are present, so that they are refused rather than silently
  dropped.

Tests:

- `default_feature_builds_refuse_failpoint_requests_before_effects` runs the
  disabled path end to end, through a test-only switch.
- `test_default_feature_build_refuses_failpoints_before_any_admission`
  (`tests/test_rust_fallback_projection.py`) runs the CLI with the test-only
  `CATABOLIC_TEST_SIMULATE_DEFAULT_FEATURES` variable, which exists only in
  `failpoints` builds. Applies with a current or stale plan, and a bare
  request, each carrying `failpoint: null` or `crash: false`, are refused,
  with every table and the output tree unchanged.
- `tests/failpoint_feature_shipping.rs` checks that only that one
  dev-dependency enables the feature. It also checks that no default feature
  set, maturin feature list or release build command selects it.

The operator `--migration-failpoint` flag of `catabolic-native db` is a
separate, documented CLI option. It only returns an injected error, never
exits, and is unchanged.

## 2. Safe replace and remove protocol

**Exists.** Frozen-compatible symlink replace (a temporary link and then
`renameat`) and remove (`unlinkat`). Both re-check the leaf first, so neither
is atomic with respect to that check. `SYMLINK_RACE_GATE` refuses both outside
debug disposable fixtures. Hardlink retirement renames into
`.catabolic-retained/<id>/data` with `RENAME_EXCL`, but a foreign file raced
into the leaf is relocated into retention. `RETIREMENT_RACE_GATE` refuses this
outside debug disposable fixtures.

**Frozen Python.** The same check-then-`os.replace` and check-then-`os.unlink`
logic. A foreign leaf written after the check is overwritten or deleted. This
is the shared defect behind the filesystem lane's replace/remove proposal.

**Plan.** The protocol never destroys an entry it has not verified at a name
it controls:

- **Replace.** Create `.catabolic-link-<id>` (the frozen name), then exchange
  it atomically with the leaf (`RENAME_SWAP` on macOS, `RENAME_EXCHANGE` on
  Linux). The old entry is now at the private name. If it is the expected
  link, delete it. If it is foreign, exchange back so it returns to its name,
  then fail closed. Readers never see the leaf missing. A filesystem without
  exchange support is refused, with no fallback.
- **Remove.** Rename the leaf exclusively to `.catabolic-unlink-<id>`. Verify
  the moved entry, then delete it. A foreign entry is renamed back exclusively.
  If the name has been reused in the meantime, the entry stays preserved at the
  private name and the operation fails closed.
- **Hardlink retirement.** A mismatched entry found after the retention rename
  is returned exclusively to its original name instead of staying in
  retention.
- **Recovery.** Recovery understands the new intermediate states. Frozen
  Python recovery of those states acknowledges the operation and leaves an
  inert private link behind. That is non-destructive, and item 6 qualifies it.
- **Residual.** As first implemented, a writer could swap the private name
  between verification and deletion. Sol round 1 found this (P2), and the
  private-directory change below closes it.
- **Gates.** Both gates stay closed in release builds. Lifting them needs
  independent review and the user's decision.

**Implemented, with one change to the plan.** HFS+ (the evidence volume)
rejects `RENAME_SWAP` with `ENOTSUP`, and APFS accepts it. Refusing
replacement on HFS+ outputs would remove a working frozen feature, so the
native engine uses exchange where it is supported and a quarantine mode
otherwise. Both modes are equally safe for data:

1. Rename the leaf exclusively to `.catabolic-unlink-<id>`.
2. Verify the moved entry. A foreign entry is renamed back.
3. Publish `.catabolic-link-<id>` with an exclusive rename. If the leaf has
   reappeared, nothing is overwritten. Both links are preserved and the
   operation fails closed.
4. Delete the verified previous link.

In quarantine mode, readers can briefly see the leaf missing between steps 1
and 3. Frozen `os.replace` never shows that gap. The coordinator accepted this
as a known platform difference, data safety first. It is documented for users
in [OPERATIONS.md](OPERATIONS.md#operating-assumptions-and-limits) and is
pending the user's final acknowledgement.

Three crash points are new:

| Stage | State left behind | Native recovery |
| --- | --- | --- |
| `after_exchange` | The leaf is the new link; the temporary holds the previous link | Deletes the verified temporary, then acknowledges |
| `after_quarantine` | Remove: the leaf is absent and the quarantine holds the previous link | Deletes the verified quarantine, then acknowledges |
| `after_quarantine` | Replace: the leaf is absent, the temporary is the new link and the quarantine is the previous link | Publishes if the replacement is still desired, otherwise restores the previous link |
| `after_publish` | The leaf is the new link; the quarantine holds the previous link | Deletes the verified quarantine, then acknowledges |

A foreign entry found at either private name always blocks recovery with
`private publication entry … changed externally; …`, and is never deleted.

**Private-directory deletion (Sol round 1 fix).** A private entry is never
unlinked in the shared output directory:

1. `mkdirat` creates `<private name>.private` with mode 0700. It fails if the
   name exists.
2. The directory is opened with `O_DIRECTORY|O_NOFOLLOW`. Its device and inode
   must match the name, its device must match the parent's, its owner must be
   the effective user and its mode must be exactly 0700.
3. The entry is moved in with an exclusive `renameat` under the name `entry`.
4. `entry` is verified inside the directory, through the held descriptor.
5. `entry` is unlinked there, then the empty directory is removed with
   `AT_REMOVEDIR`.

A swap of the private name after the caller's check therefore moves the
foreign entry into the directory, where it fails verification and is
retained. The same happens when the directory cannot be created or verified,
or contains anything except our link: the operation retains and fails, and
deletes nothing.

The directory name derives from the journal operation, which is a random
UUID. Recovery therefore finds an interrupted predecessor's directory without
scanning the parent. It completes the cleanup only for a verified directory
that holds exactly our link, or nothing. Any other content, or a squatter
that is not a directory, blocks recovery and is retained. The new crash point
`after_private_move` covers the window between the move and the unlink.

**Sol round 2 fix: identity before adoption.** An existing directory at the
private directory name is never adopted, not even a same-owner 0700 directory
that is empty or holds a link to an allowed target. After `mkdirat`, which
fails with `EEXIST` rather than reuse a name, the directory's device and inode
are committed to a `meta` row keyed by the journal operation and the directory
name. Only then does anything move into it. The schema-32 journal has no
column for this, and `meta` needs no schema change.

Execution and recovery act only on a directory whose identity matches that
record. Any other file, link or directory at the name is retained untouched,
and the operation fails with `… was not created by this operation; retained
untouched`. The record is cleared once the directory is removed.

Two new crash points cover this:

- **`after_private_create`.** The directory exists but was never recorded.
  This state blocks explicitly. The operator removes the empty directory after
  inspection, and recovery then converges.
- **`after_private_record`.** This state converges.

Track B may record volume UUIDs. If it does, the record can carry the same
identity.

**Remaining residual, stated plainly.** Between verification inside the
directory and the unlink, a writer running as the same user can still replace
`entry`, and that replacement is deleted. The directory is 0700 and owned by
us, so other users cannot reach it. A same-user writer must find the
per-operation name and act inside that window. A second residual is inode reuse. A crash between removing the directory
and clearing its record leaves a stale record. A writer would then have to
recreate a directory at the same per-operation name that reuses the same
inode number.

The test
`characterize_residual_same_user_writer_inside_the_private_directory`
demonstrates this residual; it does not qualify it. As in
[OPERATIONS.md](OPERATIONS.md), a malicious process running as the same user
is outside the supported concurrency model.

Frozen Python reaches these states only through handover. It acknowledges the
exchange, publish and remove-quarantine states, leaving an inert private link
behind. It refuses the replace-quarantine state with
`replacement destination changed externally`. None of this is destructive.
Item 6 records both behaviors.

**Evidence** (Kyoto job `51aae75494a34cffa7c58cd059275bf4`):

- The crash matrix passes 62 cases in quarantine mode, under `TMPDIR` on
  HFS+. It passes 61 cases in exchange mode on an APFS directory, through the
  ignored test `publication_crash_points_in_the_other_replacement_mode`.
- New unit tests cover these cases:
  - A foreign leaf swapped in just before displacement is restored, for both
    replace and remove.
  - A second writer reuses the leaf name, and both foreign entries survive.
  - Every interrupted private link is cleaned up after verification.
  - A foreign file planted at a private name is never deleted.
- The two hardlink race tests now expect the foreign entry back at its own
  name. If that name has been reused, they expect it kept in retention.
- The old test that characterized frozen Python destroying the foreign leaf
  is replaced. Its premise no longer holds for native code.

## 3. Reconciler apply and recover

**Exists.** The native CLI `sync`/`recover` route into
`filesystem_mutation::execute_mut`. Create, prepare and forget publish in
release builds. Replace and remove hit the item 2 gates. The Python facade
`public_reconcile` refuses apply and recover unconditionally
(`PUBLICATION_GATE`), including the `after_filesystem` callback.

**Frozen Python.** `Reconciler.apply` and `Reconciler.recover` take the
publication lock (`@serialized`), journal each action, call
`after_filesystem(operation)` after each effect, record projection statistics,
emit refresh events, call `published()` and record fallback publication.

**Plan.** Route the facade's apply and recover to the shared executor, under
the publication lock and the writer boundary. Serialize the
`after_filesystem` callback through the existing PyO3 bridge, after the effect
and before acknowledgement, matching the frozen order. Pair the results with
frozen Python on disposable fixtures. The facade inherits the item 2 gates, so
replace and remove refusals stay until those gates lift.

**Implemented.** `Reconciler.apply` and `Reconciler.recover` call the new
PyO3 method `NativeStore.reconcile_publish`. It runs
`public_reconcile::publish` and then `filesystem_mutation::execute_observed`,
on the caller's Store.

Behaviour that matches frozen Python:

- **Locking.** Publication is serialized by the publication lock, and the
  writer-outside-transaction rule applies.
- **The `after_filesystem` callback.** It receives each operation as a dict,
  after the filesystem effect and before acknowledgement. This covers
  prepare, forget and hardlink operations too. An exception raised by the
  callback propagates unchanged and leaves the operation journaled.
- **Validation order.** Budget and catalog errors come in the frozen order.
- **Forwarded fields.** Only public fields are forwarded (`notify_consumers`,
  removal and change budgets, `cancel_unapplied`), so private options cannot
  relax a gate. The generic read dispatcher still refuses apply and recover.
- **Callback truthiness (Sol round 2 fix).** Frozen Python tests
  `if after_filesystem:` before each call. Native code now does the same per
  operation, so a falsey callable (`__bool__` false or `__len__` zero) is never
  called, and an exception from `__bool__` propagates like a callback error.
  `test_callback_truthiness_matches_frozen` pairs both engines.
- **Projection execution statistics.** Frozen `projection_stats` records an
  execution snapshot around every apply, and native code did not. The shared
  executor now records it in `meta`: begun before the first journal row,
  finished after verification. This applies to every native apply, including
  CLI `sync` and fallback projection.

Known differences, all fail-closed:

- **Gated operations.** Symlink replace/remove and hardlink retirement return
  an unsafe plan with the existing gate blocker. Fallback-bound catalogs keep
  their refusal (item 5).
- **Callback reentry.** The callback runs while the native Store is checked
  out, so reentering that Store from the callback raises `native store is
  already in use`. Frozen Python allows reads there.
- **Caller transactions.** A caller inside a transaction is refused. Frozen
  Python nests.
- **Signals.** No signal bridge runs during publication. An interrupt takes
  effect after the call, and the journal stays recoverable.

**Evidence.**

- `public_reconcile` unit tests (3), plus the store filesystem, hardlink,
  publication-mapping and manifest unit tests: 42 passed. Crash matrix: 3
  passed.
- `tests/test_native_reconciler_publication.py` ran against the Kyoto-built
  debug extension, compiled without the `failpoints` feature: 22 passed.
  - 15 unchanged frozen `CatalogTest` reconciler assertions ran on the native
    facade.
  - 7 twin-catalog differentials compared frozen and native results,
    callback operation streams, the journal, owned links, publication
    generations, refresh state, execution statistics and the output tree.
    They cover cross-engine recovery after a callback failure in both
    directions, budget errors, `max_changes` and `notify_consumers=False`.
- The updated installed-gate cases in `scripts/native_reconciler_embedding_tests.py`
  ran against the same debug extension: 6 passed. The strict wheel-bound
  receipt was not re-run; that belongs to the track E installed lane.
- `tests/test_native_reconciler_contract.py`, `test_rust_maintenance_cli`,
  `test_rust_layout_mutation`, `test_rust_rules`,
  `test_rust_observation_filesystem` and `test_rust_hardlinks` all passed.

## 4. Publication mappings v2

**Exists.** `publication_mappings::run_with_account` previews fully. Apply and
`recover()` return `PUBLICATION_GATE`. The CLI maps the v2 recovery fallback to
that gate. The "Mapping runtime errors v2" decision only changes how
`projection mapping-events` presents errors. It explicitly does not change
publication or recovery admission, so it is a landed precondition, not this
item.

**Frozen Python.** `run(apply=True)` requires a matching `expected_plan` and a
removal budget. Folder apply records an event, runs `Reconciler.apply` and
`verify`, and marks the event verified or uncertain. Export and import publish
bundles or run official CLIs. `recover` replays a folder event through
`Reconciler.recover`, then `preview` (refusing actions outside the authorized
set) and `apply`. Export recovery resumes a bundle. Import recovery accepts
operator-attested resolution.

**Plan.** Folder apply and recover first, on top of items 2 and 3, with the
same stale-plan, budget and authorized-action checks. Export and import
publication stay gated as a separate sub-item. They depend on official
application execution, which needs real-service acceptance.

## 5. Dependent and fallback publication

**Exists.**

- Projection execute is refused (`projection::EXECUTION_GATE`).
- Maintenance output synchronization is refused (`maintenance.rs:400`).
- Watch automatic refresh is refused (`legacy/watch.rs:79`).
- Fallback apply and recover require `disposable_fixture`
  (`require_fallback_fixture`), in the store, `public_fallback`,
  `public_watchers` and runtime `fallback.rs`.

**Frozen Python.** These paths call `Reconciler.apply` and `recover`
directly. Fallback publication uses live probes and the `published_generation`
bookkeeping that is already ported in `published_context`.

**Plan.** Enable each path only through the shared executor, once items 2 and
3 hold. Fallback publication supports symlinks only, as frozen Python does.
Each path gets its own failpoint rows in the item 1 matrix and paired frozen
checks.

## 6. Handover

**Exists.** Python-journal-to-native recovery and native-crash-to-Python
recovery tests exist for symlink create and remove
(`tests/test_rust_observation_filesystem.py`). The hardening note records that
Python cannot see native hardlink staging.

**Plan.** Run a matrix over the item 1 crash states, recovered by the other
engine in each direction. The frozen oracle is invoked through
`CATABOLIC_PYTHON_ORACLE`. Record every non-destructive residual, such as a
leftover private link or staged entry, and the native cleanup that resolves
it.

## Status

| Item | Status | Commit |
| --- | --- | --- |
| 1 | Implemented (59-case matrix at the time; 62 with item 2) | `8ffd97d` |
| 2 | Implemented; release gates still closed | `505c368` |
| Failpoint gating | Implemented | `eecc2b6` |
| Sol round 1 P2 | Fixed | `d5b5ef0` |
| 3 | Implemented | `e50b453` |
| Sol round 2 P2 #1 (private directory identity) | Fixed | `5aa37ce` |
| Sol round 2 P2 #2 (refuse failpoints before admission) and #3 (callback truthiness) | Fixed | this commit |
| 4–6 | Not started | |
