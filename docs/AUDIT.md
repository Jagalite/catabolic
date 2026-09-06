# Catabolic code, architecture, integrity, and security audit

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

Reviewed 2026-09-06 against commit `2d48eae`, version 0.1.0, database schema 7.

## Remediation status — 2026-09-06

The four confirmed defects below are fixed in the current working tree. The
original findings describe the reviewed commit; this section records the fixes.

- A1: imports and processing share `source_access.validated_source`. Imports
  compare manifest fields with local inventory, pin the preflight revision, and
  validate the named source and root across the staging copy.
- A2: `process_runner.command_output` supervises extractor, import, and HTTP
  process groups and retires descendants even after their leader exits. Tests
  cover descendants retaining or closing pipes, timeout, and CLI interruption.
- A3: import results distinguish failure before launch from an unknown external
  outcome. They include per-group attempt IDs, confirmed completions and retry
  safety. An uncertain first group returns `applied: null`; interrupted imports
  preserve the JSON report and exit code 130. Attempts are invocation records,
  not a durable resume journal.
- A4: a supervised private HTTP worker enforces the overall deadline across DNS,
  TLS, headers and body consumption. Credentials use stdin and errors are
  redacted. Slow-body/header and stalled-worker regressions are covered.

Release tooling now uses wheel-hashed runtime/build/development locks, a pinned
build backend, commit-pinned GitHub Actions and a fail-closed OSV check. Local pip
is updated to 26.2. Source/process helpers were extracted from `processing.py`,
recovery compatibility is explicit and tested for schemas 1–7, and stale scanning
documentation is corrected.

Validation of the final implementation:

- 308 regression tests passed in 33.748 seconds.
- Lint, formatting, bundled guides, current interchange generation and all seven
  previously released interchange artifacts passed their checks.
- A live OSV check returned no advisories for all 16 pinned public packages.
- Hashed dependency installation passed in a fresh macOS/Python 3.14 environment;
  runtime/development locks also resolved to Linux/Python 3.11 wheels.
- Installed-wheel acceptance passed with ten generated media files, preserved
  source content, SQL/GraphQL, and real Jellyfin 10.11.8 scan, static playback and
  refresh delivery. Fixture root:
  `/private/var/folders/p2/hs2582qs5672qbvtm4z5_9840000gn/T/catabolic-acceptance-f6mgelga/run`.
- Tested wheel SHA-256:
  `b60045492afd210ca329f2bda037b29f6782633d7f79ea67e06ad8dd6988fd02`.

Evidence is under `.local-tests/audit/fixes-*` and `linux311-lock.log`. Remote CI
has not run. The real catalog and user media were not modified. No schema
migration is required for these fixes. Semantic database-audit commands, routine
backup/restore tooling and broader processing decomposition remain follow-up
recommendations, separate from this defect remediation.

## Assessment

The core architecture is appropriate for a local, agent-operated media catalog. Keep SQLite, explicit curation, separate inventory and projections, and journaled filesystem changes. A rewrite or a database replacement is not justified by this review.

Four medium-priority workflow defects were reproduced. Fix these before widening release, particularly enabling unattended imports. No critical defect was established in the inspected paths. This is a code review with targeted failure injection, not a proof that every input or filesystem is safe.

The strongest safeguards are in migrations and link reconciliation. The newer adapters implement weaker versions of some of those safeguards. Consolidating these mechanisms is the most useful maintainability improvement.

## Confirmed findings

### A1 — Medium: imports can accept a different file under the recorded identity

Locations: [import preflight](../src/catabolic/importers.py#L61), [staging](../src/catabolic/importers.py#L170), [processing source validation](../src/catabolic/processing.py#L41).

Import preflight and staging compare size and modification time, but do not compare the opened file's device and inode with its recorded observation. Staging also does not revalidate the named source after copying. Processing already has a stronger descriptor-based validator that checks these properties.

Reproduction: inventory a file, replace it with a different inode containing different bytes, and preserve its size and modification time. `stage_file` copied the replacement bytes successfully. The processing validator rejected exactly that occurrence with `source changed since scan; rescan before processing`. No concurrent race was necessary.

Impact: Calibre or Immich can receive different media associated with the old catalog identity and metadata. This does not modify the original source, but compromises the correctness of the import.

Fix: resolve the current profile occurrence by file ID and use the shared validated-source context for the entire staging copy. Validate the manifest's location/path against that occurrence. Keep local inode evidence in the operational layer; portable interchange need not acquire machine-specific inode fields. Add a replacement-with-preserved-timestamps regression test through the import entry point.

### A2 — Medium: timeout cleanup can leave subprocess descendants running

Locations: [extractor cleanup](../src/catabolic/processing.py#L161), [import subprocess invocation](../src/catabolic/importers.py#L144).

`command_output` starts a process group, but kills it only when the direct child is still running. If the direct child exits while a descendant retains the output pipes, the deadline expires and cleanup skips the group kill. Imports separately use `subprocess.run` without creating or managing a process group.

Reproduction: a disposable helper forked a child and exited; the child held the inherited pipes open. `command_output(timeout=0.2)` reported `timeout`, but the descendant remained alive. The audit explicitly killed that fixture child afterward. This demonstrates a lifecycle defect, not an exploit in FFmpeg.

Impact: timed-out or cancelled tools can continue consuming resources. Import descendants may also continue external work after the CLI reports a failure.

Fix: use one subprocess lifecycle mechanism with explicit process-group ownership and cleanup after timeout, cancellation, output overflow, and early parent exit. Preserve adapter-specific environment and output handling. Test parent-exits-first and descendants-closing-pipes cases, as well as ordinary timeout.

### A3 — Medium: a failed external import is reported as unapplied despite side effects

Location: [import error result](../src/catabolic/importers.py#L153).

On failure, `applied` is derived solely from previously completed groups. A tool can mutate the destination for the current group and then fail or time out. The returned result lacks an explicit indication that this group's side effects are unknown.

Reproduction: a fixture importer wrote a marker into its disposable destination and exited with status 1. Catabolic returned `applied: false`, `completed: []`, and `complete: false` although the destination had changed.

Impact: an agent may interpret the result as safe to retry from scratch. Repeated imports can duplicate records or compound partial external state. A failure exit code does correctly signal that the command did not complete, but does not resolve the ambiguity about applied changes.

Fix: distinguish not started, completed, failed before launch, and external outcome unknown. Include the attempted group and an attempt identifier. Do not promise rollback or safe retry where the adapter cannot verify it. Add durable attempt tracking and target-specific reconciliation if unattended resume is supported.

### A4 — Medium: HTTP adapters lack a total elapsed-time deadline

Locations: [HTTP request](../src/catabolic/network_adapters.py#L23), [refresh delivery](../src/catabolic/network_adapters.py#L209), [writer lifetime](../src/catabolic/store.py#L28).

The adapter supplies `timeout=15` to urllib, then reads the response up to its byte limit. This limits blocking socket operations, not total request duration. A server that keeps delivering small pieces can retain the operation far beyond 15 seconds. Write-enabled identify and refresh invocations retain the catalog writer lock while waiting.

Reproduction: a loopback HTTP fixture sent small chunks every 50 ms. With only the socket timeout shortened to 100 ms, the adapter completed after 611 ms without a timeout. The original 15-second setting has the same semantics. See the [Python urllib timeout documentation](https://docs.python.org/3/library/urllib.request.html#urllib.request.urlopen).

Impact: a slow or misbehaving endpoint can stall automation and exclude other catalog writers for a prolonged period.

Fix: enforce a monotonic deadline across connection and response consumption, while retaining the byte cap, redirect refusal, and credential redaction. Account explicitly for DNS/connect behavior. Add a slow-response test that distinguishes an idle timeout from an overall deadline.

## Database integrity assessment

The existing migration path validates the exact known schema and migration ledger, creates a SQLite backup, verifies its identity and contents, rehearses the chain, and then compares the live database with the backup under a SQLite writer reservation before upgrading. Pending filesystem operations block migration. Preservation checks reject changes to existing columns and values. These are useful protections worth retaining. See [migration validation](../src/catabolic/migration.py#L160) and [upgrade](../src/catabolic/migration.py#L472).

The ordinary Store open checks structure and migration history. Full validation additionally runs SQLite integrity and foreign-key checks, plus the active-mapping/association invariant. It does not validate all application semantics. In particular, several schema-6 operational state/operation fields are unconstrained text, and references such as `file_facts.job_id` do not ensure that the referenced job belongs to the same profile, file, and operation. This is a defense-in-depth gap; this audit did not demonstrate a supported CLI write producing those inconsistent rows.

Recommended follow-up:

- Add a read-only database audit command for enum values, JSON shapes, fact/job identity consistency, tag graph cycles, and ownership/journal consistency. Report findings without automatically repairing records.
- Move appropriate invariants into constraints in newly authored schemas. Existing frozen migration files must remain unchanged; extending old tables requires preservation-aware migration design.
- Make backup and restore verification an ordinary maintenance workflow, independent of whether a schema upgrade is pending. Interchange manifests omit enrichment state and are not full database backups.
- Keep the database on local storage and retain the single-writer model until measured concurrency requirements justify a change. Short SQL transactions already avoid holding a SQLite transaction throughout extraction; the separate process-wide writer lock remains a documented throughput limit.

These recommendations do not establish existing data loss or corruption in the user's catalog. The real catalog was not opened or migrated during the audit.

## Maintainability assessment

1. **Unify source validation and subprocess lifecycle first.** A1 and A2 show actual behavioral drift between adapters. Shared contracts should describe an occurrence revision, validated source handle, execution budget, and outcome without making the CLI responsible for safety rules.
2. **Separate processing responsibilities along existing boundaries.** `processing.py` currently combines source validation, extractors, queue scheduling, retry history, fact publication, checksum baselines, and text search. Extract these cohesive responsibilities gradually with the existing tests protecting behavior. Introduce typed internal records for job snapshots and outcomes; avoid a large framework or wholesale rewrite.
3. **Version recovery compatibility explicitly.** [Store recovery admission](../src/catabolic/store.py#L35) contains a comment about schemas 2–4 but admits every earlier schema. Current compatibility needs tests per supported version; future journal changes should require an explicit compatibility decision rather than inheriting this range automatically.
4. **Correct stale operational documentation.** [README](../README.md#L433) and its packaged copy say scans buffer an entire location in memory and large-library performance is uncharacterized. Current [scan staging](../src/catabolic/scan_staging.py#L6) uses disposable SQLite storage and 500-row buffers, and the repository contains scale/acceptance evidence. The existing document-sync test checks matching copies, not factual freshness.

## Security and release tooling

The reviewed SQL interface uses an authorizer, a computational-function allowlist, query-only mode, input/result limits, and a progress deadline. GraphQL uses a read-only resolver surface and document, depth, field, record, and output budgets. Filesystem operations generally use directory-relative no-follow access; hardlink retirement retains data rather than unlinking the remaining pointer. These controls address the local CLI threat model.

Generated outputs require exclusive management. The documented model explicitly excludes a malicious process running as the same user and simultaneous external output renames/writes. Remaining compare-and-delete races should not be advertised as protection against that adversary. External media tools also run with the user's privileges; their process groups and format allowlists are not an operating-system sandbox.

A live OSV check covered 16 installed public Python packages. No advisories were returned for the six runtime dependency packages checked. The installed development environment's `pip 26.1.2` matched **CVE-2026-13346**, represented by two aliases for the same issue. It concerns malicious package-index URLs and is fixed in pip 26.2.0. Its stated practical impact is restricted; this is a tooling finding, not evidence of a Catabolic runtime vulnerability or compromise. See the [OSV advisory](https://osv.dev/vulnerability/GHSA-qwm4-qh6w-59xr). Update the release environment's pip before subsequent builds.

For reproducible release tooling, record the resolved dependency set and hashes, constrain the build toolchain, pin CI actions to reviewed revisions, and automate advisory checks. The current direct runtime pins and installed-wheel acceptance are useful, but transitive dependencies and `setuptools>=68` can still resolve differently on a later build. FFmpeg, Python, SQLite, OS libraries, and container dependencies were not covered by this Python-package advisory query.

## Validation and limitations

- Existing suite: 293 tests executed; 292 passed in the sandbox. The sole error was the sandbox denying a loopback server bind. That exact HTTP test passed on an authorized rerun with loopback access.
- Four additional targeted failure reproductions confirmed A1–A4 using disposable files, a fixture subprocess/importer, and a loopback server. No real application import was performed.
- Local evidence is retained under `.local-tests/audit/`: `reproduce.py`, `import_failure.py`, `http_deadline.py`, `tests.log`, `http-test.log`, and `advisories.json`. That directory is ignored by Git.
- Review covered the database/migration boundary, filesystem ownership and recovery, query surfaces, curation, processing, imports, network adapters, package configuration, and test/CI configuration. It was not an exhaustive line-by-line review of every output preset.
- No production code was changed. No user media, real catalog database, or remote service was modified. Previously recorded consumer/storage acceptance was not rerun for this audit. Cross-platform fixes must be validated in the existing Linux/macOS acceptance lanes.

Recommended order: fix A1–A4 with permanent regression tests; update release tooling; add semantic database auditing and backup drills; then make the small architectural extractions. Re-run installed-wheel acceptance after the functional fixes.
