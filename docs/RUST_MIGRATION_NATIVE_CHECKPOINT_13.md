<!-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors -->
<!-- SPDX-License-Identifier: MIT -->

# Native checkpoint 13: parser composition, CLI admission and standalone CI

**Aggregate failed, 2026-10-13.** Frozen source
`499a4f31864dfabdd29444b07a60d054eede74f0` passed the native Rust and
model lanes, but the expanded CLI runner retained one obsolete control assertion
and one wrapper-test configuration error. The original source and failed logs
remain immutable; checkpoint 12's artifact/results remain separate. See the
[machine-readable receipt](../tests/parity/native-first-checkpoint-13.json).

## Integrated work

- Parser-local grammar/lexer boundaries close the fourteen retained witnesses.
  The isolated source passed 191 parser cases, 31 broader CLI methods, 16 Rust
  integrations and 28 GraphQL units. Its five other parser gaps and the independent
  review's 25 out-of-scope differences remain recorded; those lane results are
  not results for this new combined source
- The parser merge retains checkpoint 12's duplicate-key validation-only helper
  and unconditional CR position mapping. Independent source review verified
  the helper is byte-identical and no global JSON serialization changed
- The preserved CLI admits GraphQL file/stdin inputs at the frozen 131072-byte
  boundary before parsing variables. The separate structured companion retains
  its own engine-level diagnostic. The original failing witness and 55-case
  isolated replay remain recorded
- A standalone-native CI workflow prepares bounded Linux/macOS candidate jobs
  without a wheel or Python runtime prerequisite. All 29 shell wiring contracts
  passed on this integration. Actual GitHub workflow/platform execution is not
  claimed, and the workflow's qualification flags remain false
- A new 19-case composed-source regression crosses duplicate defaults/directive
  arguments with parser directive movement, escaped/literal Unicode and
  LF/CR/CRLF. It compares complete frozen and both native CLI responses and
  verifies unchanged catalogs; all 19 cases passed on the exact frozen source

## Observed local acceptance

The source passed formatting, strict native-workspace Clippy, 587 Rust passing
executions across 62 summaries (including all seven mandatory ignored witnesses),
and 29 standalone-CI shell contracts. Lifecycle checks exhausted 52 states and
364 transitions; playback exhausted five states and 1,200 transitions, with zero
skipped checks. Source, oracle, fixture, client, copied executable and oracle-Python
fences held throughout the 1,403.9-second attempt.

The expanded GraphQL runner executed 41 methods in 479.5 seconds. All 19 new
composed parser/validation cases passed. One old `conflicting-types` assertion
expected a generic refusal even though composed validation now matches the
frozen reference's exact field-name conflict. The later reviewed overlap slice
corrects the live assertion while retaining its historical capture. One embedding
method errored before execution because the native-only runner had not supplied
`CATABOLIC_COMPLETION_EXTENSION_ROOT`. It is a runner configuration error, not a
passing or failing wrapper behavior test. The next native runner explicitly
selects CLI/HTTP methods; embedding qualification stays in its deferred lane.

The aggregate stopped before generated-resource/doc checks and the HTTP/client
journey. Those commands are not counted as executed. The combined result remains
failed, regardless of separately passing component or later checkpoint evidence.

This checkpoint does not claim a new same-source standalone release candidate
until that separate artifact receipt exists. It does not infer actual Linux/macOS
CI success from fake-command shell contracts. The latest completed native
candidate is documented with its own exact source/artifact identity.

## Excluded subsequent work and open gates

Malformed GraphQL-variable JSON typed diagnostics and the complete overlapping-
field validation rule were separate implementation slices. They are not
part of this source and will not be added during its acceptance run. Their
historical differences remain explicit until separately verified.

M6 compatible safe publication/recovery, unrecognizable private sidecars,
unsupported provider options, denied broad reviews, actual mounted-filesystem/
macOS/live-integration acceptance, performance and rollback remain open. Production
HTTP/destructive paths remain gated. No default switch, Python deletion, remote
publication, release or milestone completion follows from this checkpoint.
