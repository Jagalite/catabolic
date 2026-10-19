# Installed XML qualification driver review

2026-10-11. Two installed XML drivers now support strict exact-wheel mode and an
explicit development-extension mode. Their declared two-case inventories are
fixed and checked by the common result verifier. Strict mode checks complete
installed wheel/helper payloads before and after the scenarios; reports fence
source, fixture, extension and artifact identity. Optimized Python is refused
before fixture imports or report writes.

Independent review found a stale-receipt defect: missing or failing fixture
imports could exit nonzero while retaining a previous passing report. Both
normal CLI entrypoints now clear an explicitly named report before importing
fixtures. Library imports neither inspect the caller's arguments nor touch a
report. The existing optimized-entry early refusal still leaves files untouched;
callers must honor command status, not treat an old receipt as success.

## Evidence

- 64 harness/plumbing tests pass, independently and in a parent replay
- Twelve missing/runtime-error/syntax-error fixture-import subprocess cases
  reproduce stale reports against the pre-correction drivers and pass after fix
- 54 optimized-entry variants refuse before fixture import/report handling
- Both drivers pass exactly two real scenarios against the preserved development
  extension, for four total installed development scenarios
- Both strict attempts against the helperless wheel correctly refuse and remove
  a pre-existing report
- Strict happy-path harness cases use the actual wheel verifier with disposable
  synthetic wheel/helper payloads; they do not execute a fabricated helper
- Source, wheel, installed payload, extension, missing/extra/duplicate/skipped
  test, empty run and count-drift cases remain fail-closed
- Scoped lint/format and diff checks pass

Exact local commands, source snapshots and logs are retained in
`.local-tests/plex-xml-qualification-independent/`; parent replay is
`.local-tests/rust-migration/phase3/preflight/xml-qualification-parent.log`.

Final driver hashes:

- `native_plex_xml_embedding_tests.py`:
  `6089cd908b093eb3d1a921e6590ed028f5aa8f2831cb39c709ede623defc889b`
- `native_plex_xml_review_tests.py`:
  `ba5ee38c53d808c6ea2b10094261173931472b588f1759c90ee55f933252c7eb`

The workflow invokes both strict gates and retains their separate receipts.
Actual GitHub Actions execution and the final real paired-wheel replay remain
pending. No production XML code changed in this review. No platform, release,
whole-product parity, or migration hard-gate completion claim follows.
