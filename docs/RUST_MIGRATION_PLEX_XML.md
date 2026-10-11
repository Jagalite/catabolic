# Shared native Plex XML checkpoint

This opt-in checkpoint closes the shared UTF-16 limitation recorded in the [metadata owner checkpoint](RUST_MIGRATION_PLEX_METADATA.md) and its [independent review](RUST_MIGRATION_PLEX_METADATA_REVIEW.md). Those documents and their artifact receipts remain historical evidence. Python remains the default. This is not full migration, platform, M6 output, final artifact, or release qualification.

## Owned decode boundary

`consumers::Adapter::xml` now produces a bounded `XmlText` owner. Its `Cow<str>` borrows ordinary UTF-8 and owns decoded UTF-16 or declared single-byte text. `XmlText::parse` returns a roxmltree document borrowed from that owner. Discovery, library listing, indexed-path verification, scanner/agent choices, import pages, matching detail and metadata snapshots all keep the owner alive until parsing and projection finish. No self-referential structure, unsafe lifetime extension, transport-global decoder, new runtime dependency or Python decoding callback is involved.

The decoder retains the 8 MiB raw-response ceiling and separately bounds decoded UTF-8 text to 8 MiB before a push can exceed it. The tree still has its 200,000-node limit. DTD/entity markers are rejected **after decoding**, and the parser still has DTDs disabled with no resolver. The decoded-byte ceiling and UTF-16 DTD refusal are named native safety bounds: frozen ElementTree checks DTD markers in raw bytes, so an empty UTF-16 DTD passes that historical byte check. The regression records frozen acceptance and native refusal; it does not quietly claim parity for that case.

HTTP routes, credential/header behavior, deadlines, redirect policy, fresh connections, writes and retries are unchanged. Metadata intent/PUT/readback, cancellation, history, and fencing remain their previously reviewed owners' responsibilities.

## Encoding evidence

The oracle is the exact frozen `453fca983222c6665a48775eef67c489c96b527e` source with CPython 3.14.6 / Expat 2.8.2. The scoped wire behavior includes:

- UTF-8, its optional BOM, and case-insensitive canonical `UTF-8` declarations
- UTF-16 little/big endian, with or without BOM, matching canonical declarations, Unicode including surrogate pairs, and leading/declaration XML whitespace
- Refusal of conflicting declared encodings, malformed UTF-8, lone surrogates, truncation, repeated BOMs, UTF-32 and unsupported multibyte declarations
- The frozen standard-library codec registry's accepted declared single-byte fallback maps and aliases, including the distinction between ISO-8859-1 control characters and Windows-1252 punctuation

Expat's fallback does not invoke a general decoder on each complete XML message. It asks Python to decode the 256 possible bytes once, then uses that static map. For example, the Python alias `utf8` accepts ASCII but rejects high bytes, while canonical `UTF-8` uses Expat's real UTF-8 decoder. Likewise accepted escape/HZ codec names do not apply ordinary escape or stateful decoding. Native reproduces the measured maps instead of using an approximate browser codec or an alias-to-UTF-8 shortcut.

`scripts/generate_plex_xml_encodings.py` checks the interpreter version, frozen Git revision, and unchanged adapter source. It discovers the default standard-library registry, records exact parser results, deduplicates 62 static maps / 268 normalized aliases, and emits:

- `crates/catabolic-runtime/src/consumers/xml_encodings.rs`: compile-time maps; no runtime resource loading
- `tests/fixtures/plex_xml_encodings.json`: 300 spelling × 256-byte parser outcomes, 165 rejected single-byte-wire declaration spellings, and 185 complete raw-wire cases

Run the generator with `--check` under the pinned interpreter and the frozen source first on `PYTHONPATH` to verify reproducibility. Unknown or unsupported codec names produce native `invalid_response`; frozen may instead leak a `LookupError` or `ValueError`. This is an explicit recoverable native error normalization, not an assertion that those exception classes are identical. User-registered Python codecs and other platforms' codec registries are not imported or executed by native.

## Namespace and root semantics

Indexed `Part` lookups and scanner/agent iteration now require exact unqualified tags and attributes, as ElementTree does. A foreign attribute cannot supply or shadow `file`, `ratingKey`, `guid`, `name`, or `identifier`. Iteration still includes a matching root and arbitrary nested wrappers where frozen `iter()` does. Discovery, libraries, indexing and choices do not gain a new root-tag requirement. The existing import/detail owners retain their frozen `MediaContainer` requirement and direct unqualified child-path rules.

The existing UTF-16 known-gap test was changed to passing parity only after the new immutable-CLI end-to-end differentials passed. Additional tests compare complete frozen/native discovery/indexing output and import/matching/metadata plans, outcomes, and allowed database changes. The metadata cases perform real loopback PUT and readback across the encoding matrix.

## Executed checks

All checks use disposable catalogs, synthetic media, fake credentials, and numeric loopback HTTP. No real-catalog handover, live-service request, real credential, new source-access review, commit, push, PR or release is involved.

The immutable CLI passed **156 subprocess tests**: 24 shared consumer, 32 import, 26 matching, 48 metadata, 14 independent metadata-review and 12 new XML cases. The copied runtime executable passed **21 units**, including the 76,800 individual-byte outcomes, 165 rejected declarations and 185 complete-wire cases. Three metadata Store units, eleven import/matching Store integration tests, and two runtime metadata review tests passed. Four installed-development-wheel cases passed: two new XML cases and two existing independent metadata exception/cancellation cases. Strict runtime all-target Clippy, scoped Rust/Python formatting, lint, generator reproduction, and diff checks passed.

The final receipt records immutable CLI/runtime executable hashes, source/resource/test hashes, exact commands, and the executed outcomes. Full affected consumer/import/matching/metadata regressions and the new XML cases are run against the copied CLI. The installed development wheel is checked separately, outside the checkout under `-I`, including recovery from a conflicting declaration followed by successful legacy-encoded metadata PUT/readback. Development-wheel evidence is not a final helper-paired artifact qualification.

This checkpoint does not retry, replace, or clear the previously blocked definitions/interchange and filesystem/source-access safety reviews. Existing M6 output and broader M4–M11 gates remain unchanged. A fresh independent review of this XML-owned boundary is still required.

## Frozen artifact identities

- CLI SHA-256: `bece721dbacdc59a3c7ef4e1ba26b7344a2b5a5af1b54446723712c2b991e15d`
- Runtime test executable SHA-256: `9b2465d00659ad514f4691bd5a1c6026d8335ed0727225e32f0d439b77417dd0`
- Installed/copied extension SHA-256: `dcfd9758fb7fff26dcc3180c48871b378c14df9b1c9ba550b7716359bf923eb6`
- Development wheel SHA-256: `fcfba498bf0e3d0fe1b4c7ea40f13aa99a565e5524d2a1b9eb94a99078470098`

All 20 development-wheel payload members, excluding pip's rewritten RECORD, matched the installed bytes. This wheel has no paired helper. Concurrent unrelated owner changes mean these artifacts are scoped XML evidence, not an aggregate release pair. The local XML qualification receipt preserves both intermediate attempts and final immutable artifacts. This evidence is not a general XML grammar conformance proof.
