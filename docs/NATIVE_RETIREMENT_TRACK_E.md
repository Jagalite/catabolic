<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

# Native retirement, track E: packaging and distribution

Decisions of October 16 (binding): macOS arm64 only, the native engine becomes
the `catabolic` distribution itself (maturin, thin compatibility layer, version
1.0.0), the Python API is the CLI plus the 159 preserved names, and every other
public name is deprecated with a documented list (`PYTHON_API_1_0.md`).

## Single distribution

`crates/catabolic-python/pyproject.toml` now builds one distribution named
`catabolic` 1.0.0 with maturin. Its `python-source` ships two top-level packages:

- `catabolic_native`: the extension, the paired `catabolic-native` and
  `catabolic-legacy-native` helpers and the native facade;
- `catabolic`: the thin compatibility layer. It contains no engine, SQL,
  provider, process or filesystem business logic and never imports the frozen
  Python engine. It explicitly reexports existing `catabolic_native` objects, so
  constructors, methods, argument handling, result objects, exceptions and safety
  refusals stay owned by the native facade. `catabolic.cli.main` is the same
  returning `catabolic_native.cli.main` object, which keeps the paired-helper
  manifest checks. No traceback is synthesized.

Console scripts: `catabolic = catabolic.cli:main` and
`catabolic-native = catabolic_native:main`. The Cargo workspace version is 1.0.0.
The earlier separate `catabolic-native` 0.2.0 and `catabolic-native-compat`
candidates no longer exist as distributions.

### Exclusive environment

Never install this distribution into an environment that contains the legacy
`catabolic` 0.x distribution or another `catabolic` namespace; both own the
same files. Use a new environment. The compatibility qualification
(`scripts/qualify_native_compatibility.py`) refuses legacy, split or foreign
catabolic installations and preloaded namespaces, requires the installed
compatibility files to equal the checked-in source, and then compares forwarded
object identities on the exact installed native artifacts.

### Limits that remain

Unselected private/deep imports, legacy class `__module__` strings and pickle
paths, `python -m catabolic.cli` and arbitrary sqlite connection compatibility
are outside the supported API. Production HTTP admission, publication/recovery,
omitted output-root creation, native exception behavior and the unknown-profile
and mapping-v2 decisions keep their existing limitations. Name presence and
forwarding tests do not establish behavioral parity or authorize retiring the
Python engine.
