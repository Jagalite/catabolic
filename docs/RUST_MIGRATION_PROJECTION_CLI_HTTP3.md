# Preserved projection CLI: narrow HTTP-v3 forwarding

This change forwards `projection mapping-preview` and `projection mapping-apply`
for version-three definitions to the existing production
`catabolic_store::http_mappings::execute` owner. It does not change that owner,
its transaction, any destination publication/recovery owner, the installed Python
default, release gates, or any native fixture restriction. The CLI directly uses
the workspace-pinned libc already present in the lockfile for stdin polling; no
dependency version changes or downloads are introduced. Both CLI leaves remain
**partial**, not behaviorally qualified. There is no Python runtime fallback.

## Adaptation

- The frozen parser and flags are unchanged. Definition files/stdin retain the
  existing one-MiB CLI input bound. HTTP-v3 intentionally ignores `--max-removals`,
  including negative values, just as the frozen dispatcher does. Stdin admission
  now checks cancellation around bounded poll/nonblocking reads and restores its
  original descriptor flags before returning; the existing supervisor still joins
  its worker. There is no detached input reader. Ordinary file reads retain the
  existing CLI helper and diagnostics
- Apply forwards the existing interoperable `.plex-metadata.lock` writer owner
  before ordinary Store admission, and releases it on return/error/interruption
- Empty expected plans fail before opening the Store, after metadata locking;
  preview uses a read-only Store and does not acquire that metadata writer lock
- The HTTP owner prepares the plan and atomically enqueues its existing default
  maximum of 100 changes. This CLI path does not invoke a delivery worker
- The existing SQL interrupt callback and CLI signal bridge are forwarded. The
  original interruption fixture signals after observing metadata-lock readiness,
  checks exit 130 and both output channels, verifies neither catalog changed, and
  reacquires both advisory locks. Lock readiness does not prove query execution
  has begun. A separate regression keeps a stdin pipe open after the child consumes
  a JSON value, signals both mapping leaves one second later, and requires exit 130 before EOF
- Known mapping ConsumerError, missing-operation AccessError and catalog-writer
  CatalogBusy provenance are restored at this CLI boundary. Known plan/row/request
  output-field orders use the ordinary native renderer

## Evidence and limits

`tests/test_rust_legacy_projection.py` runs independent frozen Python 3.14.6 and
native processes against separate disposable copies. Its synthetic OpenAPI
operation has no authentication and uses `127.0.0.1:9`; no server or worker runs.
The fixtures check text, JSON and machine stdout/stderr/exit; stdin; valid preview
with no changes; successful apply with separate random event IDs and untouched
pending delivery attempts; stale/empty plans; ordinary validation; both lock
contention paths and release; interruption after lock readiness; both-leaf
open-stdin interruption before EOF; ordinary and malformed file/stdin controls;
and closed unavailable routes.

A named test records a real remaining mismatch: arbitrary input definition/body
insertion order is lost by the existing owner's sorted JSON Value representation.
Selected standard-order fixtures compare exact bytes, but arbitrary nested output
order is not qualified. Malformed JSON/file-read diagnostics, unusual path
admission ordering, all underlying owner edge cases and cancellation arriving
after query evaluation at enqueue/commit are also not fully qualified here.
No full-leaf, installed-artifact, release or platform-parity claim is made.

## Remaining requested commands

- `mapping-events`: the follow-on [read-only history adapter](RUST_MIGRATION_MAPPING_HISTORY.md)
  implements combined publication/HTTP history, complete event selection and the
  original bounded tail. It remains partial for the documented diagnostic and
  noncanonical stored-payload ordering gaps
- `mapping-preview`: v3 is forwarded; v1 remote metadata/collection and v2
  folder/export/import planning still need their actual orchestration contract.
  Existing HTTP, layout, export and import components do not establish equivalence
  to those complete destination mapping plans
- `mapping-apply`: v3 only queues reviewed HTTP requests. v1/v2 publication is not
  substituted by another command. Dependent publication gates remain unchanged
- `mapping-recover`: the follow-on [read-only history adapter](RUST_MIGRATION_MAPPING_HISTORY.md)
  resolves the combined history and forwards HTTP-v3 queue status. Its `--apply`
  rejects with `mapping_http_recovery_uses_notification_queue`. Other versions
  still require actual destination recovery orchestration and existing gates;
  no synthetic success or recovery effects are substituted
- `preview`: now has an independently scoped
  [rollback-only ordinary composition](RUST_MIGRATION_PROJECTION_RUN.md) over the
  sealed layout and public reconciliation owners. It remains partial;
  fallback-bound planning refuses before its gated dependencies
- `execute`: now has an explicit early refusal entrypoint before catalog admission
  or dependent metadata/output planning. The frozen commit/apply/verify/report
  contract remains unavailable while public filesystem publication is gated

The owner ledger generator records exact v3 entrypoints and these missing
contracts. The HTTP-v3 preview/apply and combined history/recovery leaves remain partial; the separate ordinary preview/execute
adapter is also partial, with execute explicitly refusal-only. This is local
adaptation evidence, not
milestone promotion or reopening a broader owner review.

## Later ordinary v1 owner

The separate [v1 destination owner](RUST_MIGRATION_DESTINATION_MAPPINGS.md) now
implements native metadata/explicit-collection preview, reviewed apply and
readback recovery. The HTTP-v3 evidence above remains scoped to its recorded
source. V2, installed artifacts and full-leaf/platform qualification remain open.
