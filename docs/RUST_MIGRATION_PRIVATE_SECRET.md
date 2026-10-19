# Native private-credential adapter checkpoint

Frozen reference: `453fca983222c6665a48775eef67c489c96b527e`, Python **3.14.6**.
This is a narrow opt-in development checkpoint. It does not complete M10,
qualify a release, enable HTTP, switch the default engine, retire Python, or
resolve existing M6 blockers.

## Ownership and preserved effects

The preserved public row `api_cli.save_secret(store, filename, result)` now has
an adapter in `catabolic_native.api_cli`. Its Python function only forwards to
the caller's existing PyO3 owner. Pathlib conversion and CPython's default JSON
encoder are representation boundaries; native code owns every file effect and
the catalog transaction. There is no Python SQL/business implementation and no
reopening of the caller's database.

`catabolic_store::private_secret::save` implements this distinct protocol:

1. Exclusively create the new path with no-follow and creation mode `0600`
   (subject to the process umask)
2. Serialize the JSON in the original insertion order, then write a newline,
   flush and fsync; close before looking up the path identity
3. Register `(path, device, inode)` through the same Store's transaction, nesting
   under an existing transaction/savepoint
4. Only after registration, retrieve `token_id` and `expires` for the public
   summary; the token itself is never included in that summary

This does not call the Plex atomic-replacement writer. Existing output is never
replaced, and a newly created file is not removed after failure. Serialization
failure retains partial JSON without registration. Read-only/registration
failure retains a completed file. Outer rollback removes the registration but
leaves the file. Missing summary keys fail after registration, as in Python.

The PyO3 checkout returns the same Session before propagating errors. Python
serialization exceptions, including the exact callback-raised exception
object, are retained. Ordinary existing-file and missing-parent failures retain
Python's OSError subclasses, errno, message and pathname. Native SQLite failures
retain their native diagnostic provenance and cannot impersonate AccessError
from matching message text.

## Selected legacy CLI routes

The opt-in legacy dispatcher now forwards `api token-create` and
`api token-rotate` to the existing typed administration owner and this new file
owner. Issuance, hashing, expiry, audit and grant validation remain in
`access_admin`; no second implementation was added. Successful output includes
only token ID, expiry and credential-file path.

The frozen ordering is intentionally preserved rather than made atomic:

- Create: issue/commit token, save/register file, return summary
- Rotate: look up active token, issue/commit replacement, save/register file,
  revoke/audit old token, return summary
- An existing-output error leaves the newly issued token in the disposable
  catalog while leaving that file and the old token unchanged
- A final revoke failure leaves both tokens active and the new file registered

`api serve`, all defaults, schema/migrations and the earlier deliberate
scalar-token authentication refusal are unchanged.

## Selected evidence

All tests use disposable directories/catalogs and clearly synthetic principals,
grants and credentials. Public writer fixtures use explicit `FAKE-*` strings.
CLI-generated tokens exist only in these temporary fixture catalogs, never in
an actual service account. Nothing contacts a service or configures live access.

- Four Rust owner tests: exclusive bytes/mode/identity, partial serialization,
  caller-owned outer rollback, and registration failure/reuse
- Four source contracts: frozen public signature and identity-preserving
  forwarding, native ownership shape, fixed development-test scope, and strict CI gate binding
- Seventeen paired public-adapter groups on Python 3.14.6 and 3.11.15: exact
  bytes/order/escaping/newline (including tuple, huge integer, numeric mapping
  keys and nonfinite JSON values), relative/home paths, selected I/O errors,
  callback exception identity, missing summary keys, nested/outer rollback,
  registration failure/reuse, read-only effects and the lifecycle difference
  below. Catalog identity matches and fallback epoch are compared
- Four paired CLI fixtures (including the path correction below): create/rotate, exclusive-output failure ordering,
  unknown/invalid inputs, and final revoke-failure effects
- The existing administration source contracts and six CLI fixture families
  are rerun as regression checks; their historical exception/presentation gaps
  are unchanged

`scripts/native_private_secret_tests.py` pins the actual frozen checkout and
Python 3.14.6 oracle, runs each engine in its own process and disposable catalog,
and checks the supplied wheel bytes, extracted payload, script and frozen
source before/after. It **extracts** one helperless abi3 development wheel for
both interpreters; it installs no package. This is not installed-wheel evidence,
strict release qualification, a CI receipt, exhaustive schedules or independent
review. It must not be promoted into those claims.

A separate `scripts/native_private_secret_gate.py` is wired into the installed
CI lane. It requires the shared strict release-wheel verifier, one exact frozen
inventory row, four distinct required installed scenarios without skips,
seventeen paired groups, successful calls to the public row, oracle imports,
and final source/installed-artifact/runtime/oracle/script/inventory fences.
Canonical early receipt cleanup is covered by the shared receipt-boundary
suite. There is no development bypass in this gate. The strict gate has **not**
been executed here; it requires the next same-source integrated release build.

Example development invocation (use explicit locally built artifacts):

```sh
cargo test -p catabolic-store --lib private_secret::tests --locked --offline
CATABOLIC_LEGACY_BINARY=/path/to/catabolic-legacy-native \
  python -m unittest tests.test_rust_legacy_private_secret
python scripts/native_private_secret_tests.py \
  --wheel /path/to/helperless-development.whl \
  --reference-root /path/to/frozen-checkout \
  --reference-python /path/to/python3.14.6 \
  --candidate-python /path/to/python3.14.6 \
  --candidate-python /path/to/python3.11.15 \
  --report /path/to/new-development-report.json
```

## Explicit remaining gaps

- A closed/detached/foreign-thread native Session is admitted through the
  existing owner-lifetime guard before file effects. The paired closed-Store
  fixture records the difference: frozen Python creates the file and then raises
  AttributeError; native refuses first with CatabolicError. This checkpoint
  does not weaken the native lifetime boundary to reproduce that effect
- SQLite errors remain `CatabolicError` rather than Python's sqlite3 exception
  subclasses; the paired registration trigger normalizes only that documented
  class difference, retaining message and effects. CLI I/O failures similarly
  use the legacy native CatabolicError envelope, not exact Python OSError text
- CLI ordinary absolute/relative paths and `~/` with HOME are supported. Named
  `~user` expansion and the HOME-unset passwd fallback were closed in track D;
  see "Track D: complete pathlib expansion" below. The Python adapter itself
  uses Pathlib's normal expansion
- Non-UTF-8 filesystem paths and device/inode integer overflow fail without
  lossy catalog registration, but exact Python exception parity is unqualified.
  Embedded-NUL path diagnostics, custom encoder monkeypatches, callback reentry,
  all signal/write/flush/fsync/close/stat failure schedules and non-Linux
  platforms are not qualified by these tests
- Independent narrow review, executing the installed/strict artifact gate,
  and the existing broader parity gates remain open

## Independent ordinary-path correction

The first implementation passed its ordinary HOME control but incorrectly
expanded a raw `~//...` CLI argument: `PathBuf::join` treated the remaining
`/...` suffix as absolute and discarded HOME. The independent disposable
witness retained both possible destinations under its temporary root and
confirmed that frozen Pathlib wrote under HOME while the original native binary
wrote to the other fixture destination. No external path or live credential was
used. The original witness and artifact remain preserved as failing evidence.

The follow-on correction normalizes repeated separators and `.` components
before matching and expanding a complete leading `~` component. `..` is retained
and no path is canonicalized. The new paired CLI regression covers create and
rotate with ordinary `~/` controls, doubled separators, repeated internal
separators, and a leading `./`, checking the summary path, private identity,
bytes, mode and old-token revocation. Both possible paths are kept inside the
fixture. The original binary fails all six affected create/rotate subcases;
all sixteen reference/native control and correction subcases pass after repair.
The three existing normal/error CLI fixtures are retained. This correction
changes no transaction ordering or effect policy and does not qualify the
previously listed gaps.

## Track D: complete pathlib expansion (October 16)

`api token-create` and `token-rotate` now reproduce frozen
`Path(filename).expanduser().absolute()` for the `--output` path:

- POSIX parsing keeps an exact leading `//` root, drops empty and `.` parts and
  keeps `..` unresolved.
- `~` uses HOME when it is set, even when empty (an empty or `/` HOME yields
  `/`); when HOME is unset it falls back to the passwd home of the real user ID.
- `~user` uses the passwd entry for that user. A relative home is made absolute
  against the current directory.
- An unknown user, a failed passwd lookup, or a home that itself begins with
  `~` leaves the component unexpanded, which pathlib refuses with
  `RuntimeError: Could not determine home directory.` This happens after token
  issuance, as in Python: the token row persists, no file is written and a
  rotated token is not revoked.

Frozen Python leaves that `RuntimeError` unhandled, printing a traceback with exit
status 1. Native keeps exit status 1, the `RuntimeError` class and the message,
and uses the structured CLI error presentation (machine envelope on stdout, JSON
error on stderr, or `error: <message>`), with no traceback. This follows the
approved [mapping-runtime errors v2](NATIVE_RETIREMENT_DECISIONS_2026_10_16.md)
contract as precedent. It is a recorded difference from frozen Python,
**pending user confirmation**.

`test_rust_legacy_private_secret.py` adds two paired methods: named-user,
HOME-unset, empty and relative HOME expansion for create and rotate (all writes
stay inside the disposable fixture through `..` components), and both
unexpandable cases in all three output modes, comparing token, private-file and
audit state with frozen Python. Rust unit tests cover the parser and
unknown-user refusal.

Local macOS arm64 results at commit `ecce11d`: `test_rust_legacy_private_secret`
passes 6 methods and `test_rust_legacy_access_admin` passes 6 methods against
the frozen oracle. The store unit tests for admin, access and private-secret
pass 17, with 2 existing ignores.
