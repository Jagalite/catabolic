# Reproduce the Motion/Catabolic native dependency smoke

This is a qualification fixture, not a production dependency decision or full R0
acceptance. It prepares a separate Cargo workspace and does not edit Motion,
Catabolic's root manifests, or either production database.

## Initial result and scope

On 2026-10-14, Rust 1.99.0 on `x86_64-unknown-linux-gnu` successfully checked,
linked and ran this fixture against Motion commit
`1e5a24675b059337b34e98f86b225178798cc080` and a Catabolic working tree based on
`448cdc5b0958b320d2c7dfebd514624baa7a4fc3`. In-progress R0 host changes were
present in Catabolic during the linked build; this is not a receipt for a clean
final Catabolic commit.

The smoke:

- Opened and migrated a disposable Motion database: 62 migrations
- Initialized and opened a disposable Catabolic catalog: schema 32
- Verified both owners report SQLite 3.53.2 and the same SQLite source ID
- Verified math functions are enabled and soundex is absent
- Called the native Catabolic scan facade with no registered sources; it reported
  complete with no observations or helper work
- Closed and reopened both databases, preserving their migration/identity facts

The runtime does not call Python or HTTP. Motion's ordinary dependency graph
still includes HTTP libraries, since this fixture builds the actual Motion
library rather than a substitute dependency list.

The historical receipt does not prove a real scan, helper registration/digest
handling, pending work, cancellation, bounded drain or stable host errors. The
current `src/main.rs` extends the initial source with HostSession identity,
bounded saved-query reads, batched item mutation, cancellation/admission checks,
drain and reopen. It also has an opt-in real-helper path. Those added assertions
are not part of `initial-receipt.json`. They subsequently passed in the expanded
frozen-source run described below. Neither fixture claims all Motion regressions,
all Catabolic features, all host error outcomes or other operating systems.

## Prepare and run

From the Catabolic checkout, select a fresh output directory outside both source
checkouts:

```sh
python3 tests/native-host/motion-dependency-probe/prepare.py \
  --motion-root ../motion \
  --catabolic-root . \
  --output /tmp/catabolic-motion-dependency-probe
```

The script requires the documented clean Motion commit, records the current
Catabolic commit/dirty status, downloads the official SQLx SQLite crate, verifies
its archive checksum, applies exactly one manifest replacement, and copies this
fixture's source and lockfile. It only prepares files; successful preparation is
not a build result.

Then run with an installed Rust 1.99.0:

```sh
cd /tmp/catabolic-motion-dependency-probe
export LIBSQLITE3_FLAGS='-DSQLITE_ENABLE_MATH_FUNCTIONS -USQLITE_SOUNDEX'
export CARGO_TARGET_DIR="$PWD/target"
cargo +1.99.0 run --locked
cargo +1.99.0 tree --locked -i libsqlite3-sys
```

For the optional real native helper lifecycle, provide both an explicit helper
path and its independently recorded SHA-256. The host verifies the file digest
at registration; PATH lookup is not used. Use a helper built from the same final
Catabolic source being qualified:

```sh
export CATABOLIC_HOST_TEST_HELPER=/absolute/path/to/catabolic-native
export CATABOLIC_HOST_TEST_HELPER_SHA256=YOUR_VERIFIED_64_CHARACTER_LOWERCASE_SHA256
cargo +1.99.0 run --locked
```

This creates 400 disposable source files, verifies missing-helper refusal,
starts work with an owned permit, cancels that demand, drains/releases its permit,
reopens, completes a new scan, drains and reopens again. Each phase has a 20-second
fixture deadline. The receipt explicitly says `not_requested` when the helper
path is omitted; a skipped helper path is not helper qualification.

If Rust uses custom homes, set `CARGO_HOME` and `RUSTUP_HOME` first. The initial
run used `/workspace/shared/rust/cargo` and `/workspace/shared/rust/rustup`.
The required SQLite flags mirror Catabolic's `.cargo/config.toml`; Cargo does not
inherit a dependency's configuration into an unrelated host workspace.

Keep `--locked`. A later source dependency change may make this historical lock
stale. In that case review the intended graph and qualify a new lock/receipt;
do not silently describe it as the original result.

## Expanded frozen-source result

The expanded executable, including the real-helper path, passed twice on
2026-10-14. The second run used `--locked --offline` with exact before/after
snapshots. See `qualified-receipt.json` and `frozen-evidence.sha256`.

- Catabolic production source manifest:
  `9c670fd16d98fa1747dbaa461c54d407faa4747428622d3e039eef2932671f6c`
- All 700 files in that source scope matched before/after
- Executed fixture manifest (59 Cargo/source/vendor files):
  `577cd269ed0889d4d76f8e53b4e45ea3b4af67d80e17bfcec7b17d99a7494514`
- Historical authored fixture asset manifest from these earlier runs:
  `f8acfa4f269d39a352ecfbb574dec95a96babfe3e0cbd21dfc581051aa80bff6`
- Native helper SHA-256:
  `6830631b4274c9fb651c5c7d52072ac3cbac17e3200e0f8e3504143f36f0a2fa`
- Fixture, helper and linked host binary were unchanged across the frozen rerun

The supported HostSession path passed identity, bounded saved-query reads,
batched item writes, cancelled-read refusal, draining-admission refusal and
read-only reopen. The real helper path passed missing-helper refusal, retained
permit while work was active, demand cancellation, drain/reopen, a subsequent
complete scan, and exactly two permit releases. Motion's pool remained live
while native Catabolic work ran on a blocking worker.

Detailed logs/manifests remain outside source at
`/workspace/shared/motion-r0-frozen-probe/`; both passing runs are retained.
No failed expanded run was discarded. The native dependency patch remains an
isolated fixture decision, not an approved or applied production Motion change.

A read-only comparison also verified current Motion origin/main
`dcabb7bb85284df3cf7694b1328c20b68ced6c49` differs from the executed pinned
revision only by adding `MOTION_CLI_TUI_SHARED_OPERATIONS_PLAN.md` (131 lines).
There are no manifest, lockfile, toolchain or production owner-source changes.
That doc keeps domain authority in the Rust server/core and plans shared Go
client workflows. No current-main dependency alignment was already present.
The executed checkout remains the pinned revision; this is a source-diff
comparison, not a claim that another checkout was built.

## Exact external patch and provenance

Archive:
https://static.crates.io/crates/sqlx-sqlite/sqlx-sqlite-0.8.6.crate

SHA-256:
`c2d12fe70b2c1b4401038055f90f151b78208de1f9f89a7dbfd41587a10c3eea`

This matches Motion's pinned Cargo.lock and the official crates.io index.
The only changed upstream file is normalized `sqlx-sqlite-0.8.6/Cargo.toml`:

```diff
 [dependencies.libsqlite3-sys]
-version = "0.30.1"
+version = "=0.38.2"
```

See `sqlx-sqlite-libsqlite3-sys.patch`. The initial run compared all 56 files
against the official archive and found only this manifest changed. SQLx Rust
sources and the other SQLx package versions remain 0.8.6. No Catabolic dependency
is downgraded. The fixture-root `[patch.crates-io]` is essential: Cargo ignores a
patch declared only in a dependency's manifest.

No vendor bulk is checked in here. The prepared vendor copy is approximately
500 KiB. A production owner would need to choose a reviewed vendor copy or an
immutable, maintained fork and retain the upstream license notices. The
production decision is pending; see `DEPENDENCY_OPTIONS.md`.

## Lockfile boundary

The lockfile is the successful independent fixture graph, not a proposed
replacement for Motion's production lockfile. Compared with Motion's baseline,
Catabolic's exact pins select chrono 0.4.42 rather than 0.4.45, libc 0.2.183 rather
than 0.2.190, serde_json 1.0.151 rather than 1.0.154, and uuid 1.27.0 rather than
1.28.0. The fixture also picked a few incidental newer transitive patches,
including tokio 1.53.3 instead of 1.53.2. Start any production lock update from
Motion's existing lock and review/minimize the complete diff.

## Evidence

`initial-receipt.json` preserves the initial runtime summary. Original check,
run, unaligned-resolution and binary-inspection logs remain outside source at
`/workspace/shared/motion-r0-probe/`. Their hashes are in
`initial-evidence.sha256` for this execution environment; their absence on a
fresh machine is expected. Reproduce the test rather than relying on local logs.

The unmodified graph failed resolution with exit 101: SQLx's libsqlite3-sys
^0.30.1 conflicts with rusqlite's 0.38.x because both declare `links=sqlite3`.
The aligned graph has one libsqlite3-sys package version, 0.38.2, shared by
SQLx and rusqlite. Cargo's reverse tree prints it in both host/proc-macro and
application contexts. The application binary has one definition each of
sqlite3_libversion/sqlite3_open_v2 and no dynamic libsqlite3 dependency.

## Patch-format amendment

The patch was subsequently rewritten with the nonblank-context hunk
`@@ -81,3 +81,3 @@` so normal staged whitespace checks pass. Applying it with
zero fuzz produces the identical upstream manifest change. Production sources
and the Rust/Python fixture code did not change.

The current authored asset manifest is
`fc595f2879fffce95564287044f952918d2be7eee7ebb53882f5c3b94cd283c0`, recorded in
`current-fixture-asset-manifest.json`. All 59 prepared/executed files retain the
same manifest hash `577cd269ed0889d4d76f8e53b4e45ea3b4af67d80e17bfcec7b17d99a7494514`.
Fresh preparation and two helper-enabled runs passed again; the second retained
matching before/after source, fixture, helper and binary snapshots.

`qualified-receipt.json` records this current artifact. The prior receipt and
asset identity remain in `qualified-receipt-before-patch-format.json` and
`historical-fixture-asset-manifest.json`; the earlier evidence hash files are
unchanged. Latest external logs/snapshots are under
`/workspace/shared/motion-r0-patch-format-probe/`, with hashes recorded in
`patch-format-evidence.sha256`.
