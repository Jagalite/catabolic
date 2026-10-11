# Reviewed public-facade development snapshot

2026-10-11. This local continuation adds Components, fallback Policies and
CatalogRefresh facades, the legacy `catalog-refresh watch` adapter, and shared
consumer JSON response decoding. Python remains the default. This is an
independently reviewed **development snapshot**, not a full aggregate, strict
paired-wheel, platform, migration milestone, release or publication qualification.

## Exact selected coverage

- Components/Policies: **10 preserved rows**, 13 installed scenarios and 37 paired
  groups. Seven Store tests, seven installed Session lifecycle/signal regressions,
  and independent 23 revision / eight budget / 60 cursor cases also pass
- CatalogRefresh: **eight preserved rows**, 16 installed scenarios and 43 paired
  groups. Five Store tests and real-signal cancellation checks also pass
- Consumer JSON: eight owner methods / 48 response scenarios pass through the
  copied CLI and installed extension. Three new independent Rust tests plus the
  helper's three unit tests cover 65,793 short byte strings and 498 oracle cases
- Parent replay: 16 source-contract tests pass; whole-checkout Ruff and formatting
  pass, with 542 Python files formatted
- Legacy CLI ledger: **204 adapted-but-unqualified, 27 missing, 10 partial, one
  safety-gated**, still 242 rows. The newly adapted leaf is catalog-refresh watch

These 18 new preserved rows are distinct from the 79 preserved rows selected by
prior workflow/query/processing gates. Across those gate inventories, 97 of 159
preserved rows are selected and 62 remain outside them. That is an inventory count,
not a completed-row percentage or a claim that all earlier gates ran on this wheel.

## Independent corrections

- Components now checks live evaluation cancellation during the lookup and
  revalidates before accounting; exact latched signal exceptions are preserved
- Components qualification binds the tested wheel/extension to the source-stable
  build manifest instead of merely listing unrelated current source hashes
- CatalogRefresh handles malformed layout metadata within each row's retry
  boundary and skips watcher-owned rows before reading irrelevant metadata
- Configure validates falsey/malformed/non-text owner metadata before any write,
  and validates recovery/removals/catalog before Python enabled coercion
- Frozen finish's SQLite exception compatibility is retained via an exception-only
  import; source guards reject SQLite engine imports/connection/SQL calls
- Shared Jellyfin/PIN JSON decoding required no production correction in review;
  the extracted algorithm is mechanically equivalent to the previous PIN decoder

Each owner document retains exact scope, failed attempts, fixtures and residuals.
No known output publication restriction or blocked review scope was relaxed.

## Source and artifact identities

Shared local manifest: `.local-tests/phase4-shared-dev-3/build-manifest.json`.
Before/after crate source fences and exact installed payload checks passed.

- Source: `6f9230216379d0a8a74007b8ea3d807b855681cbfc46802901fd6887d1a8371e`
- Helperless wheel: `922f7ec922275752abd42e7f4103eca6097f5b051b40a460d9bbb5018f4daf0b`
- Native CLI: `07f177aa32ff32f8633a411a96a4f967ac31ba7f708b6f009c72f67afe27331f`
- Legacy CLI: `9d56c663188f0dd3b5e395b702b3084e8ffaebdfe402c82217368e268ee3f300`
- Extension: `b915a026371968526a79c307f57a5b3c84961b7579c75aa862c33294bf38e0f6`
- Components final receipt: `fb07a58fd5f8f12a4f2956fdacfd6cccb389abf157646419b8a421d05e5f8aa4`
- Refresh final receipt: `16475a04a8ea481a24f3ddeae952d448daa00b0a1ac8c3c47809e9086dddf00f`
- JSON review report: `5bc3ae0067660b64f90fc93bfad486bde1e5c10ca00759be2e71e458c89840e1`

Reports are in `.local-tests/components-policies-independent-review/`,
`.local-tests/catalog-refresh-independent-review/`, and
`.local-tests/consumer-json-review/`. They distinguish earlier immutable artifacts
from this final selected development replay.

## Integration still required

The separate public-workflows branch has newer independently reviewed GraphQL
runtime-input corrections, XML gate hardening and a Linux process-test fixture
repair. They are not part of this development wheel and must be merged and
requalified together. The two new facade gates currently accept explicitly bound
helperless development artifacts; strict paired-release-wheel mode and CI
integration remain to implement. Remaining public fallback/worker surfaces,
legacy CLI gaps, foreign context compatibility, parser/executor residuals, full
platform/real-service/performance/rollback evidence, safety-blocked output paths,
and prerequisite M0–M3 hard gates remain open. No M4–M11 hard exit gate closes here.
