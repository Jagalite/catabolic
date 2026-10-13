# Preserved TMDB candidate intake

<!-- SPDX-FileCopyrightText: 2026 The Catabolic Contributors -->
<!-- SPDX-License-Identifier: MIT -->

This is an opt-in **adapted, unqualified** checkpoint for the preserved
`identify --file-id ... --query ... [--year ...] [--apply]` command. Python remains
the default. No migration milestone, complete CLI parity, public Python facade,
live-provider behavior, or release gate is declared complete.

## Ownership

The preserved parser and terminal adapter call `catabolic_runtime::identify`.
The runtime owns the bounded HTTP request and candidate construction. Apply
forwards each candidate to the existing `catabolic_store::curation::proposal_put`
transaction; it does not introduce another proposal protocol or accept proposals.
No items, associations, mappings, managed links, or source-file contents are
created by this command. The existing curation owner and its prior qualification
limits are unchanged, and no blocked broad curation or filesystem review is
reopened by this adapter.

Retained behavior includes:

- Database/profile admission before query/year/credential validation
- Nonempty UTF-8 query capped at 512 bytes; optional integer year 1 through 9999
- Fixed TMDB movie search endpoint, page one, `include_adult=false`, exact query
  escaping, bearer credential environment reference, and JSON Accept header
- A one-MiB response bound, 256-KiB request bound, redirect refusal, no ambient
  proxy, and the existing process owner's 15-second whole-request deadline
- Native self-helper supervision, cancellation, termination and reaping; no
  production Python execution or fallback, credential arguments, or token records
- Only the first 20 provider rows are considered, with no replenishment of skipped
  rows. IDs must be JSON integers rather than booleans or floats, and titles must
  be strings. Arbitrary-precision integer identity text is retained
- Frozen Python 3.14.6 / Unicode 16 word-token and decimal-year semantics. Ranking
  uses casefolded token-set overlap and a stable descending sort, not a probability
- One fetched timestamp shared by the page; evidence and attribution retain the
  real provider URL even when an explicitly enabled local fixture serves the data
- Preview does not require the file ID to exist. Apply only validates it when a
  valid candidate reaches the proposal owner; no candidates means no proposal work
- Existing per-candidate commits occur in provider order before report sorting.
  Duplicate identical candidates reuse one proposal ID within a page. A later
  proposal refusal does not roll back previously committed candidates
- `complete` follows the provider's first-page count and produces exit 3 when
  more pages exist. Text, JSON, and machine modes retain their own formatting

## Explicit differences and remaining gaps

Native intake releases the catalog connection and advisory writer while HTTP is
running, using the existing detached-store mechanism. Frozen intake holds its
owner through the request. Native reopens with a database-identity fence and
revalidates the profile before proposals, then snapshots current recorded state
through the existing proposal owner. Replacement prevents any late response from
creating proposals. This is an intentional native lifecycle difference.

Malformed JSON or invalid network JSON encoding produces the bounded native
`CatabolicError: invalid TMDB JSON response`, not Python's `JSONDecodeError` or
`UnicodeDecodeError` diagnostics. The shared native decoder also rejects
nonfinite JSON values and invalid Unicode scalar sequences. Malformed nonnumeric
`total_pages` produces an ordinary native error rather than Python's uncaught
TypeError traceback. As in frozen intake, pagination validation happens after
candidate proposal commits. These are recorded compatibility differences, not
claims of byte-identical malformed-provider diagnostics.

Underlying ureq/urllib differences in TLS, DNS, uncommon statuses, truncated
bodies, header limits and invalid header controls are not fully qualified.
Cancellation is checked before candidate commits; already committed proposals
remain durable on later cancellation. Exhaustive commit-time signal schedules,
macOS, installed artifact/helper pairing, public Python embedding, and live TMDB
qualification remain open. No credential persistence or live-service tests ran.

## Evidence

`tests/test_rust_identify.py` uses independent disposable catalogs and the frozen
`453fca983222c6665a48775eef67c489c96b527e` Python source in fresh pinned 3.14.6
processes. The oracle's only transport substitution changes the fixed TMDB origin
to the fixture's numeric loopback URL; candidate/CLI/proposal logic is unmodified.
Native fixtures require both `CATABOLIC_TMDB_FIXTURE=1` and
`CATABOLIC_TMDB_FIXTURE_ENDPOINT=http://127.0.0.1:PORT` (numeric IPv6 loopback is
also admitted by the validator). Named, external, credential-bearing, and
path-bearing endpoints are refused. All credentials are synthetic.

The suite compares exact stdout/stderr/exit status after normalizing only fetched
clock values, UUIDs derived from those clock values, and separate database paths.
For persisted state it additionally normalizes SQLite creation timestamps. Every
actual proposal UUID is independently recomputed from its actual evidence and
snapshot before normalization, and persisted snapshots/payloads are compared.
Tests cover admission/error order, credentials, request/response limits, provider
shape/paging/filtering, Unicode and UTF-16 responses, three presentation modes,
apply/duplicate/partial-commit behavior, pending-recovery forwarding, writer
contention type, cancellation, timeout, detached writer progress, and replacement
fencing. Native unit tests cover word-token/ranking helpers and readonly/cancel refusal.
The Unicode resource is reproducible with `scripts/generate_identify_unicode.py`;
no runtime Python is required.

Reproduction (after sourcing the local migration toolchain and selecting a private
build target):

```sh
cargo build --offline -p catabolic-cli --bin catabolic-legacy-native
CATABOLIC_LEGACY_BINARY="$CARGO_TARGET_DIR/debug/catabolic-legacy-native" \
  python -m unittest tests.test_rust_identify -v
cargo test --offline -p catabolic-runtime --lib identify::tests
cargo clippy --offline -p catabolic-cli --bin catabolic-legacy-native --no-deps -- -D warnings
python scripts/legacy_cli_owners.py --check
```

The ownership ledger advances only `identify` from missing adapter to
`adapted_unqualified`. All unrelated missing, partial, and safety-gated rows stay
unchanged.

## Local receipt, 2026-10-12

The final private Linux development binary passed all **17 Python methods**:
14 identify methods plus three existing offline/error/read-only CLI regressions.
All **three native identify unit tests** passed. Scoped runtime and CLI Clippy
with warnings denied, Ruff checks/formatting, ownership-ledger regeneration,
Unicode-resource byte regeneration, and `git diff --check` passed.

The tested executable SHA-256 was
`c82f317cc0e2f9ff1441f7fa10458c182654a09506692918f8795fe885068cd2`.
This is a private development artifact, not an installed or release receipt.
No source or executable changes occurred during that final 17-method run.
