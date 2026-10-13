<!-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors -->
<!-- SPDX-License-Identifier: MIT -->

# Disposable native ASGI transport checkpoint

This is a partial M10 transport implementation. It does **not** complete the
preserved `http.app.create_app` row or qualify production native HTTP. Python
remains the installed default. The frozen reference remains
`453fca983222c6665a48775eef67c489c96b527e`; schema 32, contract 1.7.0, and the
published contract artifacts are unchanged.

## Admission deliberately remains closed

`catabolic_native.http.app.create_app(database, *, read_only=True,
allowed_hosts=None, origins=None, streams=8, per_principal=2, rate=120)` preserves
the call signature and returns an actual asynchronous ASGI callable. Ordinary
database paths, including paths to otherwise valid catalogs, are refused.

The only admitted `database` is a native capability created by
`catabolic_native.http.experimental.disposable_fixture()`. Creation owns a fresh
temporary directory and initializes a new catalog. It cannot take a catalog path
or adopt an existing catalog. Its read-only `path` property is available to seed
disposable fixtures. The capability pins the newly generated catalog ID; app
construction and every admitted request check that identity. Close each app using
ASGI lifespan shutdown or `await app.aclose()`, then call `fixture.close()`.
Fixture close refuses while native app/route/stream owners remain. Dropping the
last owner also retires its temporary directory.

Every HTTP request must supply an explicit numeric loopback `scope['client']`.
Missing, hostname, wildcard, and remote client addresses are refused. Forwarded,
X-Forwarded-*, X-Real-IP, and Via headers are refused even with a loopback peer.
These checks run in the native admission method before body callbacks, limits,
or route work. There is no local HTTP listener or proxy hop that could erase a
remote peer. A trusted ASGI host supplies the scope; this API cannot authenticate
an intentionally fabricated scope supplied by arbitrary code in its own process.

The capability is strictly for disposable data, never production catalogs,
credentials, or real media. It is not an authorization mechanism for untrusted
in-process code. Environment variables and ordinary path arguments cannot enable
production admission. Mounted applications with a nonempty `root_path` are
explicitly refused; WebSocket and ASGI extension protocols are unqualified.

## Ownership and lifecycle

The Rust adapter invokes the existing `catabolic-http` handler and shares its
application-state constructor with the socket listener. Each ASGI instance owns
one persistent native App, including its rate counts, stream limits, and request
semaphore. Different instances have independent limits. No Python routing,
authentication, database, content, or business-policy implementation is added.

The Python layer only maps ASGI scope/events and schedules `receive` and `send`.
Ingress, egress, and request-retirement waits use nonblocking native channel
operations plus event-loop readiness notifications. They never occupy Python's
bounded default executor while waiting for each other. Per-direction events are
cleared before examining native state, and registration wakes once to cover
already-ready channels. Runtime shutdown alone uses a background executor call.
Bounded native channels carry at-most-64-KiB ingress/egress chunks. The existing
native 1-MiB/30-second request-body owner remains authoritative. The response
adapter reserves its one delivery slot before polling the next native body
frame. It preserves content/SSE streaming and exact response bytes; it supplies
Content-Length for exact-size ordinary bodies, as Hyper does on socket transport.

Disconnect, task cancellation, and callback failure close upload/response
channels, abort the request future, and await that future's retirement. Stream
guards remain owned by the existing native body. Python callback exceptions are
re-raised as the original exception objects. App shutdown cancels active Python
calls and shuts down the native runtime. An already accepted blocking operation
cannot be undone by transport cancellation; shutdown waits up to five seconds
for blocking tasks, whose retained state keeps the fixture alive if they outlive
that bound. No fixture cleanup is claimed while those owners remain.

Lifespan startup/shutdown are supported. Shutdown is terminal; a closed instance
refuses HTTP calls and reports startup failure. Lifespan callback failure also
closes the app. Hosts omitting lifespan must call `aclose()` explicitly.

## Scoped evidence

`tests/test_rust_asgi_adapter.py` runs against an installed native wheel and the
pinned frozen Python source. Its fifteen methods cover constructor/signature and
admission refusal, capability identity/lifetime, paired identity/query/HTTP
errors and exact content/range/HEAD/conditional transfer, explicit hosts/CORS,
body bounds, concurrent persistent rate state and instance isolation, chunked
ingress/egress, stream-limit cleanup after disconnect/cancel, callback exception
identity, partial-body cancellation, lifespan shutdown/failure, admission closure before asynchronous shutdown, and
concurrent delayed-body progress/cancellation with a one-thread default executor.

`tests/test_rust_asgi_smoke.py` adds six framework-free installed-wheel methods
usable on minimal Python 3.11. They exercise a real ASGI request, explicit
ordinary/remote refusal, streamed body bounds, cancellation retirement, and
terminal lifespan cleanup, and delayed-body concurrency with one executor thread. Tests use only freshly owned disposable catalogs.
They open no network listener and use no real credential or service.

The local replay receipt records exact commands, source hashes, wheel/extension
hashes, imported module paths, interpreter versions, and log hashes under
`.local-tests/asgi/validation.json`. The wheel is a debug local ASGI test artifact,
not the complete release/helper qualification artifact.

## Independent concurrency correction

The initial `f03b39b` artifact is rejected. Independent review reproduced executor
starvation: blocking egress waits occupied every default-executor thread, leaving
body delivery queued behind them. Two delayed-body calls stalled with two
threads; 32 calls stalled with the machine's 13-thread default. Merely increasing
thread counts would not remove that dependency. The replacement readiness bridge
removes it, with deterministic delayed-body, cancellation, and close regressions.
The original wheel, installed bytes, source receipt, and logs are retained under
`.local-tests/asgi/rejected-f03b39b`; independent reproduction files remain under
`.local-tests/asgi/independent-review`. Earlier passing fixtures do not qualify
the rejected artifact.

## Gates still open

The public production constructor remains unavailable. Full route/contract
acceptance, exact installed release/helper qualification, macOS and other platform
acceptance, mounted deployments, TLS and trusted reverse proxies, remote ingress,
and private unregistered database-sidecar alias qualification remain open as
documented in `RUST_MIGRATION_HTTP.md`. This adapter changes none of the native
authorization/content/filesystem policies and closes none of those release gates.
Previously blocked broad reviews were not retried as part of this transport work.

## Installed gate and aggregate routing

`scripts/native_asgi_installed_tests.py` is a release-only fixture gate. Run it
outside the checkout with `-I -B`. It requires explicit `--lane`, `--wheel`,
`--source-root`, `--build-manifest`, `--native-binary`, `--legacy-binary`,
`--reference-root`, `--reference-python`, and `--report` inputs. The full lane also
requires `--dependency-wheelhouse`. There is deliberately no development switch.

- `--lane full`: exactly 15 adapter methods plus 6 framework-free smoke methods,
  totaling 21 distinct successes, on exactly Python 3.14.6
- `--lane minimal`: exactly the same 6 smoke methods on Python 3.11, with optional
  HTTP/query frameworks absent; paired Python 3.11 behavior is not qualified

Both lanes bind the complete installed wheel and both packaged/external helpers
to the same release-build source digest. Before/after fences cover implementation
and fixture sources, workflow and locks, frozen reference bytes, interpreter
identities, wheel/build/helper bytes, and imported package objects and paths.
A fresh bytecode-cache root prevents stale cached code from replacing recorded
source. Frozen Python source is the only added package import root. Third-party
imports must belong to exact verified wheel payloads. The minimal interpreter's
existing setuptools startup hook, when present, is separately pinned as
interpreter setup, not a fixture dependency. This is trusted-local content
provenance, not hermetic-build or hostile-directory attestation.

Exact identifiers live in `scripts/native_asgi_scenarios.py`. Missing, additional,
duplicated, skipped, expected-failure, or unsuccessful tests cannot satisfy either
lane. Success and `.attempt.json` receipts are cleared before argument parsing,
project imports, and optimized-mode refusal, including repeated canonical
`--report` arguments. Started/failed attempts are preserved; only a fully
successful source-stable run writes the success receipt. CI retains both.

The aggregate's explicit debug extension is not an installed wheel.
`check-native-mutations.sh` therefore opts into `--defer-installed-asgi`. Before
importing either fixture module, the aggregate verifies the exact two paths,
classes, methods and 21 IDs. Its receipt records mandatory pending installed
prerequisites, both interpreter lanes, and the dedicated gate/CI step. These are
never skipped or successful aggregate tests. Exit status and
`selected_aggregate_passed_without_skips` concern only tests actually executed;
`passed_without_skips` remains false while installed prerequisites are pending.
Unrequested deferral and drifting discovery inventories fail closed. The
separate installed CI step is mandatory in both matrix lanes.

## Offline dependency provenance and current replay limit

The paired lane requires all 18 distributions pinned by `runtime.lock` and
`http-test.lock`, including exact wheel-archive SHA-256s. Installed version labels
or RECORD claims alone are insufficient: every payload is compared with its
locked archive; missing/extra/duplicate/modified payloads are refused.

`scripts/native_asgi_dependencies.py` only exports existing local cache bytes.
It supports pip HTTP-v2 cached wheel bodies (`*.body`, direct ZIP wheel bytes)
and cached wheel files (`*.whl`). Only lock-matching archives that also match the
installed interpreter's payloads qualify; another platform's wheel cannot
substitute. It neither downloads nor invokes pip. CI assigns an explicit cache
directory to its existing pinned dependency installation, then requires a
complete validated export before publishing the candidate artifact. The full
installed lane installs those exported paths with only `pip --isolated
--disable-pip-version-check install --no-index --no-deps`. Cache retention is not
assumed: missing or unsupported cache representations stop CI at export. This
new CI routing has not been executed locally.

Local inspection successfully exported and verified the seven already-cached
runtime distributions. The eleven HTTP distributions' exact wheel archives are
absent from the available local cache: `fastapi==0.141.1`, `uvicorn==0.52.4`,
`httpx==0.28.1`, `httpcore==1.0.9`, `annotated-doc==0.0.5`, `click==8.5.0`,
`h11==0.16.0`, `starlette==1.6.0`, `anyio==4.15.1`, `idna==3.19`, and
`certifi==2026.7.22`. Required hashes remain in `requirements/http-test.lock`.
No replacement archive was generated or downloaded. Strict paired replay remains
blocked until those exact archives and the unified same-source release build
are available. The reviewed helperless debug artifact was replayed only as
development evidence and cannot satisfy this gate. Source/gate controls and debug
replays do not close production, owner-review, platform, or release gaps above.
