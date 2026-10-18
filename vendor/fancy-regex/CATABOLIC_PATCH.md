# Catabolic patch to fancy-regex 0.19.2

This directory holds fancy-regex 0.19.2 as published on crates.io (MIT, see
`LICENSE` and `AUTHORS`). It is selected through `[patch.crates-io]` in the
workspace `Cargo.toml`. The patch bounds backtracking searches for the native
scan filters (`compile_pattern` and `search` in
`crates/catabolic-store/src/observation.rs`). Upstream behavior is unchanged
unless a limit is set.

- **`with_deadline(deadline, f)`** sets a thread-local deadline for the
  searches `f` performs. The VM checks it every 64 instructions in
  `vm.rs` `run_with` and fails with `RuntimeError::DeadlineExceeded` once it
  has passed. This mirrors the frozen engine's per-search timeout.
- **`RegexBuilder::storage_limit` / `RegexOptionsBuilder::storage_limit`**:
  the search fails with `RuntimeError::StorageLimitExceeded` once the held
  backtracking branches plus saved capture values exceed the limit. It is
  checked before every instruction.
- The VM also checks the deadline before every delegated search.
- Reverse (lookbehind) delegates, built in `compile.rs`, take the same NFA
  size limit and DFA cache capacity as forward delegates. Upstream builds
  them without limits.
- The delegate DFA size limit also caps the lazy DFA cache of forward
  delegates and wrapped regexes. Upstream uses regex-automata's 2 MiB
  default.
- **`with_compile_budget(limit, f)`** charges every regex-automata engine
  built while compiling on this thread. Each delegate, wrapped pattern and
  reverse lookbehind is charged its retained memory plus the full capacity
  of its lazy DFA caches, as soon as it is built. Compilation fails once the
  total exceeds `limit`. The call returns the bytes charged, so callers can
  budget across patterns.

A single delegated regex-automata call is not interrupted. Catabolic bounds
it ahead of time:

- each delegate's automaton is limited by the delegate size limits;
- capture groups are capped at 32;
- filters are not evaluated on paths longer than 4096 bytes.

Patterns without fancy features never enter the VM. regex-automata searches
them in time linear in the path, under the same limits.

To review the patch, compare with the published crate:
`diff -r ~/.cargo/registry/src/*/fancy-regex-0.19.2 vendor/fancy-regex`.
The differences are `src/lib.rs`, `src/error.rs`, `src/vm.rs`,
`src/compile.rs` and this file,
plus the removed `.cargo_vcs_info.json` and `Cargo.lock` (unused for a path
dependency).
