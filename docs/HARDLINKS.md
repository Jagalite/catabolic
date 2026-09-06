# Hardlink outputs

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Catalogs default to symlinks. Hardlinks are an explicit, catalog-wide option;
profiles may bind that catalog to different roots but share its link mode.
Naming presets and SQL/GraphQL selections work with either mode.

## Configure and preview

Existing databases require the verified schema **5** upgrade. It adds separate
hardlink ownership and retention tables without changing existing symlink records.

```sh
catabolic --db catalog.sqlite3 db upgrade --dry-run
catabolic --db catalog.sqlite3 db upgrade
export CATABOLIC_DB=/absolute/path/catalog.sqlite3

# The output directory must already exist and be on the source filesystem.
catabolic catalog bind plex-hard --root /Volumes/media/plex-hard --link-mode hardlink
catabolic layout put plex-hard --preset plex-v1
catabolic layout preview plex-hard --catalog plex-hard
catabolic layout apply plex-hard --catalog plex-hard
catabolic sync --catalog plex-hard --dry-run
catabolic sync --catalog plex-hard
catabolic verify --catalog plex-hard
catabolic catalog list
```

The source and output directories must be disjoint; register the source subtree,
not a parent containing the output. Omitting `--root` still uses the normal
`./catabolic/NAME` default, which must be on the same filesystem as every selected
source. Omitting `--link-mode` preserves an existing catalog's mode.

The layout plan previews naming/mappings; `sync --dry-run` performs live source,
filesystem, ownership and final-reference checks. A preflight blocker prevents all
filesystem changes in that selected sync scope. Execution revalidates each step.

Multiple source directories can feed one hardlink output if they are on the same
filesystem. Different drives or filesystem boundaries cannot be combined into one
hardlink output. Preflight checks source devices and existing output parents;
execution also checks the actual destination parent and handles kernel `EXDEV`
errors. There is no implicit fallback to copying or symlinks. Filesystem and
permission rejections report a clear error and preserve any pending journal.

Changing modes is refused while any profile has owned links, retained hardlink
records or pending operations for the catalog. Use a separate catalog for the new
mode. Existing symlink libraries are not automatically converted.

## Shared data and final-reference protection

A hardlink is another filename for the same underlying file. Source bytes, inode,
and modification time are not rewritten by link creation, but link count and inode
change time change. Permissions and in-place writes are shared with the source.
Hardlinks are not independent backups or protection from application edits.

Before retiring or replacing an owned hardlink, Catabolic checks the live link
count. If it is **1**, the operation is blocked with `last hardlink protected`.
The file stays at its existing path. This also applies after a scan confirms the
source filename is missing. The count is checked again immediately before moving
an output. Creating an independent copy does not raise the original inode's link
count; Catabolic does not infer that a backup makes deletion safe.

Even a count greater than one is not sufficient evidence for safe deletion:
another process could unlink the source after the check. Therefore Catabolic
**never unlinks retired media hardlinks**. It moves each retired output into:

```text
OUTPUT/.catabolic-retained/OPERATION_ID/data
```

This uses an atomic rename that refuses to overwrite an existing destination,
including one created concurrently. The record retains the original output path,
source occurrence and device/inode evidence. If the source disappears during the
operation, the retained filename still preserves the data. Replacement moves the
old link into retention before creating the new one; the visible output may be
briefly absent. Failed replacements retain the old data for recovery.

Both the retention root and its operation directories reject symlink traversal
and filesystem crossings. Retirement requires exclusive rename support:
`renameatx_np(RENAME_EXCL)` on macOS or `renameat2(RENAME_NOREPLACE)` on Linux.
An unavailable API or unsupported filesystem blocks retirement without an unsafe
fallback. The local test suite exercises macOS; Linux behavior is covered by the
configured CI lane when run, not a claim of local Linux validation.

## Review retained data

```sh
catabolic --json catalog retained plex-hard --limit 100
# Use the returned next_after value for subsequent pages:
catabolic --json catalog retained plex-hard --after OPERATION_ID
catabolic query "SELECT * FROM catalog_retained_hardlinks WHERE profile=:profile AND catalog='plex-hard'"
```

Lists contain recorded retention paths relative to the output root, original
paths, source/inode evidence and timestamps. They are database observations, not
live verification. Sync/verify reports include retention counts and warnings.
Retained files can become the only remaining reference and continue consuming
space after the source is deleted. **There is no purge command or automatic
retention expiry.** Review the content and independent backups before doing any
manual filesystem cleanup; a link-count snapshot cannot rule out concurrent changes.
Retained ownership records are deliberately preserved, including if someone moves
or deletes the retained data outside Catabolic. Use a new catalog rather than
rebinding one with retained records.

The reserved `.catabolic-retained` tree is internal recovery storage, not selected
library content. Media filenames inside it are `data`, with their original names
in the database/manifest. Exclude this tree if a consuming application scans hidden
folders. The output and retention directories must remain under Catabolic's
exclusive management; external deletion or modification is outside its control.

## Recovery

Creation, replacement and retirement use the existing durable operation journal.
Recovery checks regular-file device/inode identity instead of interpreting a
symlink target. Files that appeared unexpectedly are not adopted or overwritten.

```sh
catabolic recover --catalog plex-hard
catabolic sync --catalog plex-hard --dry-run
```

A source change may require a new scan before recovery can determine whether a
pending operation is obsolete. If creation was rejected by the filesystem, or an
unstarted retirement is now blocked by the final-reference check, intent can be
explicitly cancelled:

```sh
catabolic recover --catalog plex-hard --cancel-unapplied
```

Cancellation applies to hardlink operations only. It is allowed when creation has
not happened, the original output is untouched, or the old output was safely
retained and no replacement exists. Completed creations are recovered normally.
Cancellation never deletes a regular file; it records any completed retirement
before clearing the journal. Then inspect with `sync --dry-run` and correct the
configuration or source state. Foreign/replaced files require manual inspection.

## Queries, manifests and exports

`catalog_outputs` exposes `catalog` and `link_mode`. `catalog_hardlinks` exposes
recorded regular-file ownership; `catalog_retained_hardlinks` exposes retained data.
Hardlink targets in these views are JSON source/device/inode evidence, not paths
that can be followed as symlinks. GraphQL `catalogs { nodes { id linkMode } }`
reports each catalog's mode.

Manifest **v3** adds `catalog.link_mode`, typed `hardlinks` and
`retained_hardlinks` collections and corresponding counts. Device/inode values are
decimal strings. In a hardlink catalog, symlink target/comparison fields are null
and `recorded_links` is empty. Owned/retained records may refer to source occurrences
outside the active exported selection. Manifest v1 and v2 remain readable and
frozen; the producer emits v3, without implicitly downgrading or dropping data.

`export --output` still produces a detached **symlink** bundle and preserves the
original catalog manifest as provenance. To manage a hardlink folder, bind a
hardlink catalog and use layout/apply/sync. Exporting metadata never deletes,
recreates or purges links.
