# Installation and application upgrades

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Catabolic is a Python command-line application. The supported operating-system
families are macOS and Linux, with Python 3.11 or later. Check the
[CI run for the revision you install](https://github.com/Jagalite/catabolic/actions)
for actual platform results. A configured platform lane is not evidence of a pass.

## Install with pip

A virtual environment keeps Catabolic's dependencies separate from other Python
applications. These commands require Git and an available Python interpreter:

```sh
python3 -m venv ~/.venvs/catabolic
. ~/.venvs/catabolic/bin/activate
python -m pip install 'git+https://github.com/Jagalite/catabolic.git'
python -m pip check
catabolic --version
catabolic docs
```

Activate the environment in a new terminal before using `catabolic`, or run
`~/.venvs/catabolic/bin/catabolic` directly. Installation does not initialize a
catalog, scan your drives, or create output libraries. Start with
[Getting started](GETTING_STARTED.md) after installing.

The URL installs the current default branch. To select a reviewed revision,
replace `COMMIT_SHA` with its full Git commit hash:

```sh
python -m pip install 'git+https://github.com/Jagalite/catabolic.git@COMMIT_SHA'
```

These instructions deliberately identify this repository as the source. They do
not assume a `catabolic` release is available on PyPI. Installing an unrelated
package with the same name is not equivalent.

## Install with pipx

If pipx is already installed and its application directory is on PATH:

```sh
pipx install 'git+https://github.com/Jagalite/catabolic.git'
catabolic --help
```

pipx creates a virtual environment and exposes the command automatically. Both
pip and pipx install the same Python application. npm is not an installation
interface for this repository; no Node.js wrapper is required.

## Install from a checkout or wheel

```sh
git clone https://github.com/Jagalite/catabolic.git
cd catabolic
python3 -m venv .venv
. .venv/bin/activate
python -m pip install .
```

For a wheel you have built or obtained from a trusted CI run:

```sh
python -m pip install /absolute/path/to/catabolic-0.1.0-py3-none-any.whl
python -m pip check
```

A wheel is the application package, not a bundled Python interpreter or media
library. pip resolves its Python dependencies unless you supply and install the
locked dependencies yourself. See [release verification](RELEASE_TESTING.md)
for the hash-enforced build and installation procedure.

## Optional tools

| Capability | Extra requirement |
| --- | --- |
| Inventory, curation, tags, SQL, GraphQL, links, manifests | No media executable or API credential |
| Signature inspection (`sniff`) | Python standard library; reads at most 4096 bytes |
| Container/stream inspection (`probe`) | `ffprobe` available on PATH |
| Full audio/video decoding (`decode`) | `ffmpeg` available on PATH |
| calibre / Calibre-Web import | Official `calibredb` executable |
| Immich import | Official `immich` CLI and `IMMICH_API_KEY` |
| TMDB movie candidate retrieval | `TMDB_TOKEN` supplied in the environment |
| Jellyfin refresh delivery | Server endpoint and configured credential environment variable |

Install external programs separately using their upstream instructions or your
system package manager. Catabolic does not bundle FFmpeg or ffprobe. Their
availability affects the relevant operation, not whether you can catalog files.

## Choose storage and configuration

Every stateful command needs an explicit database, either `--db PATH` before the
command or `CATABOLIC_DB`. There is no automatically discovered default database.
Use an absolute path in scripts, and keep the SQLite database on a local disk.
Source roots may be mounted remote storage.

```sh
mkdir -p ~/catabolic-workspace/state
export CATABOLIC_DB="$HOME/catabolic-workspace/state/catalog.sqlite3"
export CATABOLIC_PROFILE=default
catabolic init
```

Keep state outside source/output trees. Profiles describe bindings for different
machines; they do not provide remote database replication or concurrent NAS
SQLite access. See [operations](OPERATIONS.md).

## Upgrade the application, then inspect the database

For a Git installation whose project version has not changed, explicitly
reinstall the selected revision so pip does not retain an earlier build:

```sh
# Run inside the pip virtual environment; replace COMMIT_SHA first.
python -m pip install --upgrade --force-reinstall \
  'git+https://github.com/Jagalite/catabolic.git@COMMIT_SHA'
python -m pip check
catabolic --version
catabolic db status
catabolic db upgrade --dry-run
```

For pipx, `pipx reinstall catabolic` reinstalls its recorded source; a commit-pinned
source remains pinned. Use `pipx install --force` with the newly selected URL to
change that source. Follow your installed pipx version's help for its options.

See the upstream [pip VCS guide](https://pip.pypa.io/en/stable/topics/vcs-support/)
and [pipx CLI reference](https://pipx.pypa.io/stable/reference/cli.html) for source
selection and reinstall behavior.

Updating the executable does not automatically migrate the database. After
reviewing the upgrade plan, run `catabolic db upgrade`. A pending upgrade creates
a verified backup and rehearses the entire migration chain before committing.
A no-op upgrade does not make a new backup. See [migrations](MIGRATIONS.md) for
recovery, storage requirements, and downgrade limits.

## Uninstall

Use `python -m pip uninstall catabolic` in the installation environment, or
`pipx uninstall catabolic`. These uninstall the program; they do not remove your
catalog database, backups, source media, or generated folders. Keep the database
and ownership state if you intend to resume managing those outputs.
