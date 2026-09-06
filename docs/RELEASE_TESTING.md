# Release verification

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Catabolic has three complementary test layers. A passing unit suite does not
substitute for installed-package, storage, or consumer acceptance.

## Fast regression suite

```sh
.venv/bin/python -m unittest discover -s tests -v
.venv/bin/ruff check src tests scripts
.venv/bin/ruff format --check src tests scripts
.venv/bin/python scripts/sync_docs.py --check
.venv/bin/catabolic spec check
```

Use FFmpeg and ffprobe for release verification. They remain optional runtime
dependencies; CI explicitly installs both so probe/decode regressions cannot
silently disappear behind optional-tool skips.

Coverage includes populated historical upgrades, backup preservation, actual
process interruption, filesystem ownership, final hardlink references, transient
retry history, source changes, and bulk-removal limits. Failure injection makes
specific races reproducible. It is complemented by actual filesystem tests below.

## Installed CLI and representative media

Build a wheel, install it in a fresh virtual environment, then run the harness.
The harness imports no Catabolic implementation modules and uses the installed
entry point from outside the checkout. FFmpeg and ffprobe must be on PATH.

```sh
python -m pip install --require-hashes --only-binary=:all: -r requirements/build.lock
python -m pip wheel --no-deps --no-build-isolation --wheel-dir dist .
python -m venv /tmp/catabolic-release-env
/tmp/catabolic-release-env/bin/python -m pip install --require-hashes --only-binary=:all: -r requirements/build.lock
/tmp/catabolic-release-env/bin/python -m pip install --require-hashes --only-binary=:all: -r requirements/runtime.lock
/tmp/catabolic-release-env/bin/python -m pip install --no-deps dist/catabolic-*.whl
/tmp/catabolic-release-env/bin/python -m pip check
python scripts/acceptance.py --cli /tmp/catabolic-release-env/bin/catabolic
```

`--root NEW_DIRECTORY` optionally selects the artifact destination. Existing
paths are refused. Otherwise a new temporary fixture is retained for inspection.
`report.json` records expected probe values, source hashes, platform, checks and
consumer status. A missing tool or failed assertion exits nonzero.

The generated corpus includes MKV/MP4 H.264 video, two audio languages, embedded
forced subtitles, external multilingual UTF-8 SRT, named chapters, FLAC tags, WAV,
PNG, PQ transfer metadata, Markdown text, and deliberately truncated/empty media.
PQ here tests metadata interpretation, not mastering-display metadata or visual
HDR correctness. Fixture expectations are specified independently of probe output.

The journey exercises scanning two sources, probing, cached repeats, decoding,
text search with locators, proposal acceptance, tags, sidecar discovery, copy
selection, symlink and hardlink output, SQL/GraphQL, checksums, and root replacement
and restoration. Source content is checked before and after. Small clips establish
behavior, not full-length video throughput or exhaustive codec compatibility.

## Actual storage boundaries

```sh
# macOS: creates and detaches two disposable HFS+ sparse images.
python scripts/storage_acceptance.py --cli /tmp/catabolic-release-env/bin/catabolic \
  --report /tmp/catabolic-storage-report.json

# Linux: creates private temporary tmpfs mountpoints; requires mount privileges.
sudo /absolute/path/to/python scripts/storage_acceptance.py \
  --cli /tmp/catabolic-release-env/bin/catabolic \
  --report /tmp/catabolic-storage-report.json
```

The harness never accepts an existing mount or catalog. It verifies actual device
boundaries, cross-filesystem hardlink rejection, source unmount with an accessible
empty mountpoint, output preservation, and explicit rebind/rescan after remount.
It detaches only mounts it created. If detaching fails it leaves the temporary
fixture intact rather than recursively deleting through a mounted filesystem.
These tests do not certify SMB/NFS reconnect behavior, NAS locking, or power loss.

## Real Jellyfin consumer

```sh
python scripts/acceptance.py --cli /tmp/catabolic-release-env/bin/catabolic \
  --jellyfin-image jellyfin/jellyfin:10.11.8
```

This starts a uniquely named disposable Docker container, mounts only the fixture
read-only, uses temporary server state, and binds its API to localhost. It never
accepts an existing server endpoint. The explicit official image version is part
of the report; `latest` is refused.

It provisions a movie library, delivers a refresh through Catabolic's outbox,
waits for discovery, checks the selected video/audio streams and English sidecar,
and checks bytes served by the static playback endpoint. Consumer scan completion
is verified separately from HTTP refresh acceptance. The container is removed
on success or failure. Logs and the fixture report remain available.

This verifies one Jellyfin version's scanner and static playback behavior. It does
not certify Plex, other Jellyfin versions, client rendering, transcoding, or NAS.

## CI gate

Release dependencies are pinned in `requirements/{build,runtime,dev,release}.lock` with
PyPI wheel hashes across platforms. CI installs those locks with hash enforcement
and installs Catabolic separately without dependency resolution. The build backend
and GitHub Actions are pinned as well. Update the pins and hashes together, run
`pip check`, and repeat packaged acceptance when changing them. Runtime pins cover
Python dependencies; they do not freeze the operating system or FFmpeg.

`python scripts/audit_dependencies.py --report dependency-audit.json` checks the
public package names and versions in these locks against OSV. Advisories or an
unavailable/malformed response fail the check; an unavailable service does not
count as a clean audit. CI requires this check. The checked build environment uses
pip 26.2, which includes the fix for CVE-2026-13346.

`.github/workflows/ci.yml` runs the regression suite on Linux/macOS and Python
3.11/3.14. A separate job builds one wheel used by the installed CLI, storage and
Jellyfin lanes. `release-verification` fails if any required lane failed or was
skipped. Artifact reports identify the platform and consumer version.

Configure repository protection to require `release-verification` before release
or merge. The CI workflow does not itself change protection settings or publish a
package. The separate release workflow below requires these checks and publishes
the same validated distributions only on explicit request. Local success alone
is not evidence that remote CI passed.

## Build a release without PyPI credentials

The **Build and release** workflow (`.github/workflows/release.yml`) is available
from GitHub Actions. Run it on a branch with `publish` left false to build and
validate packages without any PyPI account or secret:

```sh
gh workflow run release.yml --ref main -f publish=false
```

Pushing a `v*` tag also starts a build-only run. The tag must exactly match both
`pyproject.toml` and `catabolic.__version__`, for example `v0.1.0`. Branch-based
build-only runs still check that the two declared versions agree. Tag pushes do
not publish automatically.

The release workflow calls the same CI workflow used for branches and pull
requests. Its package job uses Python 3.14 and the hash-locked release tools to:

1. Check that bundled documentation matches the sources.
2. Build a source archive and then build the wheel from that archive using
   `python -m build --no-isolation`.
3. Run `twine check --strict` on both distributions.
4. Compare package metadata, licenses, migrations, schemas, source modules and
   offline guides with the checkout, and record artifact SHA-256 values.
5. Upload the wheel and source archive as `python-package-distributions`, with a
   separate `distribution-report` artifact containing the checksums.

The regression matrix and installed-wheel media, storage, and Jellyfin acceptance
lanes must all pass. They consume the wheel from that same build. The publishing
job downloads those exact distributions after the reusable validation workflow
succeeds; it does not rebuild them. Build or test failures prevent publication.

For a local equivalent, use Python 3.14 and a fresh output directory:

```sh
python3.14 -m venv .local-tests/release-env
.local-tests/release-env/bin/python -m pip install --require-hashes --only-binary=:all: -r requirements/release.lock
.local-tests/release-env/bin/python -m pip check
.local-tests/release-env/bin/python -m build --no-isolation --outdir .local-tests/release-dist
.local-tests/release-env/bin/python -m twine check --strict .local-tests/release-dist/*
.local-tests/release-env/bin/python scripts/check_distribution.py \
  --dist .local-tests/release-dist --ref refs/heads/main
```

The content checker expects exactly one wheel and its matching source archive.
Use a new output directory for another version; stale distributions fail the
check rather than being mixed into a release. The release tools are build-time
requirements and do not become Catabolic runtime dependencies.

## Enable PyPI publication later

Add a PyPI API token as `PYPI_API_TOKEN`, either in repository Actions secrets or
in the `pypi` GitHub environment. Use a token authorized for the intended PyPI
project. Name availability and project ownership must be established separately.
The workflow reports a missing token only when publication is explicitly requested.
No credential is required by its build or test jobs.

To publish, select an existing matching version tag and set `publish=true`:

```sh
# Publishes to real PyPI after all validation passes; configure the token first.
gh workflow run release.yml --ref v0.1.0 -f publish=true
```

A branch cannot be used for publication. Update both version declarations before
creating a new version tag. Publishing the same filenames again is not supported;
the workflow does not silently skip already-uploaded files. Inspect PyPI after a
partially failed upload before deciding how to recover.

The publishing job uses the `pypi` environment. Environment protection rules can
restrict who may publish and which tags are allowed. This workflow does not create
PyPI accounts, configure protection rules, or upload on an ordinary push. Its API
token mode does not produce OIDC publish attestations. A future switch to PyPI
Trusted Publishing can remove the stored token and enable those attestations.
