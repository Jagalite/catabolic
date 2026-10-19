# Exact executable-document bang and spread ownership

This sparse source-only overlay is based on sealed stage07 virtual map `8ce4830b5ca5652c168bdc82506a135514f73ac6e2e15c6f79a1ae1d6c56c052`, seal `42a9d81ab3b4bf9b066e3a510ee9c3b34b8d6f60c114b41fa938c07051e88f77`. Unchanged files are inherited from stage06. No candidate compilation or product/oracle/runtime execution occurred.

## Narrow independent policies

`Found::Spread` is a private failure-provenance discriminator, produced only for the lexer's already recognized punctuation token whose exact spelling is `...`. No lexer, recognizer, grammar, token advancement, first-failure stopping, location construction, budget or public Error API changes are made. `Found::Punctuation('.')` is not owned as executable spread. One and two dots retain the existing unsupported-scalar lexical behavior.

Only `GrammarSite::ExecutableDocument` with `Expected::Unexpected` changes:

- Bang gains ordinary ownership; its existing before-token-limit ownership is unchanged
- Spread gains before-token-limit ownership only; its ordinary ownership remains unchanged and false absent legacy ownership

The policies are deliberately asymmetric and tested independently with the legacy-owned flag both false and true. No arbitrary punctuation group, `GrammarSite::Other`, required-value spread, directive punctuation or name/colon grammar is newly owned. Existing field-value bang and variable/string exceptions remain independent.

## Exact historical preimages

The separate `graphql-parser-executable-punctuation-v1.json` composition contract binds three complete immutable historical cases:

| Fixture | Pointer | Historical name | Promoted lane |
| --- | --- | --- | --- |
| diagnostic-ownership-v1 | /cases/21 | unowned-executable-bang | ordinary |
| unsupported-scalar-precedence-v1 | /cases/25 | recognized-spread-control | before-token-limit |
| boundary-ownership-v1 | /cases/18 | earlier-bang-before-bracket | ordinary |

The complete case, literal/reconstructed input bytes, args, reference stdout/stderr/exit and archived stage02 legacy/companion envelopes were verified against the sealed assessment and fixture hashes before reuse. Only boundary-ownership /cases/18 includes a separate `frozen_parse` object; its diagnostic is crosschecked against the reference. The other two cases have complete historical reference process captures and no separate `frozen_parse`; none is invented.

Composition requires exact stage07 case arrays and preserves every previous promotion. It changes exactly three labels in three lanes. The two other lanes are verified no-op deep copies. The matching over-budget bang control at required-value-precedence-v1 /cases/39 is unchanged. Repeating composition in a target lane fails closed; a no-op lane remains identical and can be checked repeatedly. Across five historical composed inventories, 24 mismatch-labelled residuals and six matching controls remain. This is metadata inventory arithmetic, not observed candidate compatibility.

## Staged validation and future qualification

Seven stdlib-only standalone metadata controls validate complete preimages, strict composition/nonmutation, exact asymmetric scope, all old promotions, and rejected raw-byte/input/args/order/partial inventory/process-envelope mutations. Tests block product/oracle imports and process creation. Metadata tests run with pinned Python 3.14 and Python 3.11; Ruff uses the inherited repository configuration and `--no-cache`, Rustfmt uses edition 2024. Failed preparation/lint receipts are retained when corrected.

Five new Rust unit tests remain uncompiled/unexecuted. They cover the three captured diagnostics plus the matching bang control, typed policy matrices, dedicated spread versus every existing punctuation token and lone/two-dot lexical behavior, earlier unowned and later owned failure ordering, valid fragment/inline spreads, inert string/comment spelling, original-source Unicode/rewrite locations, comment counts 3,999/4,000/4,001 and a successful over-budget token before later grammar. Uncaptured structural source controls are not described as reference captures.

The existing full-process and disposable-owner runtime consumers are staged to compose these three replacements after stage07. Their local/HTTP preflight, cancelled/borrowed owner, transaction/temp state, recovery and nonmutation checks remain intact. These consumers have not been run. The finite `evidence/capture-plan.json` contains future probes with null process expectations and explicit uncaptured status.

Before adoption, deliberately compose with sibling lexical capture work using exact symbols, fixture IDs and input hashes in `evidence/composition-impact.json`; bind fresh baseline/candidate identities; capture remaining finite controls and complete envelopes; then run authorized native and canonical qualification. No sibling work is read, guessed or merged here. Maintain at least 2,000,000,000 free bytes. No full source copy, hardlinks, build cache, Cargo, product/oracle capture, target access, real catalog, primary checkout change, commit, push or adoption occurred. Unknown-profile and mapping-runtime-v2 choices remain untouched.
