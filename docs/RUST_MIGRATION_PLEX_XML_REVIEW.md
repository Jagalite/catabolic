# Independent shared Plex XML review

Reviewed 2026-10-11 against the [owner checkpoint](RUST_MIGRATION_PLEX_XML.md), its frozen source receipts, the copied CLI `bece721dbacdc59a3c7ef4e1ba26b7344a2b5a5af1b54446723712c2b991e15d`, and frozen Python revision `453fca983222c6665a48775eef67c489c96b527e` using the pinned CPython 3.14.6 / Expat 2.8.2 environment.

**Result: two XML-boundary defects were found, corrected, and requalified.** The scoped XML review passes on the replacement artifacts below. Python remains the default. This is not a full migration, release pair, platform qualification, general XML grammar conformance proof, or clearance of an unrelated blocked review.

## Findings and corrections

### Reserved XML processing-instruction target

The original native parser accepted `<?XML version="1.0" encoding="UTF-8"?>`, which frozen ElementTree rejects. UTF-16 input with that prefix also escaped the lowercase declaration's encoding validation. Both UTF-8 and UTF-16 discovery returned `complete=true` on the original copied CLI; frozen returned `invalid_response`.

`XmlText::parse` now checks actual parsed processing-instruction nodes and rejects every case spelling of the exact reserved target `xml`. The valid initial lowercase declaration remains accepted. Initial mixed/uppercase, nested, and trailing targets are covered, alongside comments, CDATA, escaped attribute values, and legal `xml-stylesheet` controls. This is a node-level check, not a substring filter. It does not reject literal text that resembles a processing instruction.

### Explicit empty namespace

roxmltree represents an explicit `xmlns=""` namespace as `Some("")`. Requiring `namespace().is_none()` dropped valid unqualified elements after a namespace reset. The original copied CLI omitted reset `Directory`, `Scanner`, `Agent` and `Part` nodes; a root `<Part xmlns="" .../>` also failed indexing parity.

The four element-name helpers in consumers, import, matching, and metadata now accept either absent or empty namespace URIs. Nonempty namespaces remain excluded. Attribute lookup was not broadened: unqualified attributes still use their exact expanded-name semantics, so foreign attributes cannot supply or shadow the local fields.

New differentials cover namespace resets beneath foreign default namespaces, arbitrary foreign item tags where frozen permits them, direct unqualified child paths, root iteration, explicit empty import/detail roots, and rejection of nonempty-namespace `MediaContainer` lookalikes. Metadata performs a real synthetic PUT/readback using the reset paths.

## Independent evidence

- **2,004 directed decoder vectors** compare frozen parsing with a small probe compiled from the exact production `xml.rs` and static maps. These independently cross encoding/declaration aliases, case/separator variants, mixed BOMs, UTF wire formats, malformed/truncated inputs, declaration boundaries, Unicode scalar edges and attribute normalization. Five initial reserved-target mismatches were found; the identical corpus has zero mismatches after correction
- **17 independent bound vectors** verify raw UTF-8 and decoded UTF-16/Windows-1252 just below, exactly at, and just above 8 MiB. Exactly-at-limit inputs pass; oversized ones return `response_limit`. Element and ordinary-PI node counts confirm the 200,000-node budget, including the document and root nodes
- **175 subprocess tests pass** on the final copied CLI: 56 consumers/import, 74 matching/metadata, 26 prior metadata-review/XML, and 19 new independent XML tests. The new tests compare complete frozen/native reports, existing plan comparisons, readonly database bytes and applicable allowed database changes through the existing disposable fixtures
- **22 copied-runtime unit checks pass**: nine shared consumer/XML and thirteen Plex checks. This includes the original 76,800 per-byte outcomes, 165 rejected declaration spellings, 185 complete-wire fixtures, the new PI unit, and the existing DTD/budget cases
- **16 supporting native checks pass**: two metadata runtime review cases, three metadata Store units, and eleven import/matching Store integration cases
- **Six installed-development-wheel cases pass**, outside the checkout under `-I`: two new correction/literal/recovery cases, two owner XML cases, and two existing independent metadata exception/cancellation cases. The new cases recover from a rejected UTF-16 reserved target, then complete namespace-reset discovery, import, metadata PUT and readback
- All **20 wheel payload members**, excluding pip's rewritten RECORD, match installed bytes. The wheel contains no paired helper; these are development-wheel checks only
- Strict runtime all-target Clippy, scoped Rust/Python formatting, Ruff, generated-resource reproduction and diff checks pass

The copied probe is source-level parser evidence rather than an additional production API. The actual CLI and installed extension tests establish that production callers use the corrected boundary.

## Ownership, provenance and retained bounds

The review inspected all four callers' owned decoded-text lifetimes. The document borrows the local `XmlText`; each caller completes parsing and conversion to owned output while that owner is alive. Choices collect owned strings before dropping their temporary documents. No self-referential tree, unsafe lifetime extension or runtime Python callback was added.

The generated maps and fixture reproduced exactly using the pinned interpreter and frozen source. The generator checks the frozen adapter bytes, revision, Python/Expat versions and Linux registry; 62 maps and 268 normalized aliases remain unchanged. The 300 accepted declaration spellings and all byte outcomes are verified by the copied runtime tests. Runtime maps are compile-time static data; the JSON oracle is test-only. Runtime dependencies are unchanged, and the native CLI's ELF dependency list contains no Python library.

The 8 MiB raw and decoded budgets, node ceiling, decoded DTD/entity rejection and disabled resolver remain intact. The known frozen UTF-16 empty-DTD acceptance, decoded-size safety tightening and unsupported-codec exception normalization remain explicit differences. No safety bound was weakened for parity. User-registered codecs and other platforms' codec registries remain outside this pinned evidence.

The XML changes stay behind `Adapter::xml`; transport and JSON decoding were not changed. A UTF-8 Jellyfin response containing non-ASCII Unicode still passes the frozen differential.

## Separate pre-existing M9 follow-on

Four additional Jellyfin JSON wire probes (UTF-8 BOM, UTF-16LE, UTF-16BE and UTF-32) succeed in frozen Python but return native `invalid_response`. This result is identical on the original XML owner CLI and the final corrected XML CLI. The exact synthetic raw bytes, responses and artifact hashes are in the separate `jellyfin-gap.json` receipt. This is a pre-existing JSON-decoder parity gap, not an XML regression, and it was not fixed or counted as passing XML parity.

## Final artifact identities

- CLI: `00400c71042d8736abeab3217538dce43ae1262ef5e58b6f8676ed8506dbd06a`
- Runtime test executable: `b6d5052f2e120150c7a4ea8236e0f2e303b33d3c247afc034a98e85c3c4c8046`
- Installed/copied extension: `68b7640ac0eddd8c62325a50df28369840c56e66074d743ca64d501db89bdb39`
- Development wheel: `52a8e680a91c9f223e87e4576111fe2b86e4eb73d30ac54520211de234ecd9ce`

The full local receipt is `.local-tests/plex-xml-independent/`: `REPORT.md`, `commands.txt`, `source-sha256.txt`, `artifact-sha256.txt`, logs, original and corrected decoder results, bound cases, separate JSON-gap evidence and wheel-byte verification. Initial reviewer-test expectation/lint fixes and intermediate artifact runs are retained and are not counted as final passes.

Old metadata receipts and owner artifacts were left unchanged. Concurrent unrelated work means these artifacts qualify this XML slice only. No real services, real credentials, real catalogs, commits, pushes, PRs or releases were used. The previously blocked broad definitions/interchange and filesystem/source-access reviews were not retried or reopened; M6 and the remaining M4–M11 gates are unchanged.
