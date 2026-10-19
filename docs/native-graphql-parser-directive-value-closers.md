# Exact field-directive required-value closers

Source-only overlay over sealed stage06 map `d768732983a3e361a403debca170d366a8df3ddf0e07a9a44268602247584d94`, seal `2595389090bbfd14f02ee6f233ee010c2720620313703e23040833d51bd47032`. Neither this candidate nor its staged runtime consumers has been compiled or executed. Historical capture reuse is not fresh qualification.

## Production scope

The private `ArgumentContext::FieldDirective` distinguishes directives on fields, including aliases and nested fields, from operation, fragment-definition, named/inline-spread and variable-definition directives. The `directives` routine receives an explicit context at its seven call sites. Only the field-selection branch supplies the new tag. The six other callers keep `Directive`. This is provenance threading only; grammar and token advancement are unchanged.

Both independent ownership policies add exactly these `RequiredValue`/`Expected::Value` pairs for `Argument(FieldDirective)`:

- `Root` with `]`
- `ListElement` with `)`
- `ObjectMember` with `}`

Other position/closer combinations, directive bang/EOF, directive names/colons, object names/colons and `GrammarSite::Other` are not newly owned. Non-field directive parents remain unchanged. The immediate-colon/`)` `ArgumentValue` exception and ordinary variable-definition ownership stay separate and unchanged. No public Error API, lexer, source-coordinate construction, budgets, grammar, recursion strategy or first-failure stopping changed.

## Exact historical replacements

`graphql-parser-directive-value-closers-v1.json` preserves complete original cases, input bytes/args, frozen_parse objects, frozen reference envelopes and historical stage02 legacy/companion envelopes from these immutable fixtures:

| Fixture | Pointer | Name | Historical lane |
| --- | --- | --- | --- |
| boundary-ownership-v1 | /cases/12 | directive-field-root-bracket | ordinary |
| boundary-ownership-v1 | /cases/13 | directive-field-list-paren | ordinary |
| boundary-ownership-v1 | /cases/14 | directive-field-object-brace | ordinary |
| boundary-ownership-v1 | /cases/53 | field-then-directive | ordinary |
| required-value-precedence-v1 | /cases/35 | overbudget-directive-field-root-bracket | before-token-limit |
| required-value-precedence-v1 | /cases/36 | overbudget-directive-field-list-paren | before-token-limit |
| required-value-precedence-v1 | /cases/37 | overbudget-directive-field-object-brace | before-token-limit |

Every preimage matches sealed remaining-categories assessment hashes, and each frozen_parse formatted diagnostic equals the complete reference envelope diagnostic. These are historical reference/stage02 observations, not stage06 baseline or stage07 runtime results. Historical fixture bytes are untouched; no output is synthesized.

Composition accepts only the exact stage06 case arrays in these two lanes and changes these seven expectation labels. It rejects raw, partial, reordered, modified and already-composed arrays. All prior labels are preserved. Across the five composed historical contracts, 27 mismatch-labelled residuals and six matching controls remain. These counts are metadata arithmetic, not observed native parity.

## Validation and remaining obligations

Seven standalone metadata tests execute under pinned Python using only stdlib, immutable fixture JSON and overlay data. They verify identity, preimages, strict composition/nonmutation, all old promotions, rejected scope/provenance/order/input/envelope mutation, rejected raw/partial/repeated composition and raw-file byte drift. Product/oracle import and process-creation guards are enabled. Ruff uses the base repository configuration and `--no-cache`; Rustfmt uses edition 2024. Final checks and initial corrected failures are recorded outside the source overlay.

Five new Rust unit tests remain staged/uncompiled: exact seven captured diagnostics; independent typed-policy matrix; field versus all non-field directive callers, variables/defaults and the immediate-colon exception; first-failure/tail stopping and excluded pairs; original-source Unicode/line endings, comment counts 3,999/4,000/4,001 and a successful over-budget token before later errors. Existing field-token bang/EOF controls retain their assertions with the new field-directive provenance. The older four historical field-directive closer negatives become positives, while the variable-directive negative admission assertion stays unchanged.

The existing disposable-owner integration and complete-process/local/HTTP runtime consumer code is staged to compose these exact seven replacements last. No Rust compilation, Cargo, product/oracle capture, runtime/canonical suite, target/cache access, primary checkout mutation, commit or push occurred.

`evidence/capture-plan.json` in the overlay package lists future controls without expected outputs. Before qualification, bind a fresh baseline and candidate to exact composed source identities, capture uncaptured contexts/order/budget/location controls, run exact complete envelopes, local/HTTP preflight, cancellation, borrowed-owner recovery and nonmutation checks, then broader authorized qualification. Preserve at least 2,000,000,000 bytes free at capture/build boundaries.

The source overlay must be deliberately composed with any sibling lexical capture work. The package identifies the exact seven input IDs and affected source/test paths; it does not merge or claim execution of sibling work. Unknown-profile and mapping-runtime-v2 decisions, historical crash/deep-recursion exclusions and full qualification remain open and unchanged.
