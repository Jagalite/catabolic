# Ordinary notification HTTP JSON wire checkpoint

<!--
SPDX-FileCopyrightText: 2026 The Catabolic Contributors
SPDX-License-Identifier: MIT
-->

This is a narrow M9 development checkpoint, based on `d51cf41`, for the
ordinary notification body's JSON serialization. It does not complete M9,
qualify installed artifacts, or change the default Python route.

## Frozen owner and implementation

The reference is commit `453fca983222c6665a48775eef67c489c96b527e`.
Its `notifications.send()` serializes the worker input using the store's sorted,
UTF-8 JSON codec. The isolated `notification_worker.webhook()` then decodes that
input and sends `json.dumps(envelope).encode()`. Consequently the wire envelope
has recursively sorted keys, default comma/colon spaces, no final newline,
and Python's default ASCII escaping, including DEL and UTF-16 surrogate pairs
for supplementary Unicode scalar values.

Native `HttpTransport::send_url()` previously used the UTF-8 codec for the final
HTTP body. Its single send call now uses the existing
`catabolic_core::encode_with(envelope, true, false)`. No shared codec behavior
was changed. The worker-input budget still uses UTF-8 bytes before escaping.
Mapped request bodies continue to use their separate frozen UTF-8 codec.
Notification policy, delivery decisions, retries, leases, credentials, transport
settings, optional Apprise handling, and defaults are unchanged.

## Regression scope

`crates/catabolic-runtime/tests/notification_wire_json.rs` invokes the production
native transport and a fresh process importing the selected frozen notification
owner. Each pair sends to an owned numeric-loopback receiver. Four tests cover:

- Exact ordinary HTTP body bytes and selected content/idempotency headers for
  Unicode keys and values, BMP and supplementary scalar boundaries, combining
  marks, all C0 controls, DEL, quote/backslash/slash escaping, nested objects and
  arrays, code-point key order, finite numeric spellings and a large integer
- An ASCII control with exact spaces, key order and no trailing newline
- An accepted input whose ASCII-escaped body expands past 16 KiB, preserving the
  existing pre-escape input-budget behavior
- A mapped-request control whose body must remain UTF-8

`tests/test_rust_legacy_consumer_watch.py` additionally compares full ordinary
notification bodies in the existing populated watch scenario. Its new durable
receipt test uses six synthetic job subjects in separately owned disposable
catalogs, initialized with the frozen store. It compares exact machine output,
receiver bytes, event/destination/delivery rows, retry then complete outcomes,
attempt counts and cleared leases, and verifies a completed delivery is not
sent again. The existing snapshot helper normalizes only generated lease/event
identities and wall-clock fields; the six pre-seeded event IDs remain intact.

Both frozen Python and native initially produced identical durable outcomes but
different Unicode body bytes. The failing pre-fix Rust results, raw paired HTTP
body hex and normalized durable receipts are retained locally. One intermediate
test assertion also compared arbitrary-precision float token spellings (`1e-7`
versus `1e-07`) as if they were different values after wire parity had passed;
that fixture-only assertion was corrected and its failure log retained.

## Qualification limits

This work exercises synthetic, owned loopback receivers only. It does not claim
arbitrary Python lone-surrogate compatibility, all numeric inputs, live-service
acceptance, macOS, installed wheels, release readiness, whole-workspace parity,
or any broader M9/M11 gate. No broad definitions, filesystem, hardlink,
source-access, observation, remount, or watcher safety review was performed or
retried. Existing qualification and review gates remain open.

Local logs, paired receipts, pinned source inventories, artifact hashes and
preserved development binaries are under `.local-tests/notification-wire-json/`
in the isolated development checkout.

## Local qualification receipt (2026-10-12)

On Linux with Rust 1.99.0, the final implementation passed all four wire tests
and all twelve consumer-watch tests on each of Python 3.14.6 and 3.11.15, without
skips. A final source/artifact-fenced repeat passed the four wire tests and both
changed notification watch/receipt methods on each interpreter. All 25 existing
core unit tests and both existing notification runtime unit tests passed.
Scoped dependency-inclusive Clippy with warnings denied, Rust formatting,
Ruff checks/formatting, managed-doc synchronization and diff checks passed.

The first 3.11 watch run failed because the inherited test environment removed
`PYTHONPATH` and that venv had no installed Catabolic for nested `python -m`
workers. The watch fixture now sets the entire child `PYTHONPATH` to the selected
frozen source after the existing helper removes ambient `PYTHONPATH` and
`PYTHONHOME`; existing safe-path and user-site controls are preserved. This is
a test-only provenance correction. The failed run remains available as
`intermediate-311-unpinned-nested-worker.log`. The final fixture also reproduces
the pre-fix byte mismatch on both interpreters using the preserved baseline
executable; those failure logs and full paired receipts are retained.

The final native CLI SHA-256 is
`4d466b15c233396b932a457c2ea1cfdba5a0df1add59afe8f971649e1ae5a115`;
the legacy CLI used for the watch/receipt tests is
`519e9d5ea80968a893f72f49491771952730dd6a82076fd827fda34cbd288383`.
The wire test executable SHA-256 is
`a19eb50528960339d9e623b2f2888bd2e963521196d1bb195aa1fc993579cf3c`.
The immutable gzip copies are bound by `artifacts/manifest.json`. Source and
frozen-reference inventories remained byte-identical across the final focused
run; this receipt text was appended afterward without changing executable code.

Fresh independent narrow review found no blocking serialization issue and
independently reproduced the four wire tests and both changed notification
watch/receipt methods on both interpreters. Injected unrelated ambient Python
path/home values were discarded by the final fixture. The changed watch module
is outside the selected installed-adapter gate's fixed 105-case inventory and
fixture-adaptation manifest, so neither inventory requires an update. The
development `test_rust_*.py` aggregate still selects it. This is not an
installed-wheel isolation result, and the review does not promote a broader gate.
