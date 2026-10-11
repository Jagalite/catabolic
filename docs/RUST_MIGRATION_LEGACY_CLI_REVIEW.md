# Independent preserved-terminal review

This review covers the opt-in `catabolic-legacy-native` transport, frozen parser
resource generation, presentation, scoped local read/control adapters, ledger
accuracy, and the `Application.scan` extraction/forwarding boundary. It is not full
CLI qualification. Python remains the default. No M4–M11 exit gate is closed.

## Findings fixed

1. **Finite float presentation drift.** Python emits `1e-07` and `1e-05`; the new
   terminal initially emitted `1e-7` and `0.00001`. Table, JSON, machine and ordinary
   numeric presentation now reuse the existing Python-compatible scalar encoder.
   Canonical digest encoding was not changed.
2. **Unicode parser diagnostic drift.** Nonprintable separators, format characters,
   private-use and unassigned characters were printed literally instead of using
   Python `repr` escapes. Generation now records 737 compact nonprintable ranges
   from pinned Python 3.14.6 / Unicode 16.0.0. The parser uses those ranges and
   Python's `\\x`, `\\u`, and `\\U` escape widths. No runtime Python is involved.
3. **Populated page column-order drift.** Processing list/attempts, recipes and
   output definitions previously passed empty-page checks but rendered populated
   SQL-backed records alphabetically. Their known page shapes now preserve the
   frozen output's column order. Arbitrary nested object insertion-order parity
   remains unclaimed.
4. **Incomplete read-budget coverage.** Bindings and individual facts bypassed the
   streamed row budget; HTTP-operation listing bounded IDs but loaded full backing
   definitions later. These new adapters now bound backing records and aggregates,
   inspect SQLite text/blob lengths before copying oversized values, and retain
   four-MiB canonical row limits. Unpaged oversized results fail explicitly instead
   of returning a complete-looking prefix. This is an intentional native safety
   limit, not parity for previously unbounded Python results.
5. **Owner-ledger ambiguity.** Fifty reviewed terminal routes now name their exact
   entrypoint or embedded resource. The other 192 rows explicitly identify their
   mappings as family associations. Route coverage remains 203 adapted/unqualified,
   28 missing, 10 partial and one safety-gated; none is promoted by this review.
6. **Invalid UTF-8 argv panic.** Unix byte arguments previously panicked inside
   `std::env::args()` and exited 101 before parsing. The entrypoint now converts
   `args_os()` explicitly and refuses invalid UTF-8 with a fixed stderr diagnostic
   and exit 2 before catalog access. Valid-Unicode behavior remains covered. This
   does not claim Python surrogate-escaped byte-path support or machine envelopes
   for an input that cannot reach the parser.

## Independent evidence

- `tests/test_rust_legacy_review.py`: 13 methods with independent frozen processes
  and disposable catalog copies, covering finite floats, Unicode errors,
  environment selection, populated processing/recipe/definition pages, pagination,
  result omission, read budgets, fallback bounds/durable effects, HTTP-operation
  profile fences, source extraction, resource regeneration, invalid UTF-8 argv
  refusal without catalog mutation, and default separation
- `crates/catabolic-store/tests/public_read_budget_review.rs`: four tests for an
  exact four-MiB record and one extra byte, total-many-row refusal, backing HTTP
  definitions, oversized facts before collaborator invocation, caller transaction
  and TEMP identity, `query_only`, and unchanged database state
- The selected ordinary command comparisons assert stdout, stderr and exit status
  exactly. Oversized unpaged output is a separately asserted compatibility
  difference: frozen Python succeeds while native returns machine error/exit 2
- The original parser and terminal/Plex/inbox suites are rerun against copied
  review binaries. Parser evidence remains separate from command-owner evidence
- The generated resource reproduces byte-for-byte with Python 3.14.6,
  `PYTHONHASHSEED=0`, and the verified frozen source inventory. All 16 recorded
  root Python CLI/documentation sources still match the frozen checksums
- `public_scan::execute` matches the original `c69859e` scan body after only module
  relocation. Current embedding forwards the same Session-owned mutable Store
- The legacy binary contains no Python-process fallback or companion registration.
  Only the original native companion self-registers; installed Python registration
  still requires the packaged companion hash. No legacy-binary substitution was added

Final copied artifacts, scoped source hashes, per-matrix parser reports, test logs
and the aggregate evidence record are under `.local-tests/legacy-review/final/`.
These are local development artifacts, not helper-paired installed release proof.
The final rebuilt binary passed all **48 Python test methods**: 13 independent
review methods, 23 existing command/Plex/inbox methods and 12 parser methods
containing **20,170 comparisons**. The four independent Rust read-budget tests
also passed. Scoped input/source hashes were unchanged during the final run.
Resource and ledger regeneration were byte-identical after Python formatting;
Ruff and scoped CLI Clippy with warnings denied passed.

## Residual limitations

The preserved parser/tree and selected command checks do not qualify every flag,
every output shape, every mutation schedule or all signal timings. Arbitrary
dynamic/extension object ordering, Python exception subtype/diagnostic parity and
all other explicitly recorded ledger gaps remain open. No workspace-wide JSON
`preserve_order` change was made.
Native argv must be valid UTF-8; Python's Unix surrogate-escaped byte filenames
remain outside this adapter's compatibility claim.

Scan continuation, multi-source freshness barriers and scan-specific cancellation
gaps remain. The old helperless development wheel and explicit companion still
match their recorded hashes, but the embedding module changed after those tests;
their two cancellation/reopen/writer-progress checks are historical evidence.
A final frozen-source helper-paired installed lane is still required.

Scheduler selection, GraphQL and inbox owners are collaborators with separate
reviews. Previously blocked broad definitions/curation/programs/plans/interchange/
manifest/layout and filesystem/hardlink/source-access/observation/remount/watcher
reviews were not retried or reclassified. This review does not resolve those
underlying safety-review blockers. No live accounts, credentials, real catalogs,
external service writes, commits, pushes, releases or default switches were used.
