# Native integrity hardening, October 16, 2026

These native behaviors go beyond frozen-Python parity. Each is a versioned
difference that fixes a finding from the October 16
[independent review lanes](NATIVE_REVIEW_LANES_2026_10_16.md). All of them
fail closed. None changes catalog data or schema.

## Generated outputs on coarse-timestamp filesystems (A1, part of FS3)

Metadata revisions (size, mtime, ctime, device and inode) cannot detect a
same-size rewrite that lands within one timestamp tick.
`crates/catabolic-store/src/timestamps.rs` classifies each output's filesystem
with `fstatfs`:

| Class | Filesystems | Behavior |
| --- | --- | --- |
| Fine | APFS, tmpfs; Linux XFS, Btrfs, tmpfs, F2FS, ZFS | The metadata revision contract is unchanged. |
| Coarse local | HFS+; Linux ext2/3/4 | The recorded SHA-256 is verified. The result is cached only once both ctime and mtime are at least two seconds old. |
| Untrusted | FAT, exFAT, network, layered and unrecognized filesystems | The recorded SHA-256 is verified on every use and never cached. |

Two kinds of call verify digests this way:

- Artifact reuse through `usable_metadata`, which covers rendition admission,
  playback selection and component packaging.
- Content delivery of ready generated outputs through `with_transfer`.
  Verification runs after catalog ownership is released and before the first
  byte is sent. A mismatch is reported as `revision_mismatch`.

`with_opened` callers read metadata only, so they are not verified.

### Why the cache is safe

On coarse local filesystems the kernel sets ctime from the local clock, and
userspace cannot backdate it. FAT derives ctime from mtime, which userspace
can set, so FAT is untrusted rather than cached. Each cached verification
records both the wall clock and a monotonic clock. If the two disagree by more
than one second, the whole cache is discarded.

### Remaining limits

- A same-tick rewrite made while bytes are streaming is not fenced. The
  after-transfer metadata check can miss it.
- Someone who can step the system clock backwards and forwards between two
  lookups could defeat the cache. That requires root-equivalent privileges.
- On coarse and untrusted filesystems, digest verification adds a full read.
  Untrusted filesystems pay it on every use, including HEAD and Range
  requests that go through `with_transfer`.
- Source files have no recorded digest. They keep the metadata contract until
  the ctime and stat-time schema step (work plan item A5).

## Output claims never delete temporaries (FS1)

`claim_output` now creates the ownership marker directly, with
`O_CREAT|O_EXCL|O_NOFOLLOW`. It then fsyncs the marker and the output root. No
temporary name is ever created, so nothing is removed by pathname.

Frozen Python, and earlier native builds, could leave an interrupted
`.catabolic-claim-ID` temporary behind. The native engine never deletes one:

- If the marker already exists and is owned, the temporary is left in place.
- Otherwise, the claim refuses with an instruction to inspect and remove the
  file.

An incomplete native marker fails closed. Removing an obsolete
`.catabolic-link-ID` replacement link is now checked against the
destructive-leaf gate before it happens, so release builds refuse it.

## Recovery completes directory syncs (FS5)

Recovery sometimes finds that the intended result is already in place. Before
any journal operation is acknowledged, the root and every existing directory on
the operation's path are now fsynced, regardless of what recovery observed.
This covers both symlink and hardlink execution, and hardlink cancellation
after retention. Claim recovery also fsyncs the marker and the root.
Resolving a retention directory fsyncs the retention root and the
per-operation directory.

## Hardlink creation publishes only a verified staged link (FS2)

Hardlink creation used to validate a pinned source descriptor and then link the
source by name. A file substituted in between became the selected output.

Creation now links into `.catabolic-hardlink-<journal operation id>` in the
destination directory:

- On Linux the link targets the pinned inode itself, through
  `/proc/self/fd/N` with `AT_SYMLINK_FOLLOW`. If that inode has since been
  unlinked, the operation is refused.
- On other platforms the source name is linked without following symlinks.

The staged entry is checked against the recorded identity, size and mtime. It
is then published by exclusive rename and checked again. A mismatched staged
entry is preserved for inspection and never published.

Recovery reuses a staged entry left by an interrupted predecessor, but only
after verifying it. Cancellation refuses while a staged entry exists, because
the staged link may be the source inode's last reference.

Remaining limits:

- A writer in the output directory could replace the staged entry between
  verification and rename. The final check reports it, but the entry has
  already been published. Such a writer can already modify outputs directly,
  which falls outside exclusive output management.
- Frozen Python does not know about staging. If Python recovers an operation
  that native code staged, it creates the final link itself and leaves the
  staged entry behind. Handover qualification (work plan C7) has to account
  for this.

## Containment is re-verified after effects (FS6)

A directory opened during an operation could be moved outside the owned root
and still accept effects.

- **Source reads** re-walk the whole path from a freshly checked root after
  reading, instead of reopening the leaf through the old parent.
- **Symlink and hardlink effects** re-walk the destination path from the output
  root and require the same parent directory inode.

A moved directory is reported as `output directory moved during operation`.
An effect that has already landed outside the root is reported, not undone.

## Degraded event coverage no longer re-dirties every poll (FS7)

Native event monitoring could leave a source only partly subscribed. Causes
include a permission-denied subtree, the five-second registration deadline,
the watch or entry budget, or a missing root. Earlier builds re-reported that
source on every kernel wait, and every rebuild retry invalidated every source
again. Each 50 ms supervisor pulse advanced the durable dirty generation and
the watchers' pending generations, which fenced active scans even though
nothing had changed.

`crates/catabolic-runtime/src/source_events.rs` now keeps degraded coverage as
session state, with the time each source was last invalidated:

- **Transitions invalidate once.** These are a source that newly loses
  coverage (a failed registration or a disconnected root) and a degraded source
  whose coverage is restored. The backend clears a disconnected root's pin, so
  the disconnection is reported once.
- **Actual events still invalidate.** These are kernel hints in covered parts,
  lost or truncated intake, stream errors, configuration changes and rebuilds
  that directory changes require. Each of these still invalidates every source,
  as before.
- **Restoration retries do not invalidate.** Retries start at the existing
  one-second delay and double up to 30 seconds. The old subscriptions stay
  installed until the new set is registered, and are then drained, so a source
  that was covered loses no hint in the overlap.
- **Periodic reconciliation is bounded.** A degraded source is invalidated again
  at most once every 30 seconds, the frozen polling `watch` default interval.
  `source_events::invalidate_with_reconciliation` does this only after a scan
  has actually claimed the current generation (its job left `queued`). It also
  requires that no scan at that generation is still in flight. In flight means
  running under a live lease, or queued with at least one active (not
  cancelled) request. A queued job whose requests were all cancelled is
  abandoned: it neither blocks reconciliation nor counts as a claim. Each
  reconciliation scan can therefore finish while coverage stays degraded. An
  unclaimed generation is never advanced again. These results report
  `reason: degraded_coverage`.

Monitoring stays fail-closed. While any source is degraded, the report keeps
`complete: false`, `reconnecting: true`, `degraded_coverage: true` and the
source in `unavailable_sources`. The supervisor's report therefore stays
incomplete.

## Watcher events are admitted atomically with completion (FS8)

The frozen Python reference and earlier native builds commit a `watcher_changed`
event in one transaction and complete the run in another. A crash between the
two leaves `pending_reaction` set. The retry then emits again under its own
run ID, so notification deduplication (keyed by profile, event and run
identity) cannot merge the two. This defect is shared with Python. The native
engine corrects it without a schema change:

- `watcher::complete_evaluation` now admits the event and runs `finish`
  (the run row, completion generation, cleared `pending_reaction`) in one
  transaction. A crash can no longer separate them.
- Every planned run records `retry_of` (the `pending_reaction` it replaced),
  in both the frozen and native protocols. Before admitting, the native engine
  follows that chain back from the current run to the first run whose
  evaluation changed. All runs in the chain deliver that one change. If an
  event was already admitted under any of their identities, nothing is
  emitted. Otherwise the event uses the identity of the run whose evaluation
  changed. A retry that sees a newer change starts a new chain and is still
  notified. A missing or unreadable link fails closed
  (`watcher_transition_unresolved`) rather than minting a new identity.

The debug-only crash hook `CATABOLIC_TEST_WATCHER_CRASH_AT=before_finish`
exits the process at the old crash window. The test
`crates/catabolic-store/tests/watcher_event_crash.rs` covers four cases:

- a native crash followed by a retry;
- a retry after a split-protocol commit;
- a legacy commit, then a native retry that crashes, then a successful retry;
- an event committed under an unchanged legacy retry's own identity.

The stored `pending_reaction` value is unchanged, because it is observable
reference state.
