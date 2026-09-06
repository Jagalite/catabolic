# Troubleshooting

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Start with the exact database, profile, and catalog involved. A file that is
healthy in one profile or output does not establish another profile's status.
These commands inspect state; preview does not repair it:

```sh
catabolic --version
catabolic --db /absolute/path/catalog.sqlite3 --json status
catabolic --db /absolute/path/catalog.sqlite3 --json db status
catabolic --db /absolute/path/catalog.sqlite3 --json sync --catalog plex --dry-run
catabolic --db /absolute/path/catalog.sqlite3 --json verify --catalog plex
```

Replace the database and `plex` scope first. Exit 3 is a meaningful diagnostic,
not an invitation to bypass the reported blocker.

## Installation and command problems

| Symptom | Check and next step |
| --- | --- |
| `catabolic: command not found` | Activate the pip environment, invoke its absolute executable path, or check pipx's PATH setup. |
| Database path required | Supply global `--db PATH` or export `CATABOLIC_DB`; commands do not guess a database. |
| Unknown option | Put global flags before the command and check the installed subcommand's `--help`. |
| `layout plan` rejected | Use `layout preview NAME --catalog NAME`. |
| Expected JSON but received text | Check exit status and stderr; argparse errors are text. GraphQL errors use stdout JSON. |
| Database needs an upgrade | Run `db status`, then `db upgrade --dry-run`; inspect pending work before an explicit upgrade. |

## A scan is incomplete or a mounted drive looks empty

A root binding includes filesystem identity, not just a string path. An unmounted
volume can leave an empty mountpoint with a different device/inode. Catabolic
blocks this state rather than publishing an empty library. Restore the intended
mount and inspect its contents before deliberately rebinding anything.

An inaccessible directory, an unexpected filesystem boundary, or directory changes
during traversal can make scanning incomplete. The previous inventory remains
intact. Register nested mounts as separate locations. Do not turn permission
errors into broad exclusions merely to produce a successful scan.

For known irrelevant protected directories, exclusions are exact source-relative
paths or subtrees, not glob patterns:

```sh
catabolic scan --exclude .Spotlight-V100 --exclude .Trashes --exclude .DS_Store
```

Repeat the intended exclusions on later scans. Previously observed excluded
entries are preserved, not marked missing. Other hidden files remain in scope.

## There are files, but the output is empty

Scanning creates inventory. A layout needs active identifications and matching
rules. Check the separate stages:

```sh
catabolic --json files --unidentified
catabolic --json association list
catabolic layout show plex
catabolic layout preview plex --catalog plex
catabolic --json mapping list --catalog plex
```

Choose the real saved layout and catalog names. Required title/year or parent
relationships may be absent. A rule may skip unsupported extensions or roles.
`layout apply` must succeed before sync has generated mappings to use. Merely
naming a catalog `plex` does not choose a Plex preset.

`files --unmapped --catalog plex` and `files --unidentified` ask different
questions: output membership versus independent identification. Disabling a
mapping does not erase the identification.

## Output collisions or a root cannot be claimed

An output must be empty when Catabolic first claims it. A directory full of
previously generated links from another tool is not automatically adopted. Bind
a fresh empty output and compare results before changing the old library.

If a bound output was replaced or manually modified, inspect it and its ownership
marker. Do not delete the marker or overwrite the conflicting entry as a routine
fix. Catabolic must distinguish its own outputs from somebody else's files.

A naming collision usually means two associations produce the same destination.
Choose a preferred copy, model distinct editions, or customize the path. Catabolic
does not silently pick a winner. See [layouts](OUTPUT_LAYOUTS.md) and
[copy policies](ENRICHMENT.md).

## The links work on the host but not in Plex or Jellyfin

Both the generated link and its target must resolve in the consumer's filesystem
namespace. Relative symlinks depend on the relative relationship between output
and source, even when the consumer is in a container.

For example, if the host layout is:

```text
/srv/library/source/Movies/Film.mkv
/srv/library/catabolic/plex/Movies/Film/Film.mkv → relative path into source/
```

mounting `/srv/library` as `/library` in the container preserves that relationship.
Mounting only `/srv/library/catabolic/plex` does not expose the target. With sources
on several drives, provide a consistent namespace for every relevant path. Check
read/search permissions and the consuming application's symlink scan settings.

Catabolic verification checks its own namespace. Validate discovery and playback
inside the application too. See [compatibility](COMPATIBILITY.md).

## Missing source, old metadata, or unexpected query counts

SQL/GraphQL `present` is recorded evidence, not a live filesystem check. Refresh
with a complete scan. An unavailable source can block sync until evidence is
resolved. A confirmed missing source may retire its owned symlink while preserving
the active mapping, allowing later restoration.

Profile-dependent SQL views contain all profiles. Filter `profile=:profile`.
Joins over identities, mappings, tags, and associations may multiply rows; use
`COUNT(DISTINCT file_id)` when counting file occurrences. This count is not
content deduplication. Run hashing before using checksum duplicate reports.

A manifest is a desired-state snapshot. It does not refresh saved selections or
prove live health. Run layout preview/apply, sync, verify, then manifest export.
Use `--replace` to update a changed valid manifest owned by that catalog.

## Interrupted commands and retained hardlinks

```sh
catabolic status
catabolic recover --catalog plex
catabolic sync --catalog plex --dry-run
```

Inspect pending work before changing desired mappings. A blocked recovery needs
its actual source/output problem resolved. A completed scan may show that an
unapplied operation became obsolete.

`last hardlink protected` means the output is the last live reference to that
inode. Catabolic refuses retirement. Other retired hardlinks are moved to
`.catabolic-retained`, not unlinked. They can still consume storage and can later
become the final reference. There is no automatic purge. Read [hardlinks](HARDLINKS.md)
before manual changes; an independent backup does not change an inode's link count.

## Slow probing or failed imports

Use `sniff` first when a signature is sufficient. Limit global workers and per-device
concurrency; group locations sharing a NAS. Inspect `process show JOB_ID` and
`process attempts JOB_ID` before retries. A probe timeout and an invalid-file
failure have different meanings. Hashing and full decoding read much more than
signature inspection.

An import timeout may occur after the external application accepted the media.
Inspect `outcome`, confirmed groups, and the destination. Do not automatically
rerun an `external_outcome_unknown` result. [Enrichment](ENRICHMENT.md) and
[imports](COMPATIBILITY.md) describe the supported retry boundaries.

## Reporting a reproducible problem

Include the CLI version, OS/Python versions, database schema, exact command with
sensitive values removed, exit code, and relevant JSON result. State whether the
source is local, SMB/NFS, removable, or container-mounted. A small synthetic
fixture is more useful than a full media library. Manifests contain local paths
and metadata, so review them before attaching one to a public issue.
