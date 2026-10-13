# Native access-administration owner checkpoint

Reference: `453fca983222c6665a48775eef67c489c96b527e`, Python **3.14.6**.
This implements an owner slice for M10. It does **not** complete M10, enable
HTTP or Rust by default, retire Python, qualify a release, or resolve M6's
existing safety blockers. The public Python/CLI adapters and credential-file
writer remain unimplemented in this slice.

## Owners

`catabolic_store::access_admin` now owns three local administrative operations:

- `principal_put(&Store, profile, identifier)` validates the frozen string/length
  contract, performs the existing insert/profile-conflict transaction, and does
  not re-enable an existing disabled principal
- `grant_put(&mut embedding::Session, principal, definition, identifier)` checks
  the frozen fields, list/string bounds, booleans, revision dictionary and
  callback origins in the original order. It checks principal and saved-query
  existence before the grant-ID conflict, then atomically stores the canonical
  definition, increments the revision on every update, re-enables that grant,
  and writes `grant.put` audit evidence
- `issue(&Store, principal, grants, ttl)` preserves list/TTL validation, ordered
  enabled-grant checks, duplicate IDs, and the existing disabled-principal
  behavior. It generates a 12-byte hex identifier and 32-byte URL-safe unpadded
  suffix from OS entropy, computes SHA-256 of the complete secret, samples time
  after secret generation, then atomically stores only the digest, encoded
  grants and expiry alongside `token.issue` audit evidence

The JSON-valued arguments are a compatibility boundary for invalid-input order
and SQLite scalar binding, not another domain engine. Shared native encoding,
SHA-256, clock, SQLite, transaction/savepoint and selection owners are reused.
Detailed variants now preserve explicit administrative failure variants and
supply the existing AccessError codes/statuses from those variants. Flat owner
APIs, shared `Store::Error` and the schema remain unchanged. The initial
message-only `error_status` classifier was removed by the subsequent diagnostic
provenance correction; arbitrary SQLite text cannot confer business provenance. Exact Python exception
classes, non-JSON Python objects, and public signatures are not qualified here.

All operations borrow the existing native owner. No Python SQL implementation,
database-path reopening inside an administrative operation, secret-file write,
external account, transport, credential persistence or new default grant was
introduced.

## Sealed grant-query preflight

The frozen administrative path has a specific asymmetry: `Queries.select(...,
_http=True)` applies the restricted HTTP SQL views, but a GraphQL selection
without an Access object uses the local GraphQL context.

The new `GrantPreflight` capability can be constructed only inside
`access_admin`. `Session::select_access_grant` lends its exact Store to a
temporary evaluator that consumes this capability. The evaluator retains HTTP
SQL restrictions while allowing the frozen local GraphQL preflight. Its result
is checked for a complete `item_id` selection before any grant mutation.

The capability is not an HTTP JSON flag, is not stored in the public evaluation
session table, and cannot be carried into later calls. No raw callback context
or authority is supplied by a caller. Existing ordinary HTTP evaluators still
refuse GraphQL without authorization; tests repeat that refusal after a
successful administrative preflight and with lookalike JSON flags. Failed
GraphQL evaluation restores the same owner, its write capability, and any
uncommitted outer transaction.

Existing files changed for this path are limited to the `access_admin` module
export, the same-owner Session helper, and the evaluator's private capability
slot/constructor and guarded local-GraphQL branch.

## Shared callback-origin syntax correction

`notifications::callback_origin` previously accepted bracketed IPv6 only.
Frozen Python 3.14.6 also accepts a lowercase `v`, one or more ASCII hex digits,
a dot, and a nonempty tail as a bracketed IPvFuture host. The shared helper now
implements exactly that additional syntactic branch. Uppercase `V`, empty or
nonhex versions, empty tails, invalid ports, userinfo and controls remain
refused. Existing IPv6, scoped-IPv6 and origin-only path/query rules are retained.

This does not change DNS resolution, transport, redirects, approval scope,
credential handling, or outbound behavior. The tests make no network requests.
Actual transport acceptance of an IPvFuture host is not qualified here; a
transport refusal must not be worked around by changing this syntax check.

## Explicit scalar-token discrepancy and inherited behavior witness

Frozen `issue` permits SQLite-bindable scalar grant IDs. For example, numeric
`123` can match a grant whose SQLite TEXT ID is `"123"`, and the original numeric
value is retained in the token's JSON grant list. The new issuance owner matches
that persisted contract.

The existing native authenticator requires a JSON array of strings and refuses
such scalar-ID tokens. **That fail-closed behavior is intentionally unchanged**:
`access.rs`, including `Access::read_token` and its membership cache, is unchanged
by this checkpoint. Scalar-token issue-to-authentication parity remains open.

A minimal paired disposable witness establishes why reproducing the frozen
cache behavior is not an ordinary coercion fix:

1. Grant TEXT ID `"1"` allows `metadata:read` on synthetic `item-a`
2. Grant TEXT ID `"1.0"` allows `processing:request` on synthetic `item-b`
3. Frozen issuance accepts `[true, 1, 1.0, true]`, resolving both TEXT grant rows
4. Python's membership dictionary treats those scalar keys as equal. After the
   first expansion, all four grants reuse `item-a`, including the grant with the
   different action. Reversing the scalar order makes them all reuse `item-b`
5. Native authentication refuses both tokens with `unauthorized`; ordinary
   string IDs `['1', '1.0', '1']` preserve distinct memberships in both engines

This is an **inherited conditional authorization/membership confusion witness**,
not an intentional behavior to enable in native Access. The test expects the
native refusal and labels the difference explicitly. A separately proposed
scalar-authentication/cache diff was preserved locally for review and excluded
from the implementation. Resolving this compatibility/safety decision requires
separate scope and review; no complete access-parity claim is made.

## Selected verification

`access_admin_tests.rs` uses only disposable catalogs, synthetic principals and
grants, and deterministic fake fixture credentials. Its Python test driver
imports the actual frozen Git checkout, checks the exact commit and Python
version, and refuses dirty owner/query/validator sources and optimized Python.
The oracle runs separately from the candidate and owns its own temporary catalog.
It is not a Python business-logic replica.

Eight differential groups compare **260 state transitions**:

- 18 principal validation, Unicode, conflict and enablement transitions
- 80 grant definition, boundary and error-order transitions
- 84 shared-origin and grant-origin transitions, including accepted/rejected
  IPvFuture, IPv6, zones, ports, userinfo and controls
- 19 saved SQL/GraphQL selection transitions, including private SQL refusal,
  profile boundaries, missing/wrong/incomplete selections, malformed GraphQL,
  uncommitted visibility and rollback
- 26 grant-revision, enabled-state, token-list/TTL, digest and collision cases
- 13 nested/outer transaction transitions
- 8 read-only error-order transitions
- 12 audit-failure, atomic rollback and subsequent reuse transitions

Each compares returned values/errors and administrative AccessError statuses,
principal/grant/token rows, canonical stored JSON, revision/enabled/revoked
state, audit IDs/action/resource, epoch, and transaction state. Generated grant
UUIDs are checked for version/shape and normalized by creation order; SQLite's
wall-clock audit timestamp is not compared. Token entropy and time are injected
only into a private test seam, and only fake secrets appear in test memory.

An additional four-transition string-ID control and two scalar-collision
witness variants are separate from the parity groups. Native-only tests cover
entropy/clock order, exact secret shape/digest/no-cleartext-storage, rollback on
failed audit/deferred commit, disabled principal and revoked/expired/incorrect-digest
authentication gates, unchanged malformed/container-ID refusal,
same-owner GraphQL failure recovery, detached/closed refusal and reconnect,
and the sealed evaluator/HTTP refusal controls.

Run the selected tests with an explicit pinned reference:

```sh
CATABOLIC_PYTHON_ORACLE=/path/to/python3.14.6 \
CATABOLIC_PYTHON_REFERENCE=/path/to/frozen-checkout/src \
cargo test -p catabolic-store --lib access_admin::tests --locked --offline -- --include-ignored --nocapture
cargo test -p catabolic-store --lib 'access::' --locked --offline
```

The two Python-oracle tests are ignored in an ordinary Cargo run unless
explicitly selected with `--include-ignored` or `--ignored`. Native-only controls
run normally. These are selected development checks, not exhaustive schedules,
artifact-bound installed-wheel evidence, a full suite result, independent review,
platform qualification, or a release receipt. Fresh independent review must
cover the new owners, sealed preflight and shared origin-parser change.
