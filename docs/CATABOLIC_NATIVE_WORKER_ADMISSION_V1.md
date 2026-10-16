# Native worker admission and bounded cleanup, version 1

This is an opt-in native catalog contract. The default catalog remains schema32.
Schemas33–35 retain their recorded bytes and behavior. Additive schema36 provides
an exact execution-lifetime record for newly acquired claims; it does not
retroactively certify an old claim or enable production catalog migration.

## Helper admission

Native observation, watcher and fallback companions use versioned commands with
an envelope bound to the catalog ID, incarnation, schema and database device/inode.
Missing, malformed or stale envelopes are refused before admitting work. An old
helper's command refusal is not retried through the legacy unfenced route.
Schema32 keeps its explicit legacy route.

The exact retained child remains the source of process-exit knowledge. Cancellation,
catalog close, reconnect failure or a bounded wait ending does not release its
resource permit early. An expired claim, terminal database row, absent/reused PID
or a failed process lookup is not evidence that a native worker has stopped.

## Schema36 builtin cleanup

Native application hosts use `HostSession::open_authorized` with their existing
shared `HostAuthorization`. Equal namespace/revision labels cannot replace that
exact gate. Native ungated `open` explicitly refuses; schema32 operator open stays
compatible. Reads, item mutation, helper registration and scan admission acquire
leases. A scan transfers its lease with the resource permit into the retained
child supervisor. Existing-worker polling/drain/cancellation remain available
after revocation; an unrelated new operation does not. This privileged host handle
does not replace the separate scoped read/private writer boundary.

The shared-gate owner tests and actual native35/36 two-consumer cancellation case
are recorded in `tests/parity/native-catalog-r6-checkpoint.json`. They establish
local retained-child ownership, not durable reconstruction of a lost supervisor.

The existing processing Pool may positively retire only the closed native
sniff/hash/verify worker cohort described here. Its private ownership capability
binds the catalog lifetime, immutable job snapshot/options, operation, generation,
attempt, claim-token digest and a fresh private runtime nonce. The lifetime row is
created in the same transaction as the new claim. Historical claims are never
backfilled.

The capability is retained by the exact JoinHandle, rather than inferred from a
notification channel. A successful join retains its cleanup proof when cancellation
or publication failure discards the ordinary result. Capacity buckets and Pending
ownership are returned before the witness is sealed. A definite thread-spawn
failure can retain the exclusively owned unstarted plan for this same drain path.
A panic, unknown join, missing proof, changed identity/claim or failed SQL seal
leaves the claim reconciliation-required.

The seal is immutable and atomic with lease release. Native retry/reacquisition
requires the matching quiescent witness even when the original host PID remains
alive. Ordinary query APIs cannot write the witness tables, and the HTTP SQL
surface cannot read them.

### Deliberately excluded work

External probes, text/decode tools and injected callbacks cannot mint this builtin
capability. Linux source validation with a non-null volume UUID may invoke
`findmnt` outside the runtime process guard, so the entire UUID-checking cohort,
including malformed non-null UUIDs, is excluded. Existing UUID validation still
runs; it is never disabled to earn a witness. A pre-existing explicit UUID-skip
policy can remain eligible. The runtime guard rejects its own subprocess path
inside the closed worker, but is not a universal OS spawn interceptor.

The witness is for the exact processing worker and claim, not all effects in its
caller or optional later publication/catalog refresh. Synchronous `media::run`,
observation companion lifetimes, arbitrary descendants, process-group recovery,
unknown joins and SQL-seal failure recovery need separate supported owners and
qualification. In particular, a terminal observation row does not qualify normal
reopen as fully recovered. Drop may still block; bounded explicit waits do not
constitute a global shutdown guarantee.

## Evidence and remaining gates

`tests/parity/native-catalog-r4-r5-checkpoint.json` records the integrated scoped
relationship and schema36 slice, exact source manifest, individual commands,
artifact hash and real-helper opt-in cases. Its focused results do not qualify
the whole workspace, every platform, watcher reaction scheduling or arbitrary
orphan recovery. Earlier receipts remain evidence for their own source cohorts.

The independently blocked schema/definition/filesystem reviews remain incomplete
and have not been retried or reassigned. WAL cutover, controlled restore,
destructive publication and interval effects remain fail-closed where unqualified.
R5, R7 and every applicable M0–M11 safety/recovery/platform/release obligation remain
open. Actual Motion integration and two-owner activation remain deferred under the
Catabolic-first instruction.
