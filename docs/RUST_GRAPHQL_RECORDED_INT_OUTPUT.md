<!-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors -->
<!-- SPDX-License-Identifier: MIT -->
# Recorded GraphQL Int output completion

This isolated correction targets the two raw-recorded relationship-position
witnesses in `tests/parity/graphql-runtime-scalar-remaining.json`: `1_000` and
`١٢٣`. The original fixture and historical outcomes remain unchanged. These
are recorded-state compatibility controls, not demonstrated public-mutation
inputs or evidence of general parser or Python-integer parity.

## Baseline and versioned evidence

At source `00950e792669fd401fbb59a3a0956b40a8668cc1`, both witnesses still differ
from frozen Python. The exact current legacy envelopes reproduce the retained
native rejection envelopes; native CLI payloads also reject the values. Frozen
Python reproduces the retained successful envelopes, yielding 1000 and 123.

The new `tests/parity/graphql-recorded-int-v1.json` keeps the full reference,
current-baseline legacy, and current-baseline native responses for 42 cases.
Those observed baseline fields intentionally retain the old behavior. The Rust
regression checks the reference response on direct and persistent owners. Full
patched CLI envelopes and exact artifact/source/oracle fences are separate
qualification evidence; historical files are never rewritten as passing data.

The matrix includes signs, negative zero, leading zeroes, mixed Unicode decimal
scripts, surrounding whitespace, both i32 limits, overflow beyond i128, malformed
underscores, fractional/exponent/base-prefix strings, internal whitespace, NUL,
nondecimal numeric characters, non-ASCII minus, Python-numeric whitespace
rejection, empty strings, and 4300/4301-digit boundaries. Two alias controls
retain error order, location, path, sibling completion, and full error envelopes.

## Bounded output-only correction

Only the string branch of GraphQL Int output serialization changes. It accepts
an optional ASCII sign and frozen Unicode decimal digits, with a single underscore
only between digits. It retains the existing surrounding-whitespace trimming.
A streaming scan keeps constant auxiliary memory and saturates magnitude just
beyond the signed i32 limits, while continuing to validate the remaining spelling.
It does not allocate or compute an arbitrary-precision integer. The frozen
CPython 3.14.6 default limit of 4300 decimal digits, including leading zeroes but
excluding underscores, is enforced. Configured alternate Python digit limits
are outside this compatibility claim.

Invalid syntax and the digit limit still produce the existing non-integer error;
valid decimal strings outside i32 produce the existing range error. Numbers,
bools, all other output scalar branches, input coercion, query parsing, JSON,
catalog storage, and public mutation validation are unchanged. The existing
completion machinery continues to own null propagation, errors, budgets,
interrupts, and query-only restoration.

## Shared decimal primitive relocation

The Unicode 16.0 decimal-zero table and decimal-digit helper move unchanged from
`catabolic-runtime/src/document.rs` into `catabolic-core/src/python_decimal.rs`.
The subtitle parser imports the same helper; its numeric grammar does not change.
A core regression checks all Unicode scalar values against the existing frozen
CLI decimal resource, and independent frozen-Python verification confirms all
760 Nd codepoints. No dependency, resource table, or toolchain is added or changed.

## Focused verification

The qualification uses isolated, offline, locked Rust 1.99 builds with debug
information and incremental compilation disabled and one build job. It covers:

- 42 full frozen-Python/current-baseline/patched legacy and native CLI responses
- Direct and persistent-owner recorded-output matrix checks, including unchanged
  raw text storage, transaction ownership, temporary state, and query-only state
- Existing runtime completion regressions for budgets, cancellation, opaque JSON,
  grouped errors, non-null behavior, and reusable owners
- A separately sealed Python-consumer addendum preserves the original stale-test
  failure, then passes all three canonically discovered runtime-completion methods,
  including all 42 recorded-Int CLI triples, 59 grouped cases, five validation
  controls, and the existing opaque-JSON cases
- Core regressions, including exhaustive frozen decimal classification
- Scalar unit checks on operator and HTTP serializer branches
- Existing runtime document/subtitle regressions after primitive relocation
- Focused core/store/runtime Clippy, formatting, and source/artifact/oracle fences

This is focused evidence for the isolated patch. It is not a new full migration,
installed-artifact, platform, generated-client, or live-service qualification.
