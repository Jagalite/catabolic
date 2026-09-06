# Development and documentation maintenance

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Catabolic is a Python application with a small CLI adapter, application use cases,
a SQLite store, and separate query, layout, reconciliation, processing, and
interchange modules. Start with [architecture](DESIGN.md) and use the existing
ownership boundaries when changing behavior.

## Set up a checkout

```sh
git clone https://github.com/Jagalite/catabolic.git
cd catabolic
python3 -m venv .venv
. .venv/bin/activate
python -m pip install --require-hashes --only-binary=:all: -r requirements/build.lock
python -m pip install --require-hashes --only-binary=:all: -r requirements/dev.lock
python -m pip install --no-deps --no-build-isolation -e .
python -m pip check
```

The locks include platform wheel hashes; update versions and hashes together.
The editable install points the CLI at this checkout. Optional FFmpeg/ffprobe are
needed for release acceptance and complete probe/decode coverage.

## Run checks appropriate to a change

```sh
python -m unittest discover -s tests -q
ruff check src tests scripts
ruff format --check src tests scripts
python scripts/sync_docs.py --check
catabolic spec check
python scripts/check_spec_releases.py origin/main
```

The last command compares already released schema/reference artifacts with a
base commit. Choose the actual review base for your branch. Do not edit released
SQL migrations, schema JSON, or frozen reference text to make a check pass.

Tests create temporary data; they do not use a real media library. Some tests
bind local HTTP servers or exercise subprocesses, so a restricted environment
may need permission for those operations. A skipped or blocked test is not a pass.
Use focused test modules during development, and run the full relevant release
lanes for publication. See [release testing](RELEASE_TESTING.md).

## Test workflows and scale

```sh
python -m unittest tests.test_synthetic_library -v
python -m tests.synthetic_benchmark --sizes 100 1000 5000
```

The synthetic library includes several source trees, Unicode names, sidecars,
empty/partial files, exclusions, collisions, and interruption recovery. Reports
and fixtures remain under `.local-tests/` for inspection. Small payloads measure
catalog/filesystem overhead, not full-length media throughput.

[SCALE_BENCHMARKS.md](SCALE_BENCHMARKS.md) describes higher-cardinality database
and real-file benchmarks. [RELEASE_TESTING.md](RELEASE_TESTING.md) covers installed
wheels, real media, filesystem mounts, and a disposable Jellyfin consumer. Use
those independent evidence layers before claiming compatibility or speedups.

## Where changes belong

| Area | Main files |
| --- | --- |
| CLI arguments and output | `src/catabolic/cli.py`, `enrichment_cli.py` |
| Catalog use cases and records | `app.py`, `store.py`, `media_catalog.py`, `curation.py` |
| Filesystem inventory and ownership | `filesystem.py`, `scan_staging.py`, `reconcile.py`, `hardlinks.py` |
| Naming and membership | `layouts.py`, `selection.py`, `copy_selection.py` |
| SQL and GraphQL | `sql_query.py`, `graphql_query.py` |
| Processing and source checks | `processing.py`, `process_runner.py`, `source_access.py` |
| External adapters | `network_adapters.py`, `http_worker.py`, `importers.py` |
| Database upgrades | `migration.py`, `migrations/` |
| Manifest contract | `manifest.py`, `interchange/` |
| Offline guides | `documentation.py`, `guides/`, `scripts/sync_docs.py` |

Most file names in the table are relative to `src/catabolic/`. Prefer application
interfaces over direct ad hoc database writes. Source safety and output ownership
checks must apply to alternate execution paths, not only the main CLI workflow.

## Database and specification changes

Add a new numbered SQL migration for persistent schema changes. Extend supported
schema descriptions and preservation tests, including populated old databases,
backup verification, failure rollback, and pending-operation behavior.
Migration file contents are checksummed; SPDX notices for existing files are
stored in sidecars so those bytes remain unchanged. See [migrations](MIGRATIONS.md).

The Open Catalog contract has frozen typed models, JSON Schemas, and generated
field references. Add a version for incompatible changes; do not overwrite
released artifacts or silently discard unknown JSON fields. A database migration
does not automatically imply a manifest version change. See
[the specification guide](OPEN_CATALOG.md).

## Keep documentation in one place

`README.md` and the guides in `docs/` are the editorial sources. `documentation.TOPICS` controls
the installed guide index. After editing or adding a guide:

```sh
python scripts/sync_docs.py
python scripts/sync_docs.py --check
python scripts/sync_wiki.py --output .local-tests/wiki-export
python scripts/sync_wiki.py --output .local-tests/wiki-export --check
```

The wiki generator exports the same guide bodies, rewrites repository-relative
links for wiki navigation, and creates Home, a sidebar, and a footer. It does not
publish anything or delete unrelated wiki pages. `--check` detects stale or missing
managed pages. Edit the guides in `docs/` and regenerate; direct changes to managed
wiki pages would be replaced on the next export.

For an initialized GitHub wiki, clone it into a separate ignored checkout, export
there, review its diff, and push its default branch:

```sh
git clone git@github.com:Jagalite/catabolic.wiki.git .local-tests/wiki
python scripts/sync_wiki.py --output .local-tests/wiki
# Review before publishing:
git -C .local-tests/wiki diff --stat
git -C .local-tests/wiki status --short
```

GitHub requires a first wiki page before that Git repository exists. Create Home
through its web interface once, then use the normal wiki Git workflow. See
[GitHub's wiki instructions](https://docs.github.com/en/communities/documenting-your-project-with-wikis/adding-or-editing-wiki-pages).

New project files need SPDX copyright/license notices using valid comment syntax.
Use adjacent `.license` files for JSON and checksum-sensitive artifacts. Keep
third-party notices intact and verify package license files when changing builds.
Documentation should distinguish implemented behavior, measured results, and
future roadmap ideas.

## Release status

The CI workflow validates changes; the release workflow builds without credentials
and can publish to PyPI later through an explicit tagged invocation. See
[release testing](RELEASE_TESTING.md) for token setup and commands. Neither workflow
changes repository branch protection. Check the exact commit's required jobs and
artifact reports. Ship the same artifact that passed packaged acceptance, and
record any remaining platform/consumer limitations. Audit reports describe their
recorded snapshot, not a perpetual certification of every later commit.
