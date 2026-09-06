# Release verification

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
python -m pip wheel --no-deps --wheel-dir dist .
python -m venv /tmp/catabolic-release-env
/tmp/catabolic-release-env/bin/python -m pip install dist/catabolic-*.whl
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

`.github/workflows/ci.yml` runs the regression suite on Linux/macOS and Python
3.11/3.14. A separate job builds one wheel used by the installed CLI, storage and
Jellyfin lanes. `release-verification` fails if any required lane failed or was
skipped. Artifact reports identify the platform and consumer version.

Configure repository protection to require `release-verification` before release
or merge. The workflow does not itself change protection settings or publish a
package. A future publishing workflow must depend on this gate and publish the
same validated wheel. Local success alone is not evidence that remote CI passed.
