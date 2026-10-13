<!-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors -->
<!-- SPDX-License-Identifier: MIT -->

# Native checkpoint 14: integrated work stopped before aggregate qualification

**Stopped at the user's request on 2026-10-13 at 01:15:35 UTC.** Consolidated
code is `208d57fb255feaed407e1ce2f35f2b4a6ccac5a5`, tree
`92110286cff3e2f85c1798177b57123f15389071`. This is a saved implementation
checkpoint, not a passing combined-source qualification. See the
[current stop handoff](RUST_MIGRATION_STOP_2026_10_13.md).

## Included and individually checked

- Typed legacy GraphQL variable-JSON syntax diagnostics, including exact error
  identity, locations and admission ordering. The reviewed isolated source
  passed 153 variable, 55 file-boundary, 182 coercion and 104 runtime-input pairs,
  22 release CLI units and strict Clippy. Seven non-syntax JSON differences remain
  explicit. See [its evidence](RUST_MIGRATION_GRAPHQL_VARIABLE_ERRORS.md)
- The native overlapping-field validation rule now handles source-ordered
  fragment/field comparisons, nested conflict reasons, argument equivalence and
  mutually exclusive types under the retained comparison budget. Its reviewed
  source passed 77 complete frozen cases across preflight, direct/persistent
  execution and both CLIs, 41 Rust tests and 25 CLI methods. Independent review
  added 66 documents/132 CLI comparisons. Historical responses, including the
  obsolete checkpoint-12/13 control, remain recorded in
  [the overlap receipt](../tests/parity/native-graphql-overlap-checkpoint.json)
- The native HTTP qualification fixture preserves all 31 original generated-client
  assertions while coordinating processing and mutation phases through an owned
  private pipe. Real worker admission/replay/ready assertions run before verified
  graceful shutdown/reap. Snapshot/ticket mutations follow; a separate deliberate
  writer-contention request checks one 503 without mutation replay. All 16 focused
  tests and two source-stable Linux journeys passed. Independent narrow review
  found no blocking issue and inspected compiler provenance, but did not repeat
  the compiler rebuild before stop. See the
  [exact fixture receipt](../tests/parity/native-http-fixture-isolation-checkpoint.json)

The HTTP journeys used the immutable native debug binary from source `0861677`,
not a new binary built from this combined checkpoint. Harness source was
`3f9df86`; source and binary identities are distinct in the receipt. The first
active-worker ticket503 and the first isolation attempt's worker-unavailable409
remain failed evidence. The successful isolation does not establish arbitrary
concurrent mutation retry safety.

## What ran on the pending combined source

At `c1c0c3ce7355362d0e6a035c348d681707ad4582`, formatting and strict release
workspace Clippy, excluding `catabolic-python`, passed. The later HTTP integration
changes only its qualifier, focused tests and client test source; it does not
change native Rust. No new combined-source test aggregate, same-source standalone
release candidate, actual GitHub workflow, macOS test or wheel gate was started
before stop. Do not promote individual component counts into a passing aggregate.

A subsequent five-case parser continuation is **excluded**. It is incomplete,
untested local WIP at `7426bd6`, with interrupted Cargo and no completed tests.
Its publication to a new remote branch was blocked; the existing feature branch
does not include those edits.

All M0–M11 hard gates remain open. Native destructive/publication and production
HTTP restrictions remain. The default distribution still uses Python; the agreed
end state is native Rust with a thin wrapper completed last.
