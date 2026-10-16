# Opt-in native compatibility namespace candidate

This separate project supplies `catabolic` import names and the `catabolic`
console entrypoint by explicitly reexporting the existing `catabolic_native`
objects. It contains no engine, SQL, provider, process or filesystem business
logic and never imports the legacy implementation. Constructors, methods,
argument handling, result objects, exceptions and safety refusals remain owned
by the existing native facade. The console is the same returning
`catabolic_native.cli.main` object, which retains the existing paired-helper
manifest checks. No traceback is synthesized.

This is **not the default distribution**. The repository root project and native
Maturin project are unchanged. The candidate distribution is deliberately named
`catabolic-native-compat`; the final `catabolic` distribution identity, extras,
full import surface, supported platforms and release/default transition remain
pending. No package build, installation or publication is part of this slice.

## Exclusive environment and exact artifacts

Never install this candidate into an environment containing the legacy
`catabolic` distribution or another `catabolic` namespace. Different distribution
names can own colliding files; this candidate does not pretend installation is a
safe in-place migration. Use a new dedicated environment for any separately
authorized future build/installation qualification.

The `catabolic-native==0.2.0` dependency expresses package resolution only. A
matching version does **not** qualify the native provider. The source-only
qualification entrypoint `scripts/qualify_native_compatibility.py` requires the
exact installed native wheel and its existing same-source release build manifest.
It calls the existing `native_facade_qualification.strict_artifact`, which binds
all installed native payloads, the extension, paired native/legacy helpers,
external executables and their source identity. No new helper/artifact protocol
is added. The candidate lives beneath `crates/`, so the existing paired native
source digest includes its files once adopted.

The entrypoint refuses any installed legacy/candidate distribution, preloaded
`catabolic` module or importable `catabolic` namespace before native imports or
forwarding checks. It does not install anything. In a clean, separately prepared
environment containing the exact native artifacts, run outside the checkout:

    /clean/bin/python -I -B /source/scripts/qualify_native_compatibility.py \
      --source-root /source --wheel /artifacts/exact-native.whl \
      --build-manifest /artifacts/build-manifest.json

This checks the candidate's **source** namespace against the exact installed
native objects, then repeats the native artifact/source fence. JSON is emitted
only on success. It is not installed compatibility-wheel acceptance and does not
run engine operations. A final compatibility wheel requires its own separately
authorized build, payload/entrypoint qualification and supported-platform runs.

## Explicit scope

The forwarding map covers the 159 preserved inventory rows: 136 callable rows,
15 class rows and eight module/value bindings. The module bindings are distinct
proxy modules; their exported native objects retain their identity. Selected
native query/media/tagging and command-handle/error names are also reexported to
keep the existing native facade's cooperating types usable.

The native access module is a module rather than the historical package.
Unselected private/deep imports, legacy class `__module__` strings/pickle paths,
`python -m catabolic.cli`, arbitrary sqlite connection compatibility and all
868 non-preserved inventory candidates are outside this bounded candidate.
Their omission is not approval to remove them from a final release.

All existing limitations remain, including production HTTP admission,
publication/recovery, omitted output-root creation, native exception behavior,
and the pending unknown-profile/mapping-v2 compatibility decisions. Import/name
presence and forwarding tests do not establish full behavioral parity for the
159 rows or authorize retiring the Python engine.
