# Native retirement decisions, October 16, 2026

The user requested progressing to retirement of the Python engine and selected
both compatibility policies below. These decisions supersede the pending-choice
statements in the historical takeover handoff. They do not turn its historical
evidence into current qualification or remove the remaining retirement gates.

## Ordinary reads: strict unknown-profile admission

Keep the existing native `CatalogQuery` admission check. An unknown profile,
including an empty profile, returns `unknown profile: <profile>` before ordinary
catalog data is returned. Do not add a legacy-compatible read entrypoint that
returns global catalog data for an unknown profile. This is an approved semantic
difference from frozen Python, not a parity claim.

Known-profile reads retain their existing contract. Host scopes, HTTP grants,
leases and snapshot checks remain separate and unchanged. The selected policy
does not add a new profile check to every unrelated legacy API.

## Mapping runtime errors v2

For `projection mapping-events`, replace Python's unhandled traceback with the
native CLI's structured error presentation, while preserving the frozen failure
class, message and exit status 1 for these source-selected runtime failures:

- A valid JSON subject that is not an object: `TypeError`, with the actual
  decoded Python-equivalent type in the `dict() argument after **` message.
- A stored subject containing `id`: `TypeError`, reporting the duplicate `id`
  keyword argument.
- A listing whose event lacks a required summary field: `KeyError`, naming the
  first missing field in the existing summary order.

Machine mode emits the existing envelope on stdout, JSON mode emits the error
object on stderr, and text mode emits `error: <message>` on stderr. No mode emits
a traceback. Syntax/decoder errors, unknown event selection, cancellation,
successful output and full-aggregate-before-selection validation retain their
existing behavior. Stored error-looking values cannot select a failure class.

Private provenance originates at the failing Store check and flows through the
existing detailed-error boundary. Existing Rust `Error(pub String)` APIs retain
their old message strings. This change does not claim public Python exception
object/attribute compatibility or change recovery/publication admission.

Historical JSON captures remain immutable. The runtime residual assertion is
replaced explicitly with the approved v2 expectation; a separate live frozen
oracle suite exercises local process envelopes, read-only state, successful
selection, syntax controls and error-looking data. These are development CLI
checks, not installed-wheel or release acceptance.

## Bounded implementation evidence

On macOS arm64 with Rust 1.99.0, Kyoto built the CLI and compiled the two
Store integration targets; tests were then executed separately. Results:

- Two Store contract tests passed, covering strict profile admission, preserved
  legacy Rust messages, source-selected diagnostics and unchanged catalog bytes.
- Four existing external-crate error API compatibility tests passed.
- The rebuilt CLI passed 123 paired cases against frozen Python commit
  `453fca983222c6665a48775eef67c489c96b527e` using Python 3.14.6. These cover
  117 runtime/selection cases and six syntax/success controls across three output
  modes. Three additional full machine-envelope cases passed separately.
- The 11 contract-ledger audit tests, focused Python lint, Rust formatting and
  whitespace checks passed. Ledger evidence slots remain pending for the full
  aggregate and installed-surface requirements.

Local logs, build job IDs, post-check source hashes and the receipt are retained
under `/Volumes/seed1/Projects/catabolic-builds/evidence/retirement-20261016-contracts`.
The receipt SHA-256 is
`75a9864823fecc0ebe52655b6d54274dc4dff776b5dceea98a38498e991e79d8`;
the tested CLI SHA-256 is
`3b0dc5b2a1993f0997197dd889214b3bea3497ba72754bd5368b503b745276eb`.
This is an uncommitted development composition, not a release artifact. The
earlier runtime-library pass does not qualify this later full composition.

## Composed-source aggregate continuation

The same day, the complete composition was reviewed and run as one workspace
aggregate. Kyoto compiled every test target (`--no-run` policy). The aggregate
then executed separately with `--no-fail-fast`, the pinned
`CATABOLIC_PYTHON_ORACLE`/`CATABOLIC_PYTHON_REFERENCE` variables, the retained
schema fixtures and a physical `TMPDIR` on the external HFS+ volume. Each run
recorded the tracked-diff hash before and after execution. The source did not
change during any run.

| Run | Kyoto compile job | Passed | Failed | Ignored |
| --- | --- | --- | --- | --- |
| 1 | `b9c2b73cc7b54a579d9879315e5c4d03` | 1,394 | 10 | 34 |
| 2 | `c66fe9927ff04cdfbba153d5eaba5ebf` | 1,402 | 2 | 34 |
| 3 | `9a2fab883cd4404f9dc9cd51356ab9c4` | 1,403 | 1 | 34 |

Run 1 also rebuilt pyo3 outside Kyoto, because its `PYO3_PYTHON` differed from
the compile job's. Later compile jobs set it, and runs 2 and 3 compiled nothing.
The counts are libtest executions, not unique contracts.

Every repair below is to a test fixture. Production budgets, deadlines,
supervision and catalog behavior are unchanged:

1. Lineage depth. The test now admits and reads with the maximum 60-second
   `ReadBudget`, instead of the shared five-second fixture budget. The depth
   guard still returns zero components. A timeout still fails the `unwrap`.
2. Composition fallback. The shared fixture policy now sets
   `query_timeout_ms: 30000`, the validated maximum. Capture-query timeouts
   surface as `candidate_evidence_unavailable`, so under parallel load they
   silently reduced the number of probes run.
3. Notification wire oracle. The bounded read now clears `O_NONBLOCK` on the
   accepted socket. macOS sockets inherit it from the polling listener; Linux
   sockets do not.
4. Supervised processor cancellation. The test waits until the helper's PID
   file parses. Shell redirection creates the file before `printf` writes it.
5. Notification providers. Each worker executable is run once, untimed and
   bounded, before any deadline starts. The first launch of a newly linked
   74 MB debug binary took 0.94 s; later launches took 0.01 s. The unchanged
   tests passed 9/9 twice once the binary was warm.
6. Artifact metadata. After the same-size tamper write, the fixture advances
   the file's mtime by two seconds. HFS+ stores one-second timestamps, so a
   rewrite within the same second leaves the published revision unchanged.

Run 3's only failure is `every_available_fixed_preset_validates_actual_output_and_preserves_source`.
The installed Homebrew FFmpeg 8.1.2 has no `libwebp` encoder. The required
`image-webp` preset was not relaxed. Tesseract is still absent, so the OCR
witness remains an ignored, unqualified lane. Replacing either system tool
requires a decision from the user.

Python checks on the run-3 source:

- The four mapping-runtime v2 differential tests passed against frozen
  `453fca9` with Python 3.14.6. They now include signed and unsigned integer
  boundaries, `-0` and `-0.0`, and selection of a valid event when a later
  stored subject is invalid.
- The 11 contract-ledger audit tests passed.
- `tests/test_rust_legacy_mapping_json_diagnostics.py` did not run. Its setUp
  requires the oracle interpreter with SHA-256 prefix `2f7fff38`, and that
  interpreter is absent here. The Homebrew interpreter is `71a669c1`. This is a
  missing prerequisite, not a pass.

GPT-6.1-Sol reviewed the complete diff in six rounds. It found two real
fixture defects: the lineage reads still used the old five-second budget, and
one provider test started its timer before the warm-up. Both were fixed and
confirmed. No findings remain open.

The review also recorded an existing integrity boundary. On coarse-timestamp
filesystems, metadata-only revision checks cannot detect a same-size rewrite
within one timestamp tick. Those checks gate cache admission, request and
playback reuse, component selection, and content delivery
(`content_access.rs`), and content delivery does not re-hash. Treat
coarse-timestamp output volumes as a supported-platform qualification
question. This record does not change that behavior.

Evidence for each run is in
`/Volumes/seed1/Projects/catabolic-builds/evidence/retirement-20261016-aggregate{,-02,-03}`.
The run-3 receipt `result.json` has SHA-256
`7ba86533703f05d3a522aed72bf2aca559992694ee7d4a8099ffb910a45a8c23`.
Run 3's workspace log has SHA-256
`66efa61ab6e5778f31c6a9083d69e4cb6d4fe9f3d65243bdd558e07af1e13bf7`.
The aggregate did not fully pass. Both ledger entries therefore keep their
`current-native-aggregate` evidence slot pending.

## Gates before retiring the Python engine

The current root `pyproject.toml` still packages `src/catabolic`, and the contract
ledger still records `python_default: preserved`, `native_surface: opt_in` and
`release_ready: false`. No milestone closure is recorded. The 159-row forwarding
package is a bounded compatibility candidate, not the complete Python surface.

1. Close native production behavior gaps, including publication/recovery,
   production HTTP and supported-platform supervision. Existing refusals must
   remain until their contracts are implemented and qualified.
2. Prepare complete local test prerequisites and qualify the current composed
   source. The parallel budget and macOS fixture failures are repaired; the
   October 16 run 3 failed only the FFmpeg `libwebp` lane. Still needed: an
   FFmpeg with `libwebp`, Tesseract with English data, and the pinned
   diagnostics oracle interpreter. Then rerun the whole aggregate, including
   ignored OCR witnesses, on committed source. Isolated replays do not replace it.
3. Rehearse upgrade, interrupted migration, restore and rollback on representative
   copies, preserving identities and recording post-upgrade write limitations.
4. Finish explicit public API disposition, distribution/entrypoint migration,
   clean-environment installation and the promised OS/Python artifact matrix.
   Never overlay the compatibility namespace onto an installed legacy package.
5. Complete the outstanding independent review, service, performance and recovery
   gates. Previously declined review lanes are not retried or bypassed here.
6. Switch the default and remove the shipped Python engine only when those gates
   have source-bound passing evidence. Retain the frozen differential reference
   and verified rollback artifacts separately.

No default, live catalog, active workflow, deployment or package publication is
changed by this decision record.
