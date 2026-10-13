# Recognizable orphaned SQLite sidecar refusal

This narrow native content-admission change addresses the previously recorded
WAL/SHM alias gap. It is not a replacement for the blocked broad filesystem or
source-access review and does not close the content/HTTP release gate.

The existing owner refuses registered private paths/inodes and live catalog,
WAL, SHM and journal inode aliases. Those checks cannot identify an unregistered
hardlink after SQLite deletes the original sidecar name. The new descriptor
check, shared by `with_opened` and `with_transfer`, also refuses:

- the existing 16-byte SQLite database signature
- either four-byte, big-endian SQLite WAL magic value
- the four-byte SQLite wal-index version 3007000 in either byte order

The WAL magic and wal-index layout are taken from the pinned bundled SQLite
source in `libsqlite3-sys` 0.38.2 (`WAL_MAGIC`, `WALINDEX_MAX_VERSION`, and
`WalIndexHdr.iVersion`). No database schema, registration lifecycle, source
descriptor protocol, private inode persistence, or filesystem mutation changes.

This is a conservative refusal, not a format validator. An ordinary file whose
prefix coincides with a refused value is refused too. Truncated, zeroed,
modified, future-format, or otherwise unrecognizable sidecars are not proven
safe. The private-content release blocker therefore remains open. The frozen
Python implementation lacks the new prefix refusal; this is an explicit native
fail-closed improvement, not an exact-parity claim.

## Disposable witnesses

The Rust owner fixture creates an actual WAL-mode catalog, hardlinks its WAL and
SHM into an exposed disposable source, closes SQLite, and verifies both original
sidecar names have disappeared and both aliases now have one link. It reopens
the catalog, scans the aliases through the existing observation owner, and
confirms no private-file registration exists. Both descriptor-serving paths must
return `private_content` before invoking any preparation or delivery callback.
The unchanged owner admitted the WAL alias in the retained pre-fix run.

Separate controls cover both WAL values, both wal-index byte orders, recognizable
prefixes with absent/arbitrary trailing bytes, and normal content through both
descriptor paths. Explicit unrecognized-prefix controls retain the documented
limit rather than claim comprehensive orphan detection.

Local evidence is retained under `.local-tests/private-sidecar/` in the scoped
worktree. The first fixture attempt incorrectly added a schema table and was
rejected by the existing schema guard; that attempt is retained separately from
the corrected pre-fix witness. Final source-bound results and independent narrow
review are recorded with the checkpoint, not inferred from fixture presence.

The source-bound [checkpoint receipt](../tests/parity/private-sidecar-header-checkpoint.json)
records six passing Rust owner tests and an independent six-test rerun, with no
ignored cases. Formatting, scoped Clippy with warnings denied, bundled-doc sync
and whitespace checks passed. This is not actual HTTP or full-suite acceptance.
The independent review found no actionable issue within this exact diff and did
not perform the blocked broader review.
