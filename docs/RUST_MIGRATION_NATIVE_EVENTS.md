# Native source-event adapter checkpoint

This implements the remaining ordinary M5 filesystem-notification intake, using
only disposable source directories and catalogs. Python remains the default.
It does not complete the broader filesystem/source-access/observation/remount/
watcher safety review, whose existing blocked status is unchanged. It does not
qualify M6 publication, real mount/unmount behavior or native default switching.

## Production path

- `catabolic-runtime::source_events::MonitorSession` subscribes to real kernel
  notifications. Linux uses nonblocking, close-on-exec inotify; the macOS cfg
  implementation uses close-on-exec kqueue vnode subscriptions to directories
  and regular files. The macOS branch has not been compiled or run here
- `supervise_with_poll` retains one session across foreground cycles and pulses
  it during the existing detached watcher child waits. The legacy opt-in
  `supervise --native [--once]` and operation `watcher tick` with `native: true`
  reach this path. No Python business-logic subprocess provides event handling
- Source hints flow into the existing atomic observation dirty generation and
  watcher pending/debounce fields, then the existing production observation and
  watcher claim reducers. Events do not insert files, publish inventory, mark
  absent paths or run output effects themselves
- The shared Store source-event owner snapshots the profile, database identity,
  source bindings, generated roots, trust settings/revisions, and scan policies.
  Invalidations revalidate the exact snapshot inside their transaction. Changed
  configuration reestablishes subscriptions and invalidates the current sources
- Output and generated roots are excluded, together with live source-policy
  exclusions. Generated sources continue to use their existing completion
  events. Paths are not retained in the pending queue; it has one versioned
  entry per ordinary bound source, capped at 1,000 sources
- Subscription setup is descriptor-relative and does not read source bytes.
  Each setup is bounded by 16,384 registrations, 200,000 inspected entries and a
  five-second cooperative deadline. Linux kernel intake is capped at 4,096
  events per batch. These are cooperative bounds between syscalls, not hard
  deadlines for a stalled mounted filesystem
- Overflow, truncated intake, lost subscriptions, disconnected roots and failed
  registration invalidate conservatively and keep monitoring incomplete until
  reconnection. Kernel events never prove complete inventory. New subtree and
  rename notifications rebuild subscriptions before a reconciliation generation
- Busy writers retain the coalesced batch for retry. A versioned acknowledgment
  clears only the committed batch, never a newer hint. No Store writer session
  is held across subscription setup, kernel waits or foreground sleep
- Cancellation closes this monitor/waiter and reaps its watcher child scope; it
  does not cancel shared durable observation demand. A subsequent startup
  invalidates the full ordinary source set again
- Default legacy once-report fields remain `runs`, `pending`, `complete`. The
  internal `include_monitoring: true` option adds bounded hint-intake diagnostics.
  Monitoring loss or outstanding invalidations prevent a false complete report

Periodic authoritative scans remain necessary, including for filesystems that
may not deliver all remote changes. There is no claim of event-stream completeness
or cross-platform mounted-storage acceptance. Unsupported operating systems
receive an explicit unsupported error rather than a polling substitution.

## Qualification

The focused Rust tests use actual Linux kernel events for recursive modify,
create, rename, deletion, newly created subtree intake, a 6,000-file event storm,
root disconnect/reconnect, dynamic exclusions, writer contention, writer progress
while waiting, cancellation and generation/debounce behavior. Additional cases
exercise binding replacement and profile/catalog fences. The pending-hint unit
check verifies stale acknowledgment and generation exhaustion.

The Python subprocess cases use the frozen Python implementation only to create
fixtures and read durable evidence. Native executable processes cover `--once`,
continuous foreground intake, source changes during an actively delayed native
scan, fresh generation completion, newly registered subtrees and cancellation.

Reproduce with the pinned reference environment and copied binaries, avoiding
concurrent Cargo relinking of the executable under test:

```sh
source ../catabolic/.local-tests/rust-migration/env.sh
export CARGO_TARGET_DIR="$PWD/target"
cargo test -p catabolic-runtime --test source_events --offline
cargo test -p catabolic-runtime source_events::tests --offline --lib
cargo build -p catabolic-cli --offline
mkdir -p .local-tests/rust-migration/m5-native-events
cp target/debug/catabolic{,-legacy}-native .local-tests/rust-migration/m5-native-events/
CATABOLIC_NATIVE_BINARY="$PWD/.local-tests/rust-migration/m5-native-events/catabolic-native" \
CATABOLIC_LEGACY_BINARY="$PWD/.local-tests/rust-migration/m5-native-events/catabolic-legacy-native" \
PYTHONPATH="/tmp/catabolic-embedding-frozen-453fca9/src:$PWD" \
../.local-tools/catabolic-reference-3.14.6/bin/python -W ignore::ResourceWarning \
  -m unittest tests.test_rust_native_events
```

All existing destructive-leaf and projection-publication gates are unchanged.
This event adapter neither supplies a disposable-fixture permission itself nor
relaxes the native release gates. Broader independent reviews and remaining
M5/M6 qualification remain separate, unresolved gates.

### Final local checkpoint (2026-10-11)

- 11/11 real-kernel/source-fence Rust integration tests passed
- 1/1 pending-generation/acknowledgment unit test passed
- 18/18 Python subprocess tests passed: four new native-event supervisor cases,
  seven retained watcher differential cases and seven watcher-runtime cases
- Strict runtime all-target Clippy (`-D warnings`), owned Rust formatting and
  the new Python module's Ruff checks passed
- Only `x86_64-unknown-linux-gnu` is installed in this environment; macOS code
  was not compiled or executed and is not platform-qualified

Logs and copied executables are under
`.local-tests/rust-migration/m5-native-events/`. Tested debug executable SHA-256:

- `catabolic-native`:
  `789bf4bed609dd2648960c013413ad4d501bcacc9c5c9de64bd06bf4dfddb1c7`
- `catabolic-legacy-native`:
  `3a5d9b34534714af6cce1b2e77117e117e6814b423a88e80f4ae7cadc4b2874d`

These results qualify the new adapter's bounded Linux disposable-fixture path,
not a release artifact, real-mounted filesystem behavior, or any previously
blocked independent safety review.
