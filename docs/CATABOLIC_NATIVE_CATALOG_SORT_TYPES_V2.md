# Catalog dictionary-sort diagnostics, v2

This bounded repair preserves the frozen Python 3.14.6 `CatalogQuery` dictionary-sort failure at its native source: `TypeError`, with the exact message `cannot use 'dict' as a dict key (unhashable type: 'dict')`. The limit check still precedes sorting. Scalars retain the available-sort diagnostic. Arrays retain the separately recorded `sort must be text` difference.

The source-selected class travels in a separate, opaque `DetailedError`. Ordinary `Error(message)` construction and all legacy conversions remain ordinary domain errors, even when their message is identical to the dictionary diagnostic. `CatalogQuery::execute_detailed` and `page_detailed` are additive entry points; existing `execute` and `page` deliberately flatten the same implementation into the unchanged legacy `Result`.

The persistent PyO3 owner carries the detailed result inside its unchanged checkout and interrupt owner, restores the session, then converts dictionary provenance to builtin `TypeError`. The stateless helper preserves its historical `RuntimeError` for ordinary errors. The companion CLI still emits operational exit 2 with its existing stderr format. Standard legacy dispatch retains detailed provenance through its private boundary envelope and shared pure normalization of all eight catalog command forms. Its JSON/machine envelopes use the actual source-selected class; plain-text format is unchanged. No traceback or blanket CLI exception equivalence is claimed.

## Rust API compatibility boundary

The original public `#[derive(Debug)] pub struct Error(pub String)` is retained exactly, including constructor-function identity, tuple-pattern destructuring, concrete `catabolic_store::Error` type name, public `.0` read/mutation, ordinary `Debug`/`Display`/source behavior, conversions, and `Result<T>`. `CatalogQuery` retains its public `store` and `profile` fields and legacy method signatures. The administrative detailed-error family and public interrupt APIs are unchanged.

The additive `DetailedError` owns a legacy `Error` and private provenance, with explicit ordinary conversions, borrowed `error()` and diagnostic-evidence accessors, and deliberate `into_error()` flattening. Raw mapping-subject evidence is excluded from `Debug`, `Display`, the source chain, and flattened errors. There is no public constructor for typed provenance and no message-based inference.

An earlier isolated, uncommitted candidate used `Error = StoreError` with a same-named constructor function. It preserved constructor calls and `.0` but broke tuple-pattern syntax and concrete type identity. That candidate and its evidence remain historical; this additive implementation replaces it before integration. It is not described as a shipped API.

## Exact bounded evidence

The immutable v1 contract and original historical capture remain checked. The additive v2 contract covers 25 full process envelopes on each of the frozen owner, persistent native owner, stateless native helper, and companion. All original 17 witnesses now match the persistent owner's class/message contract. The expanded set has 23 matching persistent-owner cases and two explicitly retained array residuals. Sorting callers include items, files, and mappings. Native tests also verify owner/transaction continuity, exact-message spoof rejection, CLI class propagation, and unchanged ordinary error presentation. Fresh candidate evidence must accompany this replacement; historical process captures alone do not qualify its changed binaries. The external-crate `error_api_compatibility` integration test covers the restored Rust API and ordinary/detailed equivalence.

The stateless helper's ordinary `RuntimeError` behavior is intentionally recorded separately. Methods without a public sort argument are outside this bounded contract. This does not qualify installed wheels, platforms, the full native aggregate, destructive/WAL/restore operations, or publication.

Run the Python fixture with explicit `--binary`, `--legacy-binary`, `--parser-binary`, `--extension-binary`, frozen `--reference-root`, and Python 3.14.6 `--python-oracle` through `scripts/qualify_native_mutations.py --pattern test_rust_catalog_sort_diagnostics.py`. The two owner methods fail admission without an explicit extension; development wrappers and extension bytes are fenced before and after.
