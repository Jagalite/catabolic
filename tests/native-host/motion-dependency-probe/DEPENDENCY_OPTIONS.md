# Native SQLite alignment: decision options

Status: recommendation only; production Motion remains unchanged. Evidence was
checked on 2026-10-14. The current Motion origin/main
`dcabb7bb85284df3cf7694b1328c20b68ced6c49` was compared with the tested pinned
revision: only a CLI/TUI planning document was added; dependency/toolchain and
production owner files are unchanged. This is a narrow dependency assessment, not the broader
content, definition, filesystem or HTTP security review.

## Recommendation

Retain Catabolic's qualified `rusqlite =0.40.2` / `libsqlite3-sys 0.38.2` and use a
Motion/consuming-host-owned compatibility patch to SQLx SQLite 0.8.6. This is the
smallest demonstrated native path. It is locally qualified on Linux, not an
unmodified upstream-supported dependency combination and not full production
acceptance. If an unpatched released combination is mandatory, wait for or
contribute an upstream SQLx release accepting 0.38.x, then qualify that release.

## Option A: maintain the tested SQLx adapter patch

The official SQLx SQLite 0.8.6 manifest requires libsqlite3-sys ^0.30.1. Its Rust
sources compile and this linked smoke runs after that single dependency
requirement becomes =0.38.2. No C or Rust source changes were needed in the
adapter or in Motion.

The consuming workspace must own the patch; putting it in Catabolic's library
manifest cannot force a downstream host to use it. Maintenance is a one-line
local delta, but the owner must retain/audit a 56-file (~500 KiB) upstream source
copy, preserve licenses/provenance, follow SQLx and SQLite fixes, and rerun the
qualification when either side changes. A Git fork pinned to a verified immutable
commit avoids vendor bulk but creates availability and fork-maintenance
obligations. No fork has been created or authorized by this fixture.

### Proposed production file changes, not applied

`motion/rust-toolchain.toml`:

```diff
-channel = "1.98.0"
+channel = "1.99.0"
```

`motion/Cargo.toml`, for a host that actually embeds Catabolic:

```diff
-rust-version = "1.98"
+rust-version = "1.99"
@@
-sqlx = { version = "0.8.6", default-features = false, features = ["runtime-tokio", "sqlite", "macros", "migrate", "json"] }
+sqlx = { version = "=0.8.6", default-features = false, features = ["runtime-tokio", "sqlite", "macros", "migrate", "json"] }
+catabolic-runtime = { path = "../catabolic/crates/catabolic-runtime" }
@@
+[patch.crates-io]
+sqlx-sqlite = { path = "vendor/sqlx-sqlite-0.8.6" }
```

The sibling path is a concrete local example. Production may choose a separately
verified immutable Catabolic source, without changing its existing dependency
pins. UI-only Motion crates need not have their MSRV raised merely because the
server/host minimum changes.

Add the verified vendor copy at
`motion/vendor/sqlx-sqlite-0.8.6/`, preserving its original files and license
notices, and apply `sqlx-sqlite-libsqlite3-sys.patch`. Alternatively replace the
path patch with a verified immutable Git revision of the reviewed fork.

New `motion/.cargo/config.toml`, or equivalent explicit build/CI environment:

```toml
[env]
LIBSQLITE3_FLAGS = "-DSQLITE_ENABLE_MATH_FUNCTIONS -USQLITE_SOUNDEX"
```

Regenerate/review `motion/Cargo.lock` starting from its existing lock, and prove
there is one libsqlite3-sys version. Do not copy this independent fixture's
lockfile over Motion's lock. Catabolic's exact shared pins also force the
chrono/libc/serde_json/uuid version changes listed in the README. These changes
need regression and advisory review in addition to the SQLite patch. This
assessment has not declared every shared-pin change security-neutral.

Actual host API wiring, worker ownership and application cutover are separate
implementation changes; the dependency diff does not implement them.

## Option B: use a released SQLx upgrade without a patch

Not currently sufficient. The official crates.io index lists SQLx SQLite 0.9.0
as the latest released driver. Its libsqlite3-sys constraint is
`>=0.30.1, <0.38.0`, excluding Catabolic's 0.38.2. 0.8.6 requires ^0.30.1.
Upgrading Motion to SQLx 0.9.0 would therefore add a SQLx API migration while
still requiring an additional compatibility change. This route has not been
built or qualified here.

## Option C: downgrade Catabolic to Motion's SQLite

The released match is rusqlite 0.32.1 with libsqlite3-sys ^0.30.1. It provides
all five named features Catabolic enables: bundled, backup, functions, hooks,
limits. Matching feature names do not establish feature or behavior parity.

A manifest-only downgrade would not compile the existing API usage:

- rusqlite 0.32.1 progress_handler/authorizer return (), whereas 0.40.2 returns
  Result<()> and checks that the connection is owned
- 0.32.1 limit/set_limit return i32, whereas 0.40.2 returns Result<i32>
- Catabolic propagates those errors with `?`, including in
  `crates/catabolic-store/src/embedding.rs`, `migration.rs`, `sql.rs`, and other
  bounded-query owners

For example the current `db.set_limit(category, limit)?` and
`db.progress_handler(1000, Some(poll_interrupt))?` call sites cannot keep the
same type contract under 0.32.1. Adapting them requires source changes and new
proof of cancellation, authorization, limits, restoration and ownership behavior.
No downgrade changes or downgraded build were made in this assessment.

There is also a concrete SQLite runtime downgrade: the official
libsqlite3-sys 0.30.1 archive bundles SQLite 3.46.0 (2024-05-23), versus current
3.53.2. SQLite's official 3.49.1 notes fix a concat_ws memory-write bug introduced
in 3.44.0; 3.49.2 notes fix a memory-error bug introduced in 3.40.0. Going to
3.46.0 loses those later fixes. This is version evidence, not a claim that a
particular Catabolic entrypoint is exploitable. SQLite correctness fixes and
changed query/function behavior likewise require requalification. Existing
Catabolic sidecar-header qualification explicitly cites its current bundled
SQLite source; reusing that receipt after an engine downgrade is insufficient.

Thus a no-regression downgrade is not established and is not recommended as the
smallest fix. Even a working compile would not establish it.

## Required qualification before production acceptance

1. The existing combined native host contract: real bounded reads/mutations,
   scans, helper identity, pending status, cancellation, drain, close/reopen
2. Motion's database migrations/upgrades, verified backup/restore, WAL recovery,
   SQLx transactions/savepoints, prepared statements/error mappings and search
3. Catabolic's existing bounded-query hooks, authorizer/limits, cancellation,
   migration, transaction/recovery, and targeted SQLite sidecar fixtures
4. Both owners active concurrently on their separately owned databases, under
   failure/cancellation and shutdown; blocking work stays off async executor
   threads
5. Each supported platform and release profile, exact SQLite compile options,
   one sys crate/version and no accidentally loaded second SQLite
6. Complete lockfile and applicable dependency-advisory review, including the
   shared-version shifts forced by existing Catabolic pins

The patch itself executes no new code: it selects a newer already-used bundled
SQLite for SQLx. That reduces unnecessary Catabolic change, but C API stability
and a passing smoke are not proof of every FFI, concurrency or security property.
No full security review is claimed.

## Primary evidence

- https://index.crates.io/sq/lx/sqlx-sqlite
- https://docs.rs/crate/sqlx-sqlite/0.8.6/source/Cargo.toml
- https://docs.rs/crate/sqlx-sqlite/0.9.0/source/Cargo.toml
- https://index.crates.io/ru/sq/rusqlite
- https://docs.rs/crate/rusqlite/0.32.1/source/Cargo.toml
- https://static.crates.io/crates/rusqlite/rusqlite-0.32.1.crate
- https://static.crates.io/crates/libsqlite3-sys/libsqlite3-sys-0.30.1.crate
- https://www.sqlite.org/releaselog/3_49_1.html
- https://www.sqlite.org/releaselog/3_49_2.html
- https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html#the-patch-section
