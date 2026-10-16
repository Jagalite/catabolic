# Ordinary external operations and bounded rule-worker orchestration

This local opt-in checkpoint adds native `operation put` external metadata and
`rule run --worker` orchestration from source base `0f4360901b6096800a3631494a28ae458a8fe18e`.
Python remains the default. It closes neither full-leaf parity nor any M4–M11
milestone, installed-artifact, platform or filesystem-publication gate.

## Ownership and limits

- `catabolic_runtime::operations::put` performs the frozen operation-definition
  admission sequence and delegates render configuration to the existing artifact
  owner, analysis configuration to the existing media owner, and external
  processor/output lookup to their existing Store owners. The same persisted
  `processing_recipes` owner records canonical definitions, hashes, immutable
  revisions and idempotent reuse. The ordinary and preserved CLIs, and the native
  media operation-put API, use this admission owner. The API returns definitions
  before the generic publication-report visitor: a user option named `consumers`
  is never interpreted as an instruction to run consumer work
- `rules::run_external` validates the batch/detail limits before opening a
  catalog, plans through the existing rule owner, and selects only distinct
  queued/running jobs registered to that immutable rule. It preserves selection
  order, takes at most the requested batch, and executes one network step per
  selected job. It does not enqueue, recursively dispatch, or poll to completion
- Claims use the existing processor capacity, resumption, generation, expiry and
  production execution reducer. Claim admission is outside the dispatch-error
  catch; a refused claim is appended as a step and ends this invocation. Ordinary
  dispatch errors retain prior steps and stop the batch. A remote `failed` state
  is an accepted durable step, just as in frozen Python
- Native processor dispatch retains source evidence, lease fences, atomic
  transition/receipt acceptance and Store detachment. No catalog connection or
  writer lock is retained across network work. A fresh read-only plan supplies
  the bounded final report. No catalog-refresh or rendering is added to this
  command
- The private native HTTP helper uses the existing `HttpTransport` and
  `process::command_output` owners. Its process-wide deadline covers DNS, TLS and
  body reads; SIGINT propagates cancellation and the supervisor reaps the helper
  before returning. Credentials remain environment references in the IPC request
  and never appear in argv or catalog records. There is no Python fallback
- Empty `--worker ''` retains the default external pending-state report, as the
  frozen CLI dispatches a worker only for a truthy worker value

All qualification fixtures use disposable catalogs, tiny invented source files
and numeric loopback endpoints. No real destination, consumer or service was
contacted. The author's 18-method differential suite excludes successful receipt
acceptance; its invalid receipt and late-cancel cases prove no output metadata
is accepted. The later independent controls below also exercise successful receipt
metadata through the CLI, referencing already-existing tiny fixture files.
Existing native processor unit fixtures separately exercise that receipt owner.
No physical output production, publication, recovery or M6 gate was enabled or changed.

## Selected evidence

The tracked compact receipt is
[`native-external-rule-checkpoint.json`](../tests/parity/native-external-rule-checkpoint.json).
Detailed local logs, exact before/after source/artifact/reference hashes, per-test
outcomes, and diagnostic scripts are retained in the ignored
`.local-tests/external-rule/` directory of the implementation worktree.

Final selected counts and hashes are recorded in that receipt. The executable
suite has 18 methods with exact type-sensitive paired reports, HTTP requests and
all-table durable states, plus six ownership-ledger checks. Integer, boolean,
float, negative floating zero, Unicode and null distinctions are retained. Only
individually identified generated operation IDs, lease tokens and clock fields
are normalized; each is validated for type and shape/bounds first. Identically
named fields inside user options are never normalized. Every run checks foreign
keys, SQLite integrity, unchanged fixture bytes, no receipt/output/artifact/journal
rows, and writer-lock availability after the invocation.

Coverage includes immutable revision/idempotency; the exact UTF-8 option byte
boundary; definition/worker admission; empty and unregistered selections; batch
versus detail limit; unrelated and other-profile jobs; queued, running, failed,
partial and invalid responses; capacity and missing credentials; same-worker
polling; expired generation history; writer availability while HTTP is blocked;
in-flight durable cancellation and rejection of the late reply; and SIGINT
machine output, helper reaping and the retained reclaimable lease.

Each final paired run executes 18 methods and 42 exact type-aware comparison
groups. Separate ordinary-native-CLI and native-API definition probes preserve
consumer-shaped options literally and compare their complete typed results and
catalog states with frozen Python, with zero network requests.

Three native operation unit checks and six processor checks passed, including
existing real-loopback lock/fence/response-bound tests and a new cancellation
check that requires `waitpid` to report `ECHILD` after the helper returns.

The first unit run failed only its new fixture's asserted JSON byte count
(canonical JSON includes a space). The corrected boundary was rerun. The first
paired run had 17 passing methods and a failed process-observation assertion;
this container omits `/proc/<pid>/task/<tid>/children`, including for an ordinary
`sleep` child. A second diagnostic confirmed the omission. The final witness
reads kernel PPid identities from `/proc/<pid>/status`, observes an actual HTTP
subprocess, and requires every observed descendant to disappear after SIGINT.
These failed runs remain saved and are not promoted to passes. The first 3.11
invocation used the minimal pinned interpreter environment and failed in fixture
initialization because it had no Pydantic. The already-installed pinned 3.11
reference environment was reused for the replay; no packages were installed.
A Clippy-only unsafe-comment placement correction and the native API option-data
regression above were followed by fresh final-source checks.

## Independent narrow review of 06772cc

The independent review accepted commit
`06772ccccde6b651684db2279bef5eb4f6044146` within its documented bounded partial
scope, with no new blocking defect found. It used the existing full Python
3.11.15 frozen-reference environment and the same retained native artifacts.
Source and artifact fences passed. No implementation, main/next, dependency,
installation, physical output, live endpoint or gate change was made.

The review independently passed:

- The author's 18 methods / 42 typed paired groups, with no failures, errors or skips
- Seven new controls / 12 typed paired groups: successful PUT receipt and terminal
  no-op repeat; successful resumed GET without a new generation; completed work
  retained before a later batch error; valid late receipts rejected after durable
  cancellation or lease replacement; blocked-response timeout with helper reaping;
  and worker UTF-8/whitespace/empty-selection admission boundaries
- Two native metadata-entrypoint pairs, four retained Rust checks and six ledger checks

The 56 successful typed paired groups are separate from three diagnostic/nonparity
witnesses. Successful-receipt coverage is limited to the selected ordinary
pre-existing-file PUT/GET/partial cases. All-table state, completed/receipt/output
linkage, HTTP requests, fixture bytes, integrity, foreign keys, writer availability
and absence of artifacts/journal publication were checked. This does not qualify
all receipt outcomes or reuse, physical filesystem safety, publication or recovery.

The original 15-file review bundle was copied unchanged from its temporary
location to `.local-tests/external-rule/independent-review-06772/`. Every source
and preserved file hash matched. `preservation-manifest.json` has SHA-256
`f34b3f8410e8578d4030f378fe4b475e01709e45640cccba1f73890166239eb1`;
the tracked checkpoint records every file hash, including the review report,
controls, logs and SIGTERM witness. Earlier author failures and pre-fix evidence
remain separate and unchanged.

## Remaining nonparity and unqualified behavior

Two executed numeric-loopback diagnostic witnesses retain existing transport
output differences while producing equivalent durable state and exit status 3:

| Scenario | Frozen Python diagnostic | Native diagnostic |
|---|---|---|
| HTTP 302 with Location | `redirect refused; configure the final endpoint explicitly` | `HTTP request failed with status 302` |
| Response exceeding 1 MiB | `HTTP response exceeds byte limit` | `HTTP response exceeds limit` |

These historical diagnostics are corrected for selected ordinary responses in the
[later external-worker diagnostic checkpoint](RUST_MIGRATION_EXTERNAL_WORKER_DIAGNOSTICS.md).
The broader transport and ownership limits below remain open.

The independent blocked-HTTP SIGTERM witness is an executed nonparity result:
frozen Python exits with return code `-15`, empty stdout/stderr and one helper
still present at CLI return. Native exits `130` with a machine `KeyboardInterrupt`
envelope and no helper present at return. Durable catalog states match. Once the
fake response is released, the frozen helper exits too; neither side leaves a
helper afterward. This is not a SIGTERM parity success.

The existing transport's credential-header encoding, exact encoded request
budget, other network/timeout diagnostics and malformed protocol behavior are
not fully qualified by the successful ordinary response cases. No broader
transport equivalence is inferred from refusing redirects or oversized input.

Store detachment reuses its existing bounded writer-lock reacquisition wait,
where frozen Python reopens the completion writer immediately. Initial native
source verification for dispatch currently occurs in its writable Store scope,
where frozen Python opens that read phase without the writer lock. The shared
native flat Error channel also does not yet preserve every SQLite exception
category; exact failure classification under injected SQL errors is unqualified.
These ownership differences were not silently replaced with another Store owner.

Receipt outcomes and reuse beyond the selected successful CLI cases,
installed-wheel/callable packaging, macOS, process crashes, rollback/release
qualification and exhaustive malformed definition behavior remain unqualified. Program-bundle external imports retained their prior explicit refusal at this
checkpoint; track D (below) removed it.
Filesystem/publication gates and the separate paused/restricted review scopes
are unchanged. The ownership ledger therefore keeps both new leaves `partial`.
The narrow independent review above does not promote either leaf to full parity
or close any migration, platform, installation or publication gate.

## Later selected SIGTERM boundary checkpoint

The historical signal evidence above is retained. The later
[selected legacy SIGTERM checkpoint](RUST_MIGRATION_LEGACY_SIGTERM.md) aligns
standalone TERM exit/no-new-output presentation for rule `--worker`, fallback
worker and top-level watch only after existing native cleanup finishes. Private
Python callable and shared HTTP signal behavior remain unchanged. This does not
close the other documented gaps or promote any leaf or migration gate.

## Track D: program import kinds (October 16)

**Program import kinds.** Frozen `program import` admits every operation kind
through the ordinary `Operations.put` owner. The native import previously
accepted analysis and render operations through two narrower owners and
refused external processors. It now uses `catabolic_runtime::operations::put`,
the same frozen-sequence owner as `operation put`, for every kind, after
binding the processor name. A differential test imports the external program
bundle with a missing processor binding, a binding to an absent local processor,
a dry run, a real import and an idempotent repeat, comparing returned references
and every definition table with frozen Python.
