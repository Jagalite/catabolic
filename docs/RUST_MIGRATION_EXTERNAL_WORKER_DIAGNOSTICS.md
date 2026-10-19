# Selected external-worker diagnostic correction

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

This local opt-in checkpoint starts from
`343d7e83a8c275dc282cda9b1ac0e6b4a320aaa6` and corrects the two ordinary worker
diagnostics recorded in [the external-rule checkpoint](RUST_MIGRATION_EXTERNAL_RULE.md).
It does not close any milestone, platform, installed-artifact, release or
filesystem/publication gate. Python remains the default.

## Production scope

The existing `catabolic_runtime::processors::HttpTransport` remains the native
HTTP owner. With following disabled as before, selected ordinary 301/302/303/307/308
responses with Location (or the frozen handler's URI fallback) now report
`redirect refused; configure the final endpoint explicitly`. Missing headers and
unsupported explicit schemes retain the ordinary HTTP status error. Location
wins over URI. This is diagnostic classification only: no redirect target is
contacted or admitted as a request endpoint.

The existing Content-Length, bounded-read, supervised-result and dispatch-result
response guards now report `HTTP response exceeds byte limit`. Their limits,
order and refusal behavior are unchanged. In particular the native early refusal
of an advertised oversized Content-Length remains intact; this does not establish
clean-EOF, truncated-response or reset equivalence.

No credentials, source authorization, request policy, retry behavior, durable
claim/receipt reducer, Store ownership or cancellation behavior was changed.
The transport still uses the same private native helper and process supervisor.

## Executed evidence

The compact receipt is
[`native-external-worker-diagnostics-checkpoint.json`](../tests/parity/native-external-worker-diagnostics-checkpoint.json).
Exact source/reference/artifact hashes, commands, complete results and earlier
failed evidence are retained under the isolated worktree's ignored
`.local-tests/external-worker-diagnostics/` directory.

The tracked `tests/test_rust_legacy_external_worker_diagnostics.py` nine-method
suite uses independent disposable frozen/native catalog
copies, invented tiny source files and numeric loopback endpoints. It compares
complete typed reports, actual requests, every durable table, exact UTF-8
diagnostic bytes, exit status and stderr. Only the existing explicitly validated
generated identifiers, lease tokens and timestamps are normalized. It checks
SQLite integrity, foreign keys, writer availability, unchanged fixture bytes and
absence of output/receipt/artifact/journal publication.

Coverage includes PUT and resumed GET; relevant redirect statuses with and
without Location; relative, empty and URI-fallback references; Location precedence;
ordinary status and successful neighbors; valid Content-Length and HTTP/1.1
chunked bodies at 1 MiB minus one, exactly 1 MiB and one byte over; a late valid
receipt after durable cancellation; resumed generation retention; lock release;
and SIGINT helper reaping with a reclaimable lease. Existing native processor
checks additionally retain successful receipt acceptance, timeout and cancellation
cleanup witnesses. Those checks do not qualify physical output production.

The first paired run used HTTP/1.0 with a chunked response and failed four
under/exact-boundary comparisons. Its source, log and receipt are retained as a
failed invalid-framing fixture run. The fixture was corrected to HTTP/1.1;
production transport behavior was not changed to accommodate that response.
A later fixture-only correction gives the four reused cleanup regressions unique
IDs, so discovery with the original suite cannot duplicate those test cases. Its
`test_rust_legacy_` module prefix also retains the existing aggregate requirement
for an explicitly supplied legacy CLI artifact. At that checkpoint, the installed 79-method adapter
gate remained unchanged and did not claim this module as installed evidence.
The later [expanded strict gate](RUST_MIGRATION_LEGACY_ADAPTER_INSTALLED_GATE.md)
selects these nine methods alongside all previous cases, but its source-only
wiring/discovery evidence is not installed product qualification.

## Limits

The diagnostic classification covers selected ordinary ASCII Location/URI
references, not an implementation of every urllib URL/header parsing edge.
Track D later qualified selected malformed-protocol, truncated-body, refused
connection, Latin-1 credential encoding and exact request-budget behavior; see
the [external-rule track D section](RUST_MIGRATION_EXTERNAL_RULE.md). Other
transport diagnostics and wider HTTP security remain unqualified. Existing Store detachment/reacquisition and
SQLite exception-classification differences remain explicit in the earlier
checkpoint. Receipt reuse and other receipt outcomes remain limited to their
previous evidence. Previously restricted broad feature and filesystem/source
safety reviews were not retried. No live endpoint, real credential, installation,
main edit, push, release or default switch was performed.
