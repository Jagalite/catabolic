# catabolic

Programmable media catalog with query-driven rules and safe output projections.
This distribution is the native (Rust) engine, built with maturin. It ships:

- `catabolic_native`: the native extension, the paired `catabolic-native` and
  `catabolic-legacy-native` helpers and the native Python facade;
- `catabolic`: a thin compatibility layer that forwards the supported Python
  API names to `catabolic_native` objects without any engine logic;
- the `catabolic` and `catabolic-native` console scripts.

Platform: macOS arm64, Python 3.11 or newer. Version 1.0.0 supports the CLI and
the preserved Python API listed in `docs/PYTHON_API_1_0.md`; every other former
public name is deprecated and raises an error that points to that list.

Do not install this distribution over a legacy `catabolic` 0.x environment; use
a fresh virtual environment.
