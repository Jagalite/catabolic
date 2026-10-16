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
