# Native Plex PIN login checkpoint

This opt-in runtime checkpoint implements the independent login owner from
`src/catabolic/plex_login.py` at frozen commit
`453fca983222c6665a48775eef67c489c96b527e`. Python remains the default.

## Ownership and boundary

`crates/catabolic-runtime/src/plex_login.rs` owns private credential-directory
validation, bounded private JSON reads, durable atomic writes, stable locked
client identity, the fixed-origin Plex HTTP exchange, PIN start/poll, account
validation, expiry, resume, and repeated completion. The login does not open a
catalog. A successful completion returns the credential filename; it never
returns the token. PIN state is replaced only after the account validation
request succeeds. Private helper methods are not CLI operations.

The opt-in companion operations are `operation plex-login start` and
`operation plex-login complete`. Their JSON request accepts `credential_dir`
(or `storage`); completion accepts `identifier` (or `login_id`). The storage
default is `~/.config/catabolic/credentials`. The runtime API dispatches these
operations before any Store is opened.

This is not the frozen legacy `consumer plex-login` parser or its `--machine`
envelope/exit renderer. That parser remains a distinct public-surface gate,
as does the full installed Python login facade. The native test retaining the
frozen machine-flow scenario verifies the companion JSON result, redaction,
and absence of a database; it does not qualify the missing legacy envelope.

## Protocol and lifecycle

- Stable UUID client identity is published under a directory lock, released
  before network I/O; concurrent starts retain the same client ID
- PIN start uses POST `/api/v2/pins?strong=true`; completion polls the saved PIN
  and code, then validates its synthetic test token via `/api/v2/user`
- Authorization URLs preserve the frozen fragment query and form encoding
- Pending, expired, and completed results preserve the frozen JSON shape
- Repeated completion reads the private credential and performs no HTTP calls
- Files are owner-only, regular, singly linked and opened without following
  the final symlink. Atomic writes use a private temporary file, file sync,
  rename and directory sync; failed rename removes the temporary file
- HTTP has a 15-second global bound and a 65,536-byte response bound, disables
  ambient proxies and redirects, and preserves structured redacted errors
- HTTP/1.0 compatibility retains `Connection: close` and zero idle connections
- Scoped executable signal handlers set a cancellation atomic directly and
  restore the previous SIGINT/SIGTERM handlers afterward. Python keeps its own
  handlers and polls on the operation owner. Both check signals synchronously
  after file sync and before atomic credential publication
- Interrupted pre-commit publication cleans the private temporary file and
  leaves the PIN resumable. Interruption after the rename retains the committed
  credential, which repeated completion consumes without another HTTP request
- HTTP alone uses a joined bounded worker, with no access to publication or the
  next request. Cancellation cannot leave a detached writer or request alive
- Poll ID comparison preserves Python int/float equality without rounding
  distinct arbitrary-sized integers. Bounded JSON decoding accepts UTF-8 BOM,
  UTF-16, and UTF-32, including byte-order-detected forms
- A whole-request deadline failure is `plex_login_failed`, matching the frozen
  timeout category; explicit HTTP status failures retain their structured codes

## Evidence and qualification scope

The initial October 11 checkpoint passed 16 native cases and 10 separate
frozen tests. Scoped review then found publication-interruption, numeric-ID,
JSON encoding, and timeout-category differences. The correction is qualified
separately below; the initial receipt does not establish corrected behavior.

The corrected October 11 run passed 20 native cases and 10 separate frozen
tests on the frozen Python 3.14.6 interpreter, plus nine focused Rust tests.
A newly installed dev wheel passed both adjacent Plex-import signal tests and
all six workflow lifecycle tests. Runtime all-target Clippy with warnings
denied, focused Ruff, formatting, and diff checks passed. These remain dev
artifact receipts, not a release wheel or full platform qualification.

`tests/test_rust_plex_login.py` covers all ten frozen scenario names, then adds
ten native-only boundary tests: cancellation/resume, concurrent identity and
network lock release, response/private-file limits, redirect/fixture gating,
hard-linked/nonregular file rejection, the real 15-second request timeout,
paired numeric/encoding compatibility, pre/post-commit CLI/Python interruption,
identity-lock cancellation, and cancellation joined through the request bound.
The native file-credential case creates a disposable catalog and uses the native
consumer owner for connection validation and a section scan. It checks that
only the file reference enters SQLite. This catalog is deliberately separate
from the login operation itself.

Nine focused Rust unit tests cover direct private helper consumption, the
Unicode-character storage bound, failed-rename cleanup, retry delay and integer
bounds, repeated-error file-descriptor lifetimes, exact numeric equality,
pre-cancelled admission, timeout error category, and signal-handler restoration.

The publication regression uses a local test-only fsync interposer with no
post-signal delay. It covers CLI/Python, success/EINTR, and pre/post-rename
interruption, checks original Python exception identity, and verifies resumable
or already-committed outcomes with no temporary token files. Separate review
stress evidence covers 20 zero-delay CLI attempts and 20 Python attempts.

### Explicit remaining cancellation gap

Prompt cancellation of an in-flight HTTP request is not implemented. The owner
joins that request until its existing 15-second global bound, while polling
signals and keeping private-directory locks released. This is slower than the
frozen Python process cancellation. The cancellation test was deliberately
changed from immediate process death to joined cleanup; it is not equivalent
latency evidence. A second test holds the request through its deadline, requires
bounded exit 130, no later request, no database creation, no unpublished private
temporary file, and successful retry. A cancellable transport remains a concrete
implementation and acceptance gate.

All HTTP qualification is fake-service/loopback only. Each native subprocess
sets `CATABOLIC_PLEX_LOGIN_FIXTURE=1` and supplies a `fixture_endpoint` which must
be an explicit `http://` numeric loopback socket address. Invalid fixture
configuration fails closed; there is no fallback to a real Plex request.
Reference requests are intercepted. No real login, real token, external
account access, or live consumer acceptance was performed.

Reproduction from this isolated worktree:

```sh
source ../catabolic/.local-tests/rust-migration/env.sh
export CARGO_TARGET_DIR="$PWD/target"
cargo build -p catabolic-cli --bin catabolic-native --offline
cargo build -p catabolic-python --lib --offline
cargo test -p catabolic-runtime plex_login --offline -- --test-threads=1
PYTHONPATH="/tmp/catabolic-embedding-frozen-453fca9/src:$PWD" \
  /workspace/scratch/9c9a5a8c3955/.local-tools/catabolic-reference-3.14.6/bin/python \
  -m unittest -v tests.test_plex_login tests.test_rust_plex_login
```

The worktree copies of the frozen source and reference test module were verified
byte-for-byte against the frozen checkout before the run:

- `src/catabolic/plex_login.py`: SHA256
  `f0521ee82babdb9827983a82dddf4597c377dc050bf723c414faabe329661916`
- `tests/test_plex_login.py`: SHA256
  `0cf156bf294a80db6a5f5a3429f217ced5ea49a101ec19f8ded95d8d3b4e56ff`

This focused implementation evidence does not close the independent definition
or filesystem review gates, general legacy CLI compatibility, platform release
qualification, or real-service acceptance. Those review lanes were not run or
substituted here.
