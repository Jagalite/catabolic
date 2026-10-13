<!-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors -->
<!-- SPDX-License-Identifier: MIT -->

# Rust migration: October 13 stop and resume handoff

**Work stopped on 2026-10-13 at 01:15:35 UTC at the user's request.** No further
implementation or qualification is running. The requested source commit is
`208d57fb255feaed407e1ce2f35f2b4a6ccac5a5` on
`codex/rust-migration-m4-plus`, with exact tree
`92110286cff3e2f85c1798177b57123f15389071`. This document is the subsequent
progress-documentation commit. It supersedes the October 11 handoff for current
status without rewriting its historical results.

## Publication state at this documentation commit

Source and documentation are committed locally. Publication has **not succeeded**.
The last verified remote feature is `6c5e3f5bbc2fcf32b79b6b232d1673bac5b5b25d`;
remote main is `c6471145d615bbe5a60806f97ef5da0766b92fa8`.
Native Git push could not obtain a GitHub username with terminal prompts disabled,
and the GitHub CLI has no logged-in host. The first connected-GitHub tree upload
returned `user cancelled MCP tool call`; no connector commit or branch update
was attempted. A retry awaits explicit clarification. Do not read these local
commits as proof of remote publication. When authorized, publish the code commit
first and this docs commit second, verifying each exact tree and remote head.

A separate proposed new-branch publication of incomplete parser WIP was rejected
as beyond the existing-feature-branch authority. It remains local; do not publish
it or fold its edits into the feature branch as a workaround.

## Architecture and completed implementation

The native Rust crates own the Store/migrations/transactions; definitions,
curation, revisions, plans and query execution; observations/scans/watchers;
filesystem journals; media/processors/artifacts/renditions/components/fallback;
consumer/publication/playback owners; CLI dispatch and experimental HTTP. These
are Rust implementations. The optional notification helper now uses supervised
Rust HTTP/SMTP providers rather than Python/Apprise at runtime.

The user changed sequencing on October 12: finish the standalone Rust engine and
CLI, retire Python business implementation only when replacements are verified,
and finish the thin Python compatibility wrapper last. The frozen original is a
differential oracle during transition. Existing facade/wheel work remains saved;
new wrapper tests are not the native critical path. The installed default is
still Python; no default switch, release or Python deletion occurred.

The initial resumed defects were corrected with focused independent evidence:
the four SQL refusal diagnostic assertions were aligned with the frozen oracle,
and fallback prepared-plan release/constructor failure ownership now recovers
all handle slots in its focused fault matrix. These do not turn the earlier
844-test failed aggregate into a pass. The
[resumed log](RUST_MIGRATION_RESUMED_CHECKPOINT.md) preserves that history and the
many subsequent owner, CLI, facade, build and test-harness corrections.

## Exact recent results

| Source/checkpoint | Passed | Failed, excluded or unrun |
|---|---|---|
| Native 11, `b11666f` | 558 Rust passing executions, six mandatory witnesses, lifecycle/playback models; bounded standalone candidate | Full runtime/platform/release gates open; historical artifact had five dependency-notice gaps later fixed |
| Native 12, `0861677` | 582 Rust passing executions, seven mandatory witnesses, models; separately reviewed same-source standalone candidate with five supplemental notices, safe smokes/provider replay/output refusals | Aggregate retained one obsolete conflict assertion; separate original HTTP ticket request returned503; neither failure erased |
| Native 13, `499a4f3` | 587 Rust passing executions, seven mandatory witnesses, models, 29 shell CI contracts, all19 new composed parser cases; all identity fences held | 41-method CLI runner: one obsolete conflict assertion and one extension-path configuration error; later resource/doc/HTTP steps not reached; no new standalone candidate |
| Pending Native 14, `208d57f` | Individually reviewed variable-diagnostic and overlap slices; native format/strict Clippy at `c1c0c3c`; corrected HTTP fixture passed two runs using source086 binary | Combined aggregate and same-source standalone candidate **not started**; no new installed-platform or actual CI result |

See [checkpoint 12](RUST_MIGRATION_NATIVE_CHECKPOINT_12.md),
[checkpoint 13](RUST_MIGRATION_NATIVE_CHECKPOINT_13.md), and
[checkpoint 14](RUST_MIGRATION_NATIVE_CHECKPOINT_14.md) for exact boundaries.
The checkpoint-13 configuration error selected a Python-extension method without
its required extension path; it did not execute that embedding test. The next
native runner should select CLI/HTTP methods explicitly and leave embedding in
its deferred lane. The reviewed overlap slice corrects the obsolete live
expectation while preserving the original response captures.

The last completed standalone candidate is source086, manifest SHA-256
`af75aa96b89466b86bb9a2871f5c1b679af6f2d4dca7203ad78fe0c349c06d8e`.
Its executables are `catabolic` SHA-256
`01fd2a191242176d2178e0599f8042428d482582f9dcc64dc9c901a296ff169c`
and `catabolic-native` SHA-256
`87a4acf93271c9cd2459d0f43dfee1ce04c16cb096afc6115cc0d26e932e05b9`.
This is a bounded Linux candidate, not a release. Its final qualification receipt
SHA-256 is `8309ba110ae9db140eb71d2e7fa9fe4dd8fe93d04e91c4ff882f5338f53629bd`.

## Work at the stop boundary

The pending checkpoint combined completed parser/file-boundary/CI changes with
typed variable diagnostics, overlapping-field validation and the reviewed HTTP
fixture correction. The next source-stable aggregate had not started.

A new parser continuation had just begun in its own worktree. It tentatively
adapts braced Unicode and argument-syntax diagnostics in two Rust files (+47/-14),
but removes two old witnesses without replacement tests and has not promoted
remaining-control fixtures. Cargo was interrupted with exit130; no test completed.
It is saved as explicitly incomplete commit
`7426bd67f2c36640d4c20098cef159dfd9e332f3` on local branch
`work/graphql-final-parser-boundaries`, **excluded from the feature tree**.
No other implementation worker remains active. Original worktrees/logs/artifacts
are preserved; no failed receipt has been rewritten as passing.

## Remaining gates, by milestone

- **M4:** complete ledger/interchange/unknown-field/digest/stale-plan parity and
  independent acceptance; five parser controls, seven non-syntax variable JSON
  differences and other documented diagnostics remain. Broad definition review
  remains blocked
- **M5:** actual macOS/native-event and mounted-filesystem acceptance, complete
  source/report/watch behavior and resource evidence. Broad filesystem review
  remains blocked
- **M6:** compatible safe destructive publication/recovery protocol, durable
  failpoint matrix and both recovery directions. Known foreign-leaf/marker races
  remain unresolved; dangerous paths stay refused or disposable/debug-gated
- **M7:** complete real-tool/preset/cancellation/late-receipt/platform matrix;
  current FFmpeg/Poppler/Tesseract successes cover selected fixtures only
- **M8:** all fallback families/failback/component mux and lifecycle qualification;
  historical frozen stale-plan/recovery-contention outcomes remain unexplained
- **M9:** supported real-service acceptance, growing-HLS/session isolation and
  remaining notification/provider options. Rust providers have explicit
  fail-closed and unsupported-option differences
- **M10:** complete HTTP1.7/auth/client/CLI behavior and production content
  protection. Recognizable private WAL/SHM alias refusal is implemented;
  unknown/damaged/orphaned sidecar cases remain a gate. Numeric-ID cache confusion
  in the frozen oracle is intentionally not reproduced. Thin wrapper last
- **M11:** complete prerequisite/platform/performance/rollback/license/release
  gates, native default distribution transition and final thin-wrapper packaging.
  CI wiring and Linux candidates do not establish actual macOS or full release
  readiness. **No milestone hard gate, including M0–M3 prerequisites, is closed**

The two previously blocked broad definition and filesystem/source-access reviews
must not be rerun through another route. Optional Python3.11 HTTP dependency
approval remains pending; it affects a deferred wrapper/reference lane, not the
Rust engine. No real service credentials, production catalogs or managed outputs
were used to substitute for missing acceptance.

## Evidence and resumption

Tracked receipts include `tests/parity/native-first-checkpoint-{11,12,13}.json`,
`native-graphql-{variable-errors,overlap}-checkpoint.json`, and
`native-http-fixture-isolation-checkpoint.json`. Local immutable evidence is under:

- `catabolic-cli-wave12/.local-tests/wave12/` and
  `catabolic-native-checkpoint12/final-0861677/candidate/`
- `catabolic-cli-wave13/.local-tests/wave13/native-checkpoint/`
- `catabolic-cli-wave14/.local-tests/wave14/preflight/`
- `catabolic-native-http-isolation/.local-tests/http-isolation/`
- `catabolic/.local-tests/publication-stop-20261013/`

The last directory holds the verified all-local-history Git bundle, prepared
publication manifests and sequential tree expectations. Bundles include the
local WIP for recovery, not permission to publish its denied new branch. These
ignored artifacts remain local and are not files in the pushed source tree.

Resume only after a new instruction to continue. Inspect remote/local state and
this handoff first; preserve user changes. Freeze the consolidated source, use
the preexisting offline Rust1.99/tooling and an exclusively assigned cache, and
run formatting/strict Clippy/native tests, all seven exact mandatory witnesses,
lifecycle/playback models, CLI/GraphQL comparisons and the coordinated HTTP/client
journey with before/after source, oracle, fixture, client and executable hashes.
Build/review a same-source standalone candidate separately. Avoid a wrapper gate
on this native checkpoint. The incomplete parser branch requires its own fixture
updates, testing and fresh review before any future integration. Preserve all
historical failed and interrupted attempts. Do not enable destructive or
production HTTP behavior while its gates remain open.
