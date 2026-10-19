<!-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors -->
<!-- SPDX-License-Identifier: MIT -->

# Opt-in native Python CLI callable

`catabolic_native.cli.main(argv: list[str] | None = None) -> int` is a distinct
returning facade for the frozen `python:catabolic.cli.main` inventory row.
`catabolic_native.main()` remains the companion-console `execv` launcher.
The default `catabolic` package, console entry point and optional extras are
unchanged. This work neither retires Python nor closes a migration hard gate.
The `catabolic.cli.parser()` factory is not included: no native Python parser
factory is supplied, and mutable argparse.ArgumentParser object semantics remain
unadapted. Its frozen inventory row is `test-only-observed`; the native parser
used internally by main does not satisfy that separate Python object contract.

## Ownership and lifetime

The facade launches only the absolute, digest-checked packaged
`bin/catabolic-legacy-native`. No PATH lookup, development override, Python oracle
call, parser replica or automatic fallback is used. Missing, non-executable,
changed or differently source-bound helpers fail before dispatch.

A private version-one protocol uses length-prefixed JSON frames of at most 1 MiB.
The native parser distinguishes help/version/error `SystemExit` outcomes from
ordinary integer dispatch returns. `argv=None` takes the caller's current
`sys.argv[1:]`; explicit iterable text arguments are copied without changing it.
The Rust legacy parser, command owners and presentation functions remain the
actual implementation.

Native writes to stdout/stderr share one serialized frame channel, with explicit
flush events and acknowledgements. Python delivers them to the current text
streams, including StringIO and shared/captured streams. A failed callback cannot
leave an unowned reader thread: there are no Python transport reader threads.
Native progress callsites use this transport only inside the isolated callable
process. Their standalone rendering remains unchanged.

Input is requested only when native dispatch actually reaches a stdin operand.
The caller's `stdin.read(maximum + 1)` supplies text at its current position;
framed chunks are drained with bounded native accumulation and the existing
native byte limit. Help and validation refusals do not read stdin. Projection
mapping admission explicitly uses this path in callable mode; standalone native
stdin retains its independently reviewed cancellation/nonblocking-flag guard.

The callable owns child cancellation and reaping through every success, parser
exit, stream callback failure and host interrupt. Cleanup signals the child,
closes protocol input, waits for the native owner to join, and escalates from
SIGINT to SIGTERM then SIGKILL if necessary. The caller's signal handlers, process
identity, global streams and descriptor flags are not replaced. After native
parse admission, an interrupt inside the ordinary dispatch/success-output scope
returns the native renderer's interruption result. Parse-stage interrupts and
exceptions raised while reporting an existing error propagate by identity,
including interrupts during a render-only request. Native phase events preserve
this distinction; caught errors are not recursively caught during reporting.

The corresponding native CatabolicError, OSError, sqlite3.Error and ValueError
categories follow the frozen top-level caught tuple. Failures at that boundary
are rendered by a second native **render-only** request after the original child
is reaped; that request cannot repeat command effects. Maintenance progress is a
different scope: typed callback-error acknowledgements reach an additive native
fallible-progress adapter, which preserves its partial report and returns 3 at
the failing stage without starting later owners. The ordinary native run API is
unchanged. Standalone maintenance retains its existing ignored-progress-IO-error
behavior; only callable text callbacks use the typed error channel.

## Paired artifacts

The local release builder now packages both executables, each with a separate
format-one digest manifest. Existing `native-helper.json` compatibility is
unchanged. `legacy-helper.json` records the executable path/digest and the same
source digest/toolchain provenance as the companion. Generated artifacts are
excluded from both source fences. The builder checks both archive payloads,
execute bits and manifests; strict installed gates bind their exact inventory to
the same-build external copies, extension, wheel and sidecar. This is trusted
local source/content provenance, not hostile-directory attestation or hermetic
reproducibility.

## Qualification scope and remaining gaps

Selected callable tests do not qualify every native command leaf. Missing and
partial owners, parser diagnostic residuals, rendering differences, M6 output
refusals, native-only process semantics and platform gaps remain as recorded in
the CLI owner ledger and migration documents. The reviewed projection HTTP-v3
adapters remain partial; v1/v2 mappings, publication/recovery and real services
are not made available by the callable transport.

The wire accepts text argv within its 1 MiB frame budget. Non-string arguments
are refused before launch; arbitrary invalid argument objects, lone-surrogate
arguments/input and custom stream implementations that violate read/write
contracts are not exhaustively compatible. The existing native UTF-8 and
command-level input restrictions remain explicit. One retained negative witness
is oversized SQL stdin: frozen Python reports `SQL exceeds the 1 MiB input limit`,
while the native owner reports `input exceeds 1048576 bytes`; both return 2. The
callable matches its standalone native owner. This case is recorded separately
from the exact paired groups, not normalized into passing parity. Normal callback failures and
real host SIGINT are selected lifecycle evidence, not every crash schedule or
parent-process hard-death qualification. Linux testing does not qualify macOS,
Windows, other architectures or packaging/release transitions.

## Exact local checkpoint evidence

Product and installed-gate source is `8da98b2eb03dd650a03332bf4cdf981c7d64ec77`.
The additive CI routing and this evidence are separate source-only follow-ups.
The build source fence before/after is
`96c3c4e49db360a9119b14efcf8617f1c2ecc9a903e37b7921d650265e5b2fbe`.
The local Linux x86_64 cp311-abi3 wheel SHA-256 is
`379b58a94787109433c8c4027eefa7dd1e774845bec671658fccc55552f7a593`.
Its packaged legacy helper is
`f3ec48e06b3cac19c9c92732bc073710afff6edda508bec08b64611ff1ec892e`,
companion helper is
`6673eab8162a4a921c506a38854d89e48d5e80c12ad2c114f1973fd251e3cdfd`,
and extension is
`d15f0ad2edf8e2e64274e7af18910c8d12e7d110faad7c0dd39e38ad14f58b76`.

Both fresh outside-checkout installations used isolated pip with no index and no
dependencies. Exact installed qualification passed on Python 3.14.6 and 3.11.15:
14 required cases, 14 exact frozen/native groups, one selected inventory row,
111 callable invocations per engine and 145 callable-protocol children reaped on
each native interpreter. A separate standalone oversized-input child is excluded
from that 145 count. Full wheel/helper qualification passed before/alongside these
runs, including all 43 installed payloads, missing/changed/non-executable helpers,
the unchanged companion launcher, observation and import-interruption fixtures.

Independent review discovered and replayed three concrete corrections: errors
raised by error-handler output retain identity; standalone progress IO behavior
remains unchanged; and every corresponding frozen caught category is handled.
The reviewer independently verified 30 paired callback controls, 43 reaped
children, the render-only interrupt window, standalone failing-stderr behavior,
all artifact/source manifests and both installed trees. The selected scope was
accepted; no broader owner or excluded safety review is implied.

Additional focused evidence includes four native maintenance integration checks,
ten native CLI unit checks, 12 standalone maintenance cases, and Clippy with
warnings denied. All 122 source/packaging/receipt/CI controls passed separately
on Python 3.14.6 and 3.11.15. A final 32-case standalone projection/legacy/ledger
replay passed against the installed release helper in 176.318 seconds, including
open-stdin cancellation and descriptor-flag restoration. The installed CI matrix
now runs the strict callable gate and uploads its receipt. These local results do
not claim a GitHub Actions run. An earlier initial build
failed due an invalid boolean environment setting, and two superseded candidate
builds were canceled with status 130 before producing a wheel. Their failures
remain recorded; none is claimed as successful qualification.

See [the source/artifact/test receipt](../tests/parity/native-python-cli-callable-checkpoint.json).
Local complete logs and detailed installed receipts are under
`.local-tests/python-cli/`. No push, publication, full aggregate qualification,
platform promotion, Python default change or M6 gate change is claimed.

## Later scan-watch integration

The merged scan-watch adapter now sends cycle progress through the same framed
text-stream bridge. Two development regressions first failed against the retained
pre-correction helper (missing callable stderr and an unobserved callback error),
then passed after routing through the bridge. The combined development replay
passed 39 methods, including four explicit inherited scan-report nonparity
witnesses, plus 12 CLI and five maintenance orchestration Rust checks. This
does not qualify nonempty scan report parity.

The current strict installed runner additionally requires an empty-inventory
watch progress group in all three presentation modes (15 groups total). The
14-group installed receipts above remain evidence for their recorded older
source. A new combined-source installed replay is still required.
