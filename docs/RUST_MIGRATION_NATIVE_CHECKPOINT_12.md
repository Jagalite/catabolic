<!-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors -->
<!-- SPDX-License-Identifier: MIT -->

# Native checkpoint 12: GraphQL execution and standalone notices

**Recorded result, 2026-10-13:** native Rust/model and standalone-artifact lanes
passed, while the aggregate and separate HTTP journey retain failures. See the
[exact source/artifact/failure receipt](../tests/parity/native-first-checkpoint-12.json).
This checkpoint is **not accepted** as a whole. It does not close any milestone, enable production
HTTP/destructive publication, or change the default Python distribution.

## Rust implementation in this snapshot

- Three parser cases: escaped UTF-16 pairs, constant directives and whitespace in
  wrapped types, with unchanged workspace serialization and identity rules
- Three document-validation cases: alias-conflict/unused-variable ordering,
  duplicate default-object keys and variables discarded by duplicate input keys
- Native source-ordered field collection and GraphQL result completion, retaining
  the existing catalog resolver, authorization/scope, transaction and cancellation
  owners. Repeated fields, null directives/arguments, list completion and
  introspection record accounting have frozen-reference controls
- Direct transfer of arbitrary-width JSON number values without round-tripping
  through a deserializer that can panic or expose private number encodings.
  Positive/negative large integers and nested opaque JSON have exact controls
- The accepted-CR parser view retains distinct source positions; a conservative
  pre-execution conflict refusal prevents incompatible field groups reaching
  resolvers. Complete overlapping-field diagnostic parity remains unfinished
- Five exact upstream dependency license texts are included in standalone
  candidate staging, bound to package/version/source and verified offline cache

The standalone native CI runner, larger parser-diagnostic expansion and corrected
legacy GraphQL file-admission limit are separately reviewed/queued work. They are
intentionally outside this source snapshot. Their later checks cannot be counted
as evidence for this checkpoint.

## Frozen acceptance requirements

1. Clean, exact Git source, reference source and retained migration-fixture fences
2. Native-workspace formatting, all-target strict Clippy, builds and tests;
   `catabolic-python` excluded and no new wheel/wrapper prerequisite
3. Seven exact mandatory ignored tests: the existing six lifecycle/media/admin
   witnesses plus the five-notice offline-cache staging witness
4. Lifecycle/playback models and the composed parser/document/completion/source
   location comparisons through both actual native CLI binaries
5. Existing native numeric-loopback HTTP/generated-client journey on disposable
   media, including the real native worker and transfer owners. Reused client
   output has exact provenance; this is not fresh TypeScript generation
6. Same-source standalone release candidate, manifest/payload verification,
   outside-checkout/Python-denied smoke, staged provider execution, optimized
   output refusals and independent narrow artifact review

Source, artifacts, commands, failures and limits must be preserved separately.
The previous large-integer panic is retained as a failed development result;
its repair does not rewrite that evidence. The candidate remains experimental
and unqualified for release even if all of this bounded acceptance passes.

## Remaining gates

Overlapping-field diagnostic parity, recorded-state scalar lexical differences,
additional parser diagnostics, malformed-variable CLI diagnostics and provider
option gaps remain explicit native behavior work. M6 compatible safe publication
and recovery, unrecognizable private sidecars, denied broad reviews, real mounted
filesystems, macOS, live supported integrations, resource/performance and rollback
evidence remain open. Unsafe destructive paths stay fail-closed outside explicit
disposable fixtures. Frozen Python remains a comparison reference until verified
replacement and rollback permit retirement; the thin wrapper remains last.

## Terminal evidence for source 0861677

Formatting, strict native-workspace Clippy, builds and 582 Rust passing executions
across 62 summaries passed, including all seven required ignored witnesses.
Lifecycle exhausted 52 states/364 transitions and playback five states/1,200
transitions, with zero skipped checks. The complete source, oracle, migration
fixtures and client identities stayed unchanged.

The eight-method composed CLI run failed one retained `conflicting-types`
control: its isolated-lane expectation was the older generic refusal. A separate
exact replay verified that both composed native CLIs now match the complete frozen
Python response for that control, with unchanged catalogs and artifact identities.
The original aggregate is still failed; the follow-up does not rewrite it. The
60 parser, 55 document, 59 runtime and 58 location cases ran, with their separate
retained non-parity controls. Wide opaque-JSON and recorded-scalar tests passed.

The aggregate stopped before generated-resource and HTTP stages. Separate checks
on the unchanged source passed generated tables and bundled docs, then the native
HTTP journey failed at non-idempotent ticket issuance with a retryable 503
`catalog_busy` while the worker was active. That attempt is incomplete. The
request was not blindly replayed. Read-only inspection of this failed disposable
catalog found zero ticket rows and unchanged database bytes; this does not prove
general replay safety or establish a production fix.

The source-bound standalone candidate passed its independent artifact review,
eight safe smoke cases, eight exact staged release-provider tests, the five-notice
witness, 30 optimized output refusals and 16 copy-only negatives. Its manifest is
`af75aa96b89466b86bb9a2871f5c1b679af6f2d4dca7203ad78fe0c349c06d8e`.
All 193 registry dependency records have notice text; five additional workspace
records use the project license. This is not a complete license audit. A separate
same-process replay tied 61 imported Catabolic test-oracle modules to the frozen
reference. Runtime/release qualification flags remain false.

The frozen checkout and immutable artifacts are retained. This result is a
coherent local implementation checkpoint with explicit failed acceptance, not a
reason to enable output/HTTP paths, release, or declare a migration gate complete.
