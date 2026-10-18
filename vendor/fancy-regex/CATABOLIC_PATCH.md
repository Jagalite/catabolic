# Catabolic patch to fancy-regex 0.19.2

This directory is fancy-regex 0.19.2 as published on crates.io (MIT, see
`LICENSE` and `AUTHORS`), selected through `[patch.crates-io]` in the
workspace `Cargo.toml`. The only change is a search step limit, used by the
native scan filters (`compile_pattern` in
`crates/catabolic-store/src/observation.rs`):

- `RegexBuilder::step_limit` / `RegexOptionsBuilder::step_limit` (default
  unlimited, so upstream behavior is unchanged unless set);
- `RuntimeError::StepLimitExceeded`;
- in `vm.rs` `run_with`, each instruction costs one step. An instruction that
  inspects the haystack costs one step plus the haystack length: delegated
  forward and backward searches, absent repeaters, seeks, literals and
  backreferences. The search fails once the steps, or the held backtracking
  branches plus saved capture values, exceed the limit.

Patterns without fancy features never enter the VM. fancy-regex hands them
to regex-automata, which searches in linear time within its size limits.

Compare with the published crate to review the patch:
`diff -r ~/.cargo/registry/src/*/fancy-regex-0.19.2 vendor/fancy-regex`.
The differences should be `src/lib.rs`, `src/error.rs`, `src/vm.rs` and this
file, plus the removed `.cargo_vcs_info.json` and `Cargo.lock` (unused for a path dependency).
