# Completed scan report presentation

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

## Qualified boundary

The legacy presenter now carries command-local report context for newly
requested, completed public `scan`, `maintenance` and top-level `watch` results.
The same context is used for the watch cycle's stderr progress copy.

At the recorded observation, guarantee, guarantee execution/budget and scan
validation-policy paths, fields follow the frozen Python insertion order.
Unknown fields are retained. Nested provider payloads and report-like objects
outside these paths retain their previous rendering; no global object-shape
registry or ownership policy is changed.

A fresh public CLI scan has no `max_age` option. Its default positive zero is
spelled `0` in the legacy text and JSON wire output. This is a rendering-only
adaptation: the native `Value`, persisted guarantee and structured native
contract retain their original number types. An explicitly configured maximum,
resume/continuation context, negative zero, nonzero integer or fractional value
must not acquire that default spelling merely because its object looks similar.

Resume, continuation, pending/unavailable/error observations and other scan
execution schedules are outside this presentation qualification. Their existing
rendering and behavior are retained.

## Evidence

`tests/parity/scan-report-presentation-v1.json` is an additive fixture containing
nine complete output envelopes: scan, maintenance and watch, each in text, JSON
and machine modes. Inputs are complete, immutable successful reports from the
sealed diagnostic witnesses. The fixture binds their hashes, the frozen CLI
module hash, Python 3.14.6 and reference commit
`453fca983222c6665a48775eef67c489c96b527e`.

The frozen CLI's real parser and presenter regenerate each expected envelope
with dispatch replaced by the recorded report. This replay does not execute
scans and does not establish runtime or timing parity. Rust tests compare the
entire stdout string, exit code and exact watch progress lines, with native
float-zero input retained unchanged. Numeric controls cover `0`, `0.0`, `-0.0`,
`4` and `4.25` across default, configured, resumed, continued and unrelated
contexts. Unknown/provider payloads are checked byte-for-byte against the
ordinary renderer and retain their numeric types.

Live disposable-catalog checks separately cover direct scan, maintenance and
watch in JSON/machine modes. They assert the selected field orders, default
legacy integer spelling, actual durable reports and three-batch accounting,
and the unchanged native floating-point persisted maximum. Four pre-existing
nonparity witnesses and the eight accounting regressions remain explicit.

## Remaining differences

The full maintenance JSON/machine, watch JSON/machine and direct scan text
comparisons remain failing diagnostics. These outputs retain actual timestamp,
identity, PID, sampled storage and duration-dependent committed-byte values.
Native `minimum_start` remains the stricter admission-time/started-at floor;
frozen Python uses the earlier public `after` boundary. No normalization or
presentation substitution turns these execution differences into passes.

No observation, filesystem, publication, security, writer, cancellation,
continuation, budget, claim, freshness or recovery owner is changed. This is not
a full scan parity, installed-artifact, platform or release qualification.
