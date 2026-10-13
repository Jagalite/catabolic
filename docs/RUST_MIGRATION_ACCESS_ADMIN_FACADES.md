# Local access-administration facade checkpoint

Frozen reference: `453fca983222c6665a48775eef67c489c96b527e`, Python 3.14.6.

Later selected credential-output implementation: see
[the private-credential adapter checkpoint](RUST_MIGRATION_PRIVATE_SECRET.md).
The writer-refusal statements below describe this earlier checkpoint.
This is a selected opt-in adapter checkpoint, not full M10/access parity,
HTTP enablement, Python retirement, a release receipt, or resolution of M6.

## Ownership

The preserved Python signatures are available in `catabolic_native.access`:

- `principal_put(store, profile, identifier)`
- `grant_put(store, principal, definition, identifier=None)`
- `issue(store, principal, grants, ttl=3600)`

Each uses the caller's existing native Session/Store. The PyO3 `public_access`
module forwards to the three production `access_admin` owners. Python contains
no SQL, validation policy, entropy, digest, selection engine, database reopening,
or credential-file writer. The existing public `AccessError` constructor and
`content_access.revision_of` helper are unchanged. The owner returns an explicit
`Failure::Access(AccessFailure)` only at business-validation decision sites;
Python translates that variant into public `AccessError(code,status)`. SQLite,
selection and lifecycle errors travel as `Failure::Native` and remain ordinary
native `CatabolicError`, even when their message exactly matches a business code.
The existing flat Store APIs delegate to detailed variants and keep their public
`Result` signatures. A crate-private generic transaction seam shares the existing
begin/finish/unwind mechanism; begin, commit and rollback-cleanup failures always
remain native. No error-message prefix or SQL-text classification is used.
The returned successful grant retains the caller's original definition object.

A typed PyO3 argument boundary preserves distinctions that ordinary JSON
serialization would erase: tuple versus list fields, non-string revision-map
keys, exact integer TTL versus bool/float/int subclasses, and invalid scalar
values in string lists. Bounds, allowed fields, error order, database decisions
and transaction ownership remain in the production Rust owners.

The same-owner checkout preserves lexical transaction thread checks, nested and
outer rollback, read-only ordering, detach/reconnect and closed-state refusal.
It polls Python signals on the owning thread and restores the exact exception
after the owner is returned. The sealed grant preflight restores an already
present embedding interrupt handler after bounded SQL/GraphQL evaluation removes
its own deadline handler, so the subsequent grant audit is still interruptible.
A cleanup-only Session helper removes that callback even when SQLite aborts and
poisons an active scope. Neither helper grants authority or revives an owner.

Grant preflight uses the existing sealed admin-only capability: local GraphQL
and restricted HTTP SQL. No general privilege flag, callback context or HTTP
entry point was added. Ordinary HTTP-shaped query calls still refuse the same
unauthorized GraphQL selection before and after an administrative preflight.

## Scalar-token divergence remains deliberate

Issuance preserves SQLite-bindable scalar grant IDs, including
`[true, 1, 1.0, true]`, and stores their original JSON values. Native
`access::Access` is unchanged and intentionally refuses these issued tokens.
Frozen Python's bool/int/float membership-cache collisions can mix distinct
TEXT grant IDs and memberships. This behavior must not be reproduced as an
ordinary coercion correction. The existing paired collision witness and
string-ID controls are retained, and the facade paired fixture verifies scalar
issuance/persistence only. No scalar issue-to-authentication parity is claimed.

## Local CLI slice

The legacy dispatcher forwards nine leaves through a separate transport module
to `catabolic_store::admin_cli`:

- `api principal-put`, `grant-put`
- `api token-revoke`, `tokens`
- `api principal-disable`, `grant-disable`, `operation-disable`
- `api source-expose`, `operation-approve`

The first two reuse the reviewed administration owners. Other mutations belong
to the new native owner module; the CLI contains only bounded file loading,
owner construction, and forwarding. Existing binding and recipe owners validate
source exposure and operation approval. Token listing filters by profile and
omits digests/secrets; revoke keeps its audit transaction, including missing IDs.
Disable semantics, including existing global identifier scope, are preserved.
The grant file remains bounded to 262144 bytes. The CLI carries the same tagged result through a dedicated dispatch boundary;
only the explicit business variant uses the existing AccessError envelope.
Token listing preserves column order.

`token-create` and `token-rotate` are still unavailable. `save_secret` is not
implemented; the CLI never prints or persists a new token. `api serve` refusal,
`api worker`, M6 restrictions and all default-engine decisions are unchanged.

## Selected checks and limitations

`tests/test_native_access_admin_contract.py` pins exactly three frozen inventory
rows, public signatures, thin typed forwarding, fixed scenario/group counts,
strict-artifact gate use, and optimized-Python refusal.

`scripts/native_access_admin_tests.py` requires nine distinct installed
scenarios without skips and thirty exact paired groups. It verifies actual
installed wheel payloads, source snapshot, frozen source/import provenance,
Python 3.14.6 oracle identity, successful calls to all three rows, and unchanged
identities after execution. Strict mode requires the shared release-wheel gate;
`--dev` explicitly selects helperless development evidence and cannot count as
strict/release qualification. Reports contain normalized token shape, digest
and expiry checks, never raw secrets. All catalogs, principals and credentials
are disposable fixtures; no live account, transport or credential file is used.

`tests/test_rust_legacy_access_admin.py` has six fixed fixture families covering
success/error envelopes, profile boundaries, bounded grant files, revision and
disable behavior, recipe/destination errors, audit rollback, and continued
credential-writer/serve refusal. Audit rollback uses schema-preserving
`api_audit` AUTOINCREMENT exhaustion: token revoke and grant put reach the audit
write, return `database or disk is full`, preserve all captured tables and
sequences, and succeed after fixture repair. A trigger installed before CLI
launch would instead fail schema admission and is not a mutation-rollback witness.
A Rust CLI boundary regression installs triggers after opening the owner and
checks principal/grant/revoke collision diagnostics across text, JSON and machine
envelopes, along with real business-error controls. Exact output is compared except the explicitly
listed inherited diagnostic differences below; state and refusal are checked
for those differences.

Remaining gaps are explicit:

- Non-JSON scalar objects, nonfinite/out-of-range numbers, surrogates,
  tuple-valued query IDs and pathological container subclasses are outside the
  selected transport contract. Unsupported inputs are refused before owner
  admission. Their exact frozen exception categories and mixed-invalid-input
  precedence are not claimed
- Native Store errors are string-only. SQLite failures have the preserved
  message/state behavior but remain `CatabolicError` rather than Python's
  `sqlite3.IntegrityError` or other SQLite exception classes
- Malformed grant-file JSON uses the existing native `invalid JSON` diagnostic;
  Python's `JSONDecodeError` location/text is not emulated. Both reject without
  mutation. Native grant-return definition JSON uses canonical map order rather
  than the input object's insertion order; semantic output/state match. The
  fixture records this presentation difference. Non-ASCII boundary/BOM and
  nonfinite JSON transport are unqualified
- CLI interruption schedules, generated operation UUID presentation, all Python
  object coercions, all production producer suites and platforms remain
  unqualified. The Python signal fixture is selected, not exhaustive
- Credential-file persistence, scalar-token authentication compatibility and
  existing M6 safety blockers still require separate scope and review

The source and artifact checks in this checkpoint must not be used to mark
these public facades complete or the CLI leaves fully parity-qualified.

## Development verification receipt

Selected checks passed locally on the checkpoint source:

- Four new source contracts and three existing AccessError/revision contracts
- Twelve existing owner tests, including the 260-transition frozen differential,
  scalar-cache witness, string-ID controls and lifecycle/preflight controls
- Nine installed scenarios / thirty paired groups / three preserved rows on
  Python 3.14.6 and 3.11.15, using the same helperless abi3 wheel
- Six CLI fixture families, with the diagnostic/presentation gaps above explicit
- Selected Python Ruff checks and Rust formatting

The strict release-wheel mode is implemented but was not run. Neither a full
suite, platform matrix nor independent review is claimed. The first reinstall
attempt was refused because pip's default index/version behavior could access
pypi.org. A distinct bounded install using `--isolated`,
`--disable-pip-version-check`, `--no-index`, `--no-deps` and the existing local
wheel succeeded without requesting network access. One development report
attempt correctly failed its final source fence after a concurrent lint edit;
the successful reports were rerun against the stable test script.

## Diagnostic provenance correction

Independent review found that the original message-only classifier let an
already-open Store's SQLite trigger raise `invalid_grant` or `unknown_principal`
and impersonate public AccessError (400 or 404). State rollback was correct, but
the exception category and metadata were not. The old installed wheel fails
36 selected collisions: all twelve business-code messages across principal
insert, grant audit and token-issue audit. Ordinary trigger text is a control.

The corrected frozen/installed fixtures check all 39 cases, exact error category
(`IntegrityError` frozen, `CatabolicError` native), absence of AccessError fields,
nested rollback and same-owner reuse after each failure. This is included within
the existing thirty paired groups; no SQLite class parity is claimed. Existing
twelve owner tests and their 260-transition differential remain, with one new
transaction provenance test covering business rollback, unwind, deferred commit,
cleanup-error poisoning, read-only refusal and reuse. Selected exact Python
signal identity/rollback/reuse checks are retained.

This correction is a narrow development checkpoint. It leaves the native
non-string-token authentication refusal, absent credential-file writer,
`token-create`/`token-rotate` refusal and all release/default-engine restrictions
unchanged. Full production schedules, SQLite exception subclasses, platform
qualification and release artifact gates remain outside this evidence.
