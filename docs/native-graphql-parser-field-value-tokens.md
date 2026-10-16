# Exact required field-value tokens

This source-only candidate extends sealed stage05 (`d73e0e8fa13698000cab48196f2598d01d7750259852a70276c692cf2b816720`). It has not been compiled or run. Historical captured envelopes are evidence, not new native results or a compatibility claim.

## Bounded production change

- `RequiredValue { context: Argument(Field), position: ... }`, `Expected::Value`, `Found::Punctuation('!')` is owned in both independent policies at root, list-element and object-member positions
- `Found::Eof` is newly ordinary-owned only at root and list-element field-value positions
- EOF admission and object-member EOF are deliberately unchanged pending exact captures
- No grammar, lexical control flow, location formatting, token/byte limits, public Error API, variable flag, directive exception, first-failure stopping or caller provenance changed
- `GrammarSite::Other` is never newly owned. Executable `!` retains its existing admission-only policy

Existing typed provenance is sufficient. No new tag, message matching, whole-document matching or parser architecture is required.

## Exact historical witnesses

The separately versioned `graphql-parser-field-value-tokens-v1.json` preserves complete preimages and hashes from two byte-frozen files:

| Fixture | Index | Name | Historical active lane |
| --- | ---: | --- | --- |
| boundary-ownership-v1 | 36 | closer-root-eof | ordinary |
| boundary-ownership-v1 | 37 | closer-list-eof | ordinary |
| boundary-ownership-v1 | 40 | closer-root-bang | ordinary |
| boundary-ownership-v1 | 56 | required-list-bang | ordinary |
| boundary-ownership-v1 | 57 | required-object-bang | ordinary |
| required-value-precedence-v1 | 46 | overbudget-closer-root-bang | before-token-limit |

The four bang rows and two EOF rows were checked against the sealed remaining-categories inventory, exact document bytes/args, canonical entire cases, and complete reference/legacy/companion envelopes. Historical reference captures use frozen commit `453fca983222c6665a48775eef67c489c96b527e`, Python 3.14.6 and graphql-core 3.2.12. Native envelopes are historical stage02 captures, not stage05 baseline captures. The old fixture provenance remains byte-identical and bound by full raw-file and canonical provenance hashes. No expected result was synthesized.

The composition accepts only exact stage05 inventories in these two lanes and changes only these six expectation labels. It preserves all earlier promotions and all other rows. Across the five earlier composed contracts, the remaining mismatch-labelled historical rows become 34, with six matching historical controls retained. This is metadata arithmetic, not a runtime result.

## Controls and future qualification

Six metadata tests cover exact six-row identity, independent policy scope, composition/nonmutation, prior promotion preservation, rejected provenance/order/field/envelope changes, rejected raw/partial/repeated composition and byte drift. Existing 23 metadata tests remain unchanged. Product imports and subprocesses are guarded during these tests.

Four added Rust unit tests are staged but uncompiled: exact captured messages/locations; typed independent-policy matrix; all directive callers and variable-default provenance; first-failure/tail stopping; successful token 4,001 and Unicode comment advances around 3,999/4,000/4,001. They include valid-neighbor and object-member-EOF nonpromotion controls. New structural controls do not claim frozen diagnostic or process expectations.

The existing Rust disposable-owner integration suite and Python complete-process/local/HTTP suite consume the six replacements after existing stage05 promotions. Their updated runtime paths are staged but unexecuted. No Cargo, product/oracle process, target/cache access, primary checkout edit or push is part of this candidate.

Before qualification, bind fresh stage05 baseline helper identities and a rebuilt candidate to exact source maps. Replay the six full envelopes and existing suites; capture any expanded EOF/admission or location cases explicitly before changing expected diagnostics. `evidence/capture-plan.json` outside the source tree records uncaptured controls separately with no fabricated expected outputs. Verify local/HTTP preflight, cancellation, borrowed-owner recovery and nonmutation. Keep the 2,000,000,000-byte disk floor at capture/build boundaries.

Unknown-profile admission, mapping-runtime-v2 decisions and the four historical crash/deep-recursion exclusions remain unchanged. No full compatibility waiver is claimed.
