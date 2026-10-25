# Fixed source-succession proof budget

The closed completed-session source preparation, execution and permanent-receipt
owners use a fixed 65,536-unit local work ceiling. The same-owner candidate test
corridor uses the identical profile; generic structural readers do not select it.
No command field, caller callback or configuration can select a different ceiling.

This is a source-specific safety policy, not a promise that every valid catalog
fits. The original NFO constructor retains its 8,192-unit work ceiling and error
behavior. Every profile retains 2 MiB cumulative proof bytes and 8,192 cumulative
JSON value nodes, original raw-document depth and raw-stream byte/node/work
limits, and the exact inherited operation identity, cumulative limits, allocation
sink, cancellation and deadline. No new parent or budget reset is introduced.
The ordinary parent remains 500,000 work units with its existing five-second
operation deadline and row/value/byte limits. A tighter actual parent wins.

The independent 64 Ki policy ceiling comprises 32 Ki potential 64-byte quanta
from 2 MiB plus four 8,192-unit structural passes. It is not an algorithmic bound:
SQL, repeated history validation, key comparisons, shifts and other real work
are charged too. Larger histories or sources can still fail this ceiling, the
unchanged byte/node limits, source membership limits or cumulative parent limits.
It does not assert scalable whole-library relocation or unrestricted history.

An isolated diagnostic of the actual two-file, one-context, two-hop completed
fixture measured local work 1,403/7,794/3,285 for first preparation/mutation/receipt
and 4,642/11,028/6,520 for the corresponding second-hop owners. Maximum local
usage was 5,399 nodes and 1,025,484 bytes; shared second-mutation work reached
11,872 including terminal checks. These measurements motivated separating the
whole-source domain from the NFO source-document profile, not changing prices or
omitting any proof. They are diagnostic evidence, not production qualification.

A source-local byte overflow records sticky ByteBudget on the real parent;
node/work/depth overflow records sticky WorkBudget. Existing parent cancellation,
timeout or failure remains the first failure. Reconstructing a local wrapper or
swallowing a callback error cannot make the operation committable. Tests retain
the full deferred-barrier and receipt engine and deliberately swallow a local
budget error only after actual physical renewal and complete effect construction,
then require real transaction refusal and unchanged persisted state.
