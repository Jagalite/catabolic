# Native M4–M11 implementation checkpoint

This is an **opt-in implementation checkpoint**, not a completed migration or a
release candidate. Python remains the default implementation. No M4–M11 hard
exit gate is complete. The historical M1–M3 receipts in
[RUST_MIGRATION_NATIVE.md](RUST_MIGRATION_NATIVE.md) apply to their recorded source
snapshot and platforms; they do not automatically qualify these changes. M0's
remaining scope, performance and baseline discrepancies remain explicit in
[m0-gates.json](../tests/parity/m0-gates.json).

The frozen Python reference is `453fca983222c6665a48775eef67c489c96b527e`, schema
32, HTTP contract 1.7.0. No schema migration, Python retirement, engine fallback,
release publication or automatic default switch is introduced here.

## Implemented vertical slices

The native CLI and `catabolic_native.execute_operation` call the same runtime
owners. Each admitted mutation has one native Store owner. An unsupported owner
or action fails explicitly; it never retries a mutation through Python. The
companion's action/JSON interface does not claim legacy CLI presentation or full
public Python facade compatibility.

- M4: tags, curation, saved definitions, interchange, manifest construction and
  publication, program bundles, estimates and typed read plans. See
  [definitions](RUST_MIGRATION_DEFINITIONS.md).
- M5/M6: guarded observations, remounts, observation/query watchers, managed
  symlink and hardlink journal owners. See
  [observation and output](RUST_MIGRATION_OBSERVATION.md). Destructive symlink
  effects and hardlink retirement are blocked in release builds while their
  safety gates are open.
- M7/M8: bounded processes, real media analysis, text/subtitle/PDF/OCR extraction,
  render presets, artifacts, processors, receipts, components and packet-verified
  extraction/remux, plus immutable fallback policies. See
  [media](RUST_MIGRATION_MEDIA.md).
- M9: notifications, optional Apprise adapter, managed outbound HTTP, consumer
  publication and native playback. See
  [integrations](RUST_MIGRATION_INTEGRATIONS.md) and the playback section below.
- M10/M11 preparation: shared CLI/PyO3 delegation, installed-wheel smoke and
  exact installed-payload verification, fail-closed unsupported interfaces and
  reproducible local qualification. Full transport/facade/distribution and
  release qualification remain open.

Canonical JSON preserves arbitrary integers and legitimate
`$serde_json::private::Number` object keys, rejects nonfinite floats and retains
the frozen encodings used for identity. Interchange alone rejects duplicate keys
where the released contract requires it. Existing SQL migration resources remain
unchanged. Capability and media subprocess waits occur outside the writer lease;
reentry checks identity, source revisions, current authorization and job fences.

## Playback and content boundaries

Native playback persists reservations, sessions, shared jobs and attempts in the
existing schema. A production pure reducer controls claim/fence/cancellation
decisions. Admission pins item, source revision, recipe and destination; each
interested session freshly authorizes its own operation. One caller canceling
cannot retire a different authorized caller's job, including callers using
different operation identifiers for the same recipe.

The runtime uses real FFmpeg, bounded queues, private cache directories,
descriptor-pinned source access, growing HLS playlists and guarded segment names.
Readonly/detached/enclosing-transaction worker or cleanup calls refuse before
filesystem effects. Source changes, lost authorization, expiry and stale results
release reservations without serving the changed source. Encoder execution is
qualified only on this Linux environment; non-Linux execution is blocked pending
its worker-death and process-lifecycle qualification.

Content descriptors, byte ranges and tickets recheck current principal, token,
grant and source evidence. Reads check descriptor metadata before and after
reading. Credential files, registered private files, and live catalog/WAL/SHM/
journal inode aliases are refused. An unregistered orphaned private sidecar alias
cannot always be identified after SQLite removes its original pathname; that
remains a content/HTTP release blocker. The recorded `787b5ed` checkpoint had no
native HTTP listener. Later work adds an explicitly experimental loopback
listener, documented below; it is not qualified for general deployment.

The Statelessness playback adapter calls the production reducer and checks an
independent input/effect ledger. The bounded graph and injected stale-completion
fault have retained shrink/fresh-process replay evidence. Separate actual SQL
tests compare committed effects to reducer plans. These bounds do not establish
general liveness or every cross-workflow interleaving.

The earlier M1 two-caller lifecycle fixture is separate historical evidence, not
the rendition demand contract. Its last-caller cancellation stops physical work;
frozen API rendition cancellation does not. It must not be cited as production
M7/M8 shared-demand qualification.

## Safety discrepancies and review corrections

The following are intentional restrictions/corrections, not silent parity claims:

1. Frozen symlink replace/remove has a check-to-syscall foreign-leaf race. Native
   default/debug and all release builds fail closed for those effects. Only a
   debug `disposable_fixture` exception exercises unchanged cross-language journal
   semantics. A shared safe protocol remains required before real-output use.
2. A hardlink retention race preserves a swapped foreign file's bytes but can
   relocate it into retention. Hardlink replacement/removal now has the same
   explicit disposable-debug-fixture restriction, checked before admission and
   unfinished-journal recovery; release builds cannot opt in. This is incomplete
   functionality, not a resolved race or M6 qualification. Marker-temporary
   cleanup races also remain open. Neither is qualified merely because existing
   deterministic tests pass.
3. Explicit consumer retry cannot steal a live lease; idempotent enable/disable
   cannot invalidate one. Frozen Python permits those races. Playback likewise
   fixes the frozen first-caller operation coupling described above. These safer
   discrepancies need explicit acceptance before a full-parity/default claim.
4. Retained historical effects may be acknowledged during recovery, but changed
   desired mapping/source evidence must remain unhealthy until currently valid
   publication is verified. Regressions distinguish bookkeeping from publication.
5. Subtitle numeric parsing deliberately follows frozen Python 3.14/Unicode 16,
   including Unicode decimal digits and legal underscores. The frozen fixture
   records differences from older Python Unicode datasets instead of silently
   dropping valid cues.

Independent media, outbound and playback/packaging reviews found and rechecked
concrete fixes, including timestamp cue loss, consumer lease theft, shared caller
isolation, readonly cleanup side effects, empty-suite false qualification and
unrelated-wheel false qualification. Separate independent definition and
filesystem review execution was unavailable; those review lanes remain incomplete.
Ordinary tests and partial inspection do not replace those missing reviews.

## Qualification and reproducibility

The second source-stable checkpoint is commit `55d5448`, recorded separately in
[native-m4-plus-checkpoint2.json](../tests/parity/native-m4-plus-checkpoint2.json).
On the same Linux x86_64/glibc 2.41 platform it passed 526 distinct native
scenarios without skips, 245 distinct Rust checks, strict Clippy, both generated
TypeScript client builds, and the real experimental HTTP/client/worker journey.
The Rust count deduplicates eleven overlapping document checks and includes the
two PDF/OCR and one Python-journal checks explicitly enabled after the default
workspace run. No ignored Rust check remains unrun.

The paired helper/extension wheel passed exact installed-payload and subprocess
ownership checks on Python 3.11.15 and 3.14.6. Each interpreter also passed 13
installed embedding tests and 33 frozen/native result groups across 47 selected
signature rows (41 selected-contract checks and six explicit partial contracts).
The separate Python application run completed 1,041 tests: 1,040 passed and the
one known case-insensitive-filesystem test was skipped. Thirty release refusal cases verify the closed
symlink, hardlink-retirement and fallback-publication gates. This is bounded
checkpoint evidence, not full API, platform, M6 safety or release qualification.

The native aggregate verifies unchanged source, both executables, the frozen
oracle's 518 resources and 32 historical databases plus their manifest before and
after execution. The receipt records hashes of retained failed/interrupted
attempts as well as passing runs. In particular, review reproduced an HTTP/1.0
connection-reuse defect; the corrected transport uses fresh connections without
automatic request replay, and its permanent regression passes in this aggregate.
New Plex and additional workflow-facade work in the continuation branch is not
included in this checkpoint. No GitHub Actions run or non-Linux test is implied.

[native-m4-plus.json](../tests/parity/native-m4-plus.json) records unique scenario
identifiers, artifact/source hashes, observed outcomes, review coverage and open
gates. Source hashes refer to recorded checkpoint commit `787b5ed`, not later
unqualified implementation edits. Evidence logs are local checkpoint receipts, not published release assets.

The 2026-10-11 Linux x86_64 checkpoint passed 286 unique native differential
scenarios without skips and 167 unique Rust checks, including separately enabled
PDF/OCR and Python journal-oracle tests. Release output refusal and both retained
model fault/replay checks passed. The rebuilt `cp311-abi3-manylinux_2_39_x86_64`
wheel passed outside-checkout imports, mutation smoke and exact installed-payload
comparison on Python 3.11.15 and 3.14.6. This is not macOS or older-glibc evidence.

The original Python suite passed 1,034 tests on Python 3.12, with one
case-insensitive-filesystem skip. Its first Python 3.14.6 rerun had three CLI
subprocess import failures because the isolated environment lacked an installed
Catabolic package. Installing the frozen reference resolved all three focused
rechecks. The fresh full Python 3.14.6 rerun then completed 1,034 tests with
zero failures/errors and the same one filesystem skip. The failed attempt is
retained rather than rewritten as a pass. Tests do not claim a
case-insensitive filesystem was exercised here.

```sh
# Requires the pinned Python reference/legacy fixtures and external media tools.
scripts/check-native-mutations.sh

# Run outside the checkout using a clean environment containing the actual wheel.
/path/to/clean-venv/bin/python /path/to/scripts/qualify_native_wheel.py \
  --binary /path/to/copied/catabolic-native \
  --wheel /path/to/catabolic_native.whl --receipt /path/to/receipt.json
```

The mutation runner rejects zero scenarios, duplicate identifiers, leaked
Python-only oracle cases, incomplete outcome coverage, skipped tests, optimized
Python, and implementation/test-source or executable changes during the run.
It binds both CLI and HTTP executables, verifies the frozen oracle and historical
fixture bytes before/after execution, records actual oracle imports, and starts
with a fresh bytecode cache for itself and subprocesses. A failed preflight
cannot retain an earlier passing receipt. The wheel runner rejects optimized
Python and malformed archives, and compares each installed payload and the imported extension with the supplied
wheel bytes. A passing smoke for an older artifact does not qualify a rebuilt one.
Debug fixture tests cannot qualify release destructive-output safety.

## Remaining hard gates

### Work after the recorded checkpoint

The following owners are covered by the selected checks in the `55d5448`
aggregate register, with the documented acceptance gaps still open. They are not
covered by the older `787b5ed` register:

- Item workflow, rule orchestration and reviewed desired layouts:
  [workflow checkpoint](RUST_MIGRATION_WORKFLOWS.md)
- Target metadata, XSPF/OPDS/NFO, detached bundles and official CLI imports:
  [export/import checkpoint](RUST_MIGRATION_EXPORTS.md)
- Detached observation scheduling, generated inventory, watcher supervisors and
  refresh generation handling: [observation checkpoint](RUST_MIGRATION_OBSERVATION.md)
- Shared rendition requests, component selection and actual package publication:
  [component checkpoint](RUST_MIGRATION_COMPONENT_SELECTION.md) and
  [shared rendition checkpoint](RUST_MIGRATION_SHARED_RENDITIONS.md)
- Experimental HTTP 1.7 transport, actual worker/stream journeys and generated
  clients: [HTTP checkpoint](RUST_MIGRATION_HTTP.md)
- Qualified-helper packaging and installed subprocess ownership:
  [artifact checkpoint](RUST_MIGRATION_NATIVE_ARTIFACTS.md)
- Fresh fallback publication verification and fenced consumer delivery:
  [fallback consumer checkpoint](RUST_MIGRATION_FALLBACK_CONSUMERS.md)
- Persistent native Store and selected embedding facades:
  [embedding checkpoint](RUST_MIGRATION_EMBEDDING.md), with explicit partial
  contract coverage and completed scoped lifecycle/provenance review

The [public-surface inventory](RUST_MIGRATION_PUBLIC_SURFACES.md) separately
enumerates remaining preserved Python entry points, legacy CLI branches and Plex
owners. A path-based companion operation is not a persistent-facade or legacy
parser/presentation qualification.

The second checkpoint binds those reviewed fixes to its own source-stable
aggregate and installed artifacts. Subsequent implementation changes require a
fresh receipt; do not combine overlapping counts or attach old source hashes to
new work.

### Exit conditions still open

- M4: complete frozen-ledger coverage of revisions/digests/unknown fields,
  full reviewed workflow integration, real official-application acceptance, and
  the blocked independent definition-boundary review
- M5: native event monitoring, the complete supported regex dialect, full watcher
  exploration/scale, unmount/disk-full/permission cases and real platform evidence
- M6: compatible safe destructive-output protocol, all required crash boundaries,
  both rollback directions, blocked independent filesystem review and real
  Linux/macOS adversarial filesystem evidence; logical publication is additionally
  restricted to explicit disposable debug fixtures while qualification is open
- M7: all resource/late-result/cancellation scenarios, complete runtime integration
  and production-reducer/independent-ledger evidence for every execution family
- M8: full fallback-family ledger coverage and budget/replay review, consumer and
  scheduler integration, plus qualified publication under the M6 dependency gate
- M9: full Plex workflows, actual supported consumer/provider acceptance,
  scheduler integration and non-Linux growing-playback qualification
- M10: every HTTP 1.7.0 black-box/authentication/client gate, unresolved private
  sidecar and scoped-query cases, the complete documented Python facade and
  installed-platform/resource compatibility
- M11: every frozen ledger row, missing independent review, platform/resource and
  performance budgets, handover/rollback rehearsal and exact artifact approval

Until those gates close, retain the Python distribution and do not enable native
managed-output writes outside disposable fixtures or publish a feature-complete
native release.
