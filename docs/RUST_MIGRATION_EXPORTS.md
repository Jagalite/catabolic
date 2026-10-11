<!-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors -->
<!-- SPDX-License-Identifier: MIT -->

# Native target, metadata export and official-import checkpoint

These opt-in companion operations retain Python as the default. They do not
close M4, the blocked independent definition/filesystem review lanes, full CLI
or Python-facade compatibility, real third-party application acceptance, or
platform/release gates.

## Owners and boundaries

- Store `targets` embeds the frozen versioned target definitions and descriptions
  with provenance: 20 targets and 17 independent layout definitions.
- Store `exports` builds XSPF, OPDS and NFO metadata without filesystem effects.
  Tests compare complete bytes, including Unicode, XML escaping and forbidden
  characters, Python scalar spelling, OPDS field order, and episode relationships.
  Native construction has a 128-MiB metadata budget. Complete arbitrary Python
  object repr and every urllib URL-parser edge are not claimed qualified.
- Store `export_bundle` publishes a new detached bundle, never adopts or replaces
  an existing destination. Portable path/casefold/NFC and parent collisions are
  preflighted, bound-root overlaps are refused, metadata is exclusively created,
  and relative media links plus the manifest are synced. Source bytes are not
  copied or changed. This is not qualification of the separate managed-output
  concurrent-leaf-writer safety gap.
- Store `imports` owns official-application admission and source snapshots;
  runtime `imports` releases the catalog session while staging and invoking the
  official `calibredb` or `immich` CLI. Calibre/Calibre-Web group each logical
  book's formats; Immich handles one photo/video per attempt. Tools receive only
  private copies, never original source paths. Snapshot and descriptor revisions
  are checked before and after copying. Immich environment options are cleared
  except the explicitly required key and instance URL; diagnostics are discarded.
- Launch is an explicit outcome boundary. Before launch a failed attempt may be
  safely retried only if no earlier group completed. After launch failure or
  interruption returns `external_outcome_unknown`, never an automatic retry.
  Completed groups remain in the report. CLI SIGINT/SIGTERM cancels and awaits the
  native process owner; the Python boundary polls Python signals. Staging is
  removed on every exit. The installed Python cancellation path requires its own
  qualification in addition to the independently checked native CLI path.

Companion actions are `target list/describe/definitions`, `export build/publish`,
and `import catalog`, using the existing `operation SURFACE ACTION --options`
JSON entry point. Official imports are explicit effects and require `apply:true`.
No live application or account was contacted during these tests.

## Evidence

`tests/test_rust_exports.py` covers exact target descriptions/definitions, all
three export byte formats, immutable sources, exclusive bundle publication and
collision refusal. `tests/test_rust_imports.py` uses actual disposable executable
fixtures to compare argv, grouping, environment and partial outcomes against
unchanged Python 3.14.6 reference commit
`453fca983222c6665a48775eef67c489c96b527e`.

Independent workflow/import review found and fixed signal outcome loss, falsey
metadata arguments, and Python 3.14 leading-dot suffix behavior. Permanent tests
include SIGINT during a growing private copy and after tool launch, cleanup and
child termination, failure of the second import group, and independent catalog
writer progress during a tool wait. A combined 69-case reconciliation/hardlink/
import/export regression passed after the shared reconciler refactor. This
functional regression does not replace either platform-blocked review.

Real Calibre/Calibre-Web/Immich version acceptance, platform matrix execution,
the complete legacy CLI/public facade, and final source-bound qualification
remain release prerequisites.
