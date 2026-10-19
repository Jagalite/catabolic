# Additive native error boundaries

This bounded compatibility repair restores the original public `catabolic_store::Error(pub String)` tuple struct and adds a separate opaque `DetailedError` / `DetailedResult<T>` family for source-selected catalog-sort and mapping-subject diagnostics. It replaces the isolated alias-based candidate; it does not claim that candidate was shipped.

## Compatibility and provenance

The original error constructor, constructor-function identity, tuple pattern, public message field and mutation, concrete type name, `Debug`, `Display`, source behavior, conversions and `Result<T>` remain unchanged. The external-crate `error_api_compatibility` tests exercise these contracts. Ordinary conversions never infer provenance from message text. Flattening via `into_error()` or `From<DetailedError>` intentionally discards details; wrapping that error again stays ordinary.

`DetailedError` contains private provenance selected only by the actual dictionary-sort check or the actual failed mapping subject decode. Its public borrowed accessors allow boundary presentation, but callers cannot construct or mutate typed provenance. Mapping raw evidence is excluded from `Display`, `Debug`, the source chain, and flattened errors. There is no global or thread-local error metadata, pointer identity, unsafe cast, marker in public JSON, or diagnostic-string inference.

## Query ownership and ordering

`CatalogQuery::execute_detailed` and `page_detailed` own the single implementations. Existing `execute` and `page` methods retain ordinary signatures and flatten those implementations. The six private entity methods propagate detailed failures; ordinary helpers and the public `store`/`profile` fields are unchanged. The limit check remains before dictionary-sort classification, existing filter/profile checks retain their order, and arrays retain the two explicitly recorded residuals.

Persistent PyO3 carries `DetailedResult<Value>` as the successful payload through its unchanged Store checkout, interrupt and session-restoration owner. Only after restoration does it translate actual dictionary provenance to builtin `TypeError`. All other ordinary persistent failures remain `CatabolicError`; stateless ordinary failures remain `RuntimeError`.

The legacy CLI adds a private boundary-failure variant separate from the existing public administrative detailed-error family. All eight catalog forms share the same pure request normalization and detailed query owner, preserving the item ID and all-catalogs transformations and missing-database precedence. The separate SQL `query` command is unchanged. Ordinary presentation retains interruption, access, contention, consumer, exit-code and envelope handling; detailed dictionary errors use the existing Python-error presentation path. The companion CLI retains its ordinary text error output.

## Mapping ownership and ordering

The public `history`, `events`, `events_with_subject`, `recover_http`, and administrative and interrupt APIs keep their existing signatures. New `events_with_subject_detailed` and private `history_with_subject_detailed` capture evidence only at the existing failed decoder call. Old entry points flatten that one execution. Aggregation, first-failure selection, the historical HTTP cap/order, selected successful subject capture, filtering, truncation and read ownership remain unchanged.

The CLI uses the existing generic interrupt supervisor and carries the detailed result through `Ok(inner_result)` inside the original interrupt-check reset scope. Cancellation still takes precedence, `signals::finish()` runs after the supervisor even on its error, and presentation still has one completed-output sidecar. There is no Store reopen or second query to classify the failure.

Only source-marked decoder failures reach diagnostic-only syntax reconstruction. Syntax-valid native refusals such as depth, nonfinite values and lone surrogates retain their original native diagnostics. Accepted values are never replaced. The immutable mapping fixture and sort v1/v2 captures remain historical contracts; this candidate requires fresh source- and artifact-fenced process evidence rather than relabeling earlier results.

## Qualification boundary

Focused validation covers external Rust compatibility, ordinary/detailed success and diagnostic equivalence, source-selected provenance and exact-message spoofing, same-owner transaction continuity, first failed aggregate record, selected-earlier-event refusal, unchanged database state, raw redaction, all eight CLI routes, syntax and source guards, sort v2's 25 cases on four surfaces, and the 336 malformed mapping CLI envelopes. Receipts distinguish the executed checks and artifacts from unrun stages.

This does not qualify general native parity, installed wheels, other platforms, public Python `JSONDecodeError` object attributes, synthetic tracebacks, or mapping recovery/publication behavior. All unrelated WAL, restore, destructive publication and interval-effect gates remain fail-closed. No default flip, release, push or real catalog migration is part of this repair.
