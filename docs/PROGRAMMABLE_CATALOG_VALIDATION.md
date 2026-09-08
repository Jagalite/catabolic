# Programmable catalog validation

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Validation date: 2026-09-08. Implementation baseline: `5d90764`; redesigned
database schema: **16**. These are local macOS/Python 3.14.6 results on disposable
fixtures. The existing Linux/macOS and Python 3.11/3.14 CI matrix remains intact;
this record does not claim that remote matrix has run for uncommitted changes.

## Regression and compatibility

`python -m unittest discover -s tests -q` passed **466 tests** in 69.444 seconds.
Fourteen new tests cover saved query contracts/composition and budgets, SQL and
GraphQL gap parity, machine envelopes, analysis deduplication and retry, actual
FFmpeg convergence, explicit video/audio rendition inputs, stale ancestors,
external receipts/fencing and bounded HTTP execution outside the writer lock,
projection preview rollback/repeat/empty guards, and populated schema-14 upgrade
and rollback. The existing recovery, collision, unavailable-source, required-work,
duplicate-receipt, disabled-rule, changed-recipe, scan and filesystem tests remain.

The end-to-end fixture proves:

1. A saved missing-rendition query selects an original.
2. A rule queues and executes a retained job with an immutable operation.
3. The accepted output records source revision and lineage.
4. The original gap query becomes empty.
5. Another query selects the generated occurrence.
6. A projection admits it and uses ordinary journaled reconciliation.
7. An unavailable destination blocks publication; a later retry needs no render.
8. Repeated rule/projection runs preserve outputs and create no extra attempt.

Migration tests compare every original column and row, preserve rule/job IDs and
foreign keys, retain embedded definitions, and verify that adopted query content
reuses its migrated ID when saved again. A failed table rebuild leaves the
original database intact. Released migrations 1–14 are unchanged.

Ruff lint and formatting, Python compilation, bundled-guide synchronization,
`catabolic spec check`, and preservation of all **14 frozen interchange artifacts**
passed. No static type checker is configured; runtime typed interchange and
schema validation are covered by the existing suite. The repository's OSV audit
completed with no advisories for its 44 pinned packages at execution time.

## Installed and consumer acceptance

The final installed implementation passed **168 CLI commands** in 51.095 seconds
in a clean wheel environment with pinned runtime dependencies, outside an editable
source install. This includes the programmable query/rule/projection journey,
real FFmpeg processing, receipts, refresh, verification and source-hash checks.
Jellyfin **10.11.8** passed scan, selected-copy, audio-stream, external-subtitle,
static-playback-byte and refresh-delivery checks. Actual macOS mount tests passed
cross-filesystem rejection, unmount preservation and explicit rebind after remount,
with no cleanup errors. The wheel and source distribution passed strict metadata
checks and byte comparison of all **151 packaged source/data files**; `pip check`
passed. Only this validation documentation was finalized after the runtime replay.

The retained messy-collection reference journey also passed **84 commands** in
18.150 seconds: 10/10
resolvable identities, three explicit ambiguities, stable repeat publication,
verified interruption/outage recovery and unchanged source hashes. This is a
scripted reference run. **Agent judgment was not measured**; no blind trials or
verified agent isolation are claimed by this redesign.

Jellyfin evidence concerns the existing consumer-specific fixture. The new
programmable projection is exercised separately with a flat layout. It does not
certify every layout, rendition policy, codec or target application.

## Scale evidence

Command:

```sh
python tests/scale_benchmark.py --root NEW_DIRECTORY \
  --sizes 10000 100000 --filesystem-sizes 1000 --repeats 1 --memory-mib 768
```

| Saved missing-output query | Complete IDs | Pages | Worker seconds | Peak RSS MiB |
| --- | ---: | ---: | ---: | ---: |
| 10,000-row fixture | 10,000 | 10 | 0.0761 | 51.39 |
| 100,000-row fixture | 100,000 | 100 | 0.5318 | 64.31 |

Both queries use an explicit 100,000-ID bound and deterministic pages. Database
fixtures are synthetic catalog state, not scans of 100,000 real media files. The
100,000-entry manifest deliberately reports `guarded` because its related-item
closure exceeds the record limit; this is not successful complete export. Its
peak RSS was 586.45 MiB under the 768 MiB cap.

The separate 1,000-file filesystem fixture verified 1,000 links, unchanged inodes
on repeat, ten explicitly budgeted removals with 990 surviving inodes unchanged,
and successful recovery after an injected crash. Source files remained unchanged.
These are one-trial local measurements with other validation work running during
the session, not a NAS benchmark or sustained performance guarantee. The larger
existing benchmark lanes remain available.

## Reproducing and locating evidence

Use the repository CI commands for lint, tests, package build, dependency audit
and frozen contracts. Build with `python -m build --no-isolation`, validate with
`python -m twine check --strict DIST/*` and `scripts/check_distribution.py`, then
install the wheel with `--no-deps` into an environment populated from the
hash-locked runtime requirements. Pass its executable explicitly:

```sh
python scripts/acceptance.py --cli INSTALLED_CLI --root NEW_DIRECTORY \
  --jellyfin-image jellyfin/jellyfin:10.11.8
python scripts/experience_acceptance.py --python INSTALLED_PYTHON --root NEW_DIRECTORY
python scripts/experience_evaluator.py --root EXPERIENCE_DIRECTORY
python scripts/storage_acceptance.py --cli INSTALLED_CLI --report REPORT.json
```

Local evidence is retained under `.local-tests/programmable-review/` (review
inventory, `regression-final.log`, audit, `storage-final.json`, independent
experience evaluation and distribution checks),
`.local-tests/programmable-acceptance-final/report.json`,
`.local-tests/programmable-experience-final/report.json` and
`.local-tests/programmable-scale-final/report.json`. These ignored, machine-local
fixtures are not portable repository artifacts. The commands and new assertions
are committed source material when this work is committed.

## Remaining scope

SQL retains arbitrary predicates; GraphQL provides common rendition-gap filters
and bounded evidence/discovery rather than complete SQL-filter parity. Recorded
currentness is immediate snapshot evidence, not live recursive source checking.
Live admission validates the full bounded ancestry. Programming definitions are
local database configuration; portable definition interchange remains follow-up.

Native AV1/Opus presets, HDR-to-SDR conversion, waveform generation, normalization
and general document/OCR extraction are not added. Existing external receipts are
the extension path for externally generated files. There is no new DAG scheduler,
recursive event executor or automatic query-trigger daemon. Analysis and external
runtime/storage estimates remain explicitly unknown where no defensible estimate
exists. See [the guide](PROGRAMMABLE_CATALOG.md) for operational bounds and
compatibility details.
