# Native import restart admission v1

This is a bounded Catabolic-side restart adapter for native schemas 34 through 37.
It does not implement Motion checkpoint persistence, application-reference
migration, a new identity/job owner, or a production two-owner deployment.

## Explicit host contract

A host resuming the import handoff must call
`HostSession::open_after_import_cutover` with
`IMPORT_RESTART_CONTRACT_VERSION = 1`. Its inputs include the trusted host's
persisted original activation `CommandEnvelope`, recorded `HostCutoverCheckpoint`,
current independently resolved receipt-read authority, the next authorization
revision, and the existing receipt policy.

`RecordedImportHandshake` is trusted host input, not a request DTO and not proof
that Catabolic committed. The host must durably preserve the entire original
command identity, incarnation, generation and body. `canonical_payload()` is the
canonical complete semantic envelope; it is not merely the body to pass back to
`CommandEnvelope::new`. The fixture persists the body and original digest, then
checks digest equality when reconstructing the exact original intent.

The host also owns persistence and correctness of its reference acknowledgment.
It must keep application and external work frozen until its actual durable phase
and Catabolic's actual durable activation receipt agree. A matching digest or
boolean cannot establish that playlists, preparations, markers, progress,
history, offline or download references were updated. Missing trusted checkpoint
information cannot authorize calling an ordinary constructor with a fresh gate.

`HostSession::open_authorized` is unchanged. It accepts an existing trusted shared
gate and is not advertised as enforcing import restart handoff. This adapter is
an explicit integration contract, not an automatic restriction imposed on every
privileged host or Store constructor.

## Receipt-only reconciliation and admission

The factory opens the existing native writer session, checks the supported schema
and retains that lifetime through admission. `reconcile_persisted_activation`:

1. Requires the held writer owner and no ambient transaction
2. Checks checkpoint identity/digests and exact activation operation/body binding
3. Calls the existing durable command owner with `allow_new_commands = false`,
   regardless of the supplied current authority's setting; it never recomputes a
   new activation command or executes an absent intent
4. Accepts only the retained, currently visible full activation receipt
5. Runs `reconcile_activation` against the actual current catalog/incarnation
6. Before positive admission, rechecks settled durable work, the complete bounded
   native ownership inventory, watcher ownership, every retained observation's
   exact schema 37 cleanup witness, and the existing native journal restriction

Missing, conflicting, expired, redacted, restore-unknown or corrupt evidence never
creates a gate. Invalid data or owner/open errors return an error; other closed
states return a closed `ImportRestartAdmission`. Neither form exposes a host.
`KeepAdmissionClosed` and `FinishHostActivation` both withhold the `HostSession`.
The latter is an instruction to reconcile the host's own remaining work, not
permission to fabricate acknowledgment. Only `AdmitCatabolicOnly` creates and
exposes a fresh shared gate with a writable host session. The same gate can be
cloned for existing scoped readers/private writers, and normal lease revocation
continues to cover admitted work and retained result delivery.

## Bounded qualification

`native_import_restart` contains process-exit fixtures that sync a host checkpoint
file and exit without Rust destructors, then reopen through the factory. They
cover the missing-activation, committed-before-host-acknowledgment and recorded
activated boundaries across 34–37; mismatch/current-visibility/compacted-expiry
cases; original-envelope preservation; and fixture-only incarnation rotation.
A claimed activated phase cannot make an absent receipt execute, even when the
caller supplies `allow_new_commands = true`.

Explicitly invoked actual-helper cases on 37 also lose the process after a real
companion seal, after validation, after activation before host acknowledgment,
and after host acknowledgment. The seal-only fixture starts with a retained
original staged intent, then loses the process immediately after sealing; the
stale pre-seal intent and absent activation remain closed. Its positive case uses
the fresh gate for a bounded saved read, host mutation, schema 37 scoped reader,
private metadata writer, and an actual closed scan. Committed writer and scoped
reader results remain held while the real scanner joins; revocation cannot drain
until both results finish delivery. Equal-label foreign gates cannot reuse their
capabilities, and revocation refuses all operation families. Recorded PIDs and exact witnesses are preserved. Negative cases recheck
changed claims and unrelated pending, terminal, API or unwitnessed observation
owners after the receipt
already committed. A positive witness for one scan cannot waive another owner.

Older 34–36 allowed/refused scan exercises use an explicitly controlled retained
child under their historical contract. They are not companion-cleanup evidence.

The separate `native_import_staging` additions compose metadata, ordered pin
history and native bindings in single- and cross-batch imports across 34–37. A
late binding failure rolls all earlier owner changes, generation, search,
receipts and holds back. Real exits after a binding write and after transaction
commit exercise the exact mixed-owner boundary, followed by resume or replay.

## Unchanged limits

No schema migration or frozen schema resource is added or changed here. Actual
external owner/reference work is deferred. Real restore publication, WAL cutover,
destructive filesystem publication, interval media effects, non-Linux/release
gates and the blocked independent reviews remain unqualified and fail closed.
Alternative multi-part/cut/equivalence shapes and the broader contribution-source
semantics through the importer remain separate qualification gaps; these runtime
fixtures do not convert independent owner tests into import-path evidence.
