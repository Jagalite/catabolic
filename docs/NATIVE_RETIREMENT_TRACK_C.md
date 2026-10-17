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
- **Residual.** A writer who targets the private name between verification and
  deletion. This is the same class as the accepted hardlink staging limit.
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
and 3. Frozen `os.replace` never shows that gap. This visibility difference
needs the user's acceptance.

Three crash points are new:

| Stage | State left behind | Native recovery |
| --- | --- | --- |
| `after_exchange` | The leaf is the new link; the temporary holds the previous link | Deletes the verified temporary, then acknowledges |
| `after_quarantine` | Remove: the leaf is absent and the quarantine holds the previous link | Deletes the verified quarantine, then acknowledges |
| `after_quarantine` | Replace: the leaf is absent, the temporary is the new link and the quarantine is the previous link | Publishes if the replacement is still desired, otherwise restores the previous link |
| `after_publish` | The leaf is the new link; the quarantine holds the previous link | Deletes the verified quarantine, then acknowledges |

A foreign entry found at either private name always blocks recovery with
`private publication entry … changed externally; inspect it`, and is never
deleted.

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
| 2 | Implemented; release gates still closed | this commit |
| 3–6 | Not started | |
