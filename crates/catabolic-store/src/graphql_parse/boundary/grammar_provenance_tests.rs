// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Source-selected exact captured caller sites. All native checks await execution.
use super::*;
use serde_json::Value;

fn first_failure(source: &str, maximum: Option<usize>) -> Failure {
    let mut lexer = Lexer::new(source);
    lexer.maximum = maximum;
    let result = lexer.next().and_then(|current| {
        Recognizer {
            lexer,
            current,
            variable: false,
            moves: vec![],
        }
        .document()
    });
    match result {
        Err(failure) => failure,
        Ok(()) => panic!("expected first failure"),
    }
}

fn historical(name: &str) -> Value {
    let text = match name {
        "graphql-parser-budget-precedence-v2.json" => {
            include_str!("../../../../../tests/parity/graphql-parser-budget-precedence-v2.json")
        }
        "graphql-parser-diagnostic-ownership-v1.json" => {
            include_str!("../../../../../tests/parity/graphql-parser-diagnostic-ownership-v1.json")
        }
        "graphql-unsupported-scalar-precedence-v1.json" => include_str!(
            "../../../../../tests/parity/graphql-unsupported-scalar-precedence-v1.json"
        ),
        "graphql-parser-boundary-ownership-v1.json" => {
            include_str!("../../../../../tests/parity/graphql-parser-boundary-ownership-v1.json")
        }
        "graphql-parser-required-value-precedence-v1.json" => include_str!(
            "../../../../../tests/parity/graphql-parser-required-value-precedence-v1.json"
        ),
        "graphql-parser-lexical-capture-v1.json" => {
            include_str!("../../../../../tests/parity/graphql-parser-lexical-capture-v1.json")
        }
        _ => panic!("unknown historical fixture"),
    };
    serde_json::from_str(text).unwrap()
}

fn captured_kind(row: &Value) -> FailureKind {
    let tuple = &row["tuple"];
    let context = match tuple["context"].as_str().unwrap() {
        "Field" | "Argument(Field)" => ArgumentContext::Field,
        "FieldDirective" => ArgumentContext::FieldDirective,
        "VariableDefinitionDirective" => ArgumentContext::VariableDefinitionDirective,
        "Directive" => ArgumentContext::Directive,
        "Alias" | "ExecutableDocument" => ArgumentContext::Field,
        _ => panic!("unknown captured context"),
    };
    let position = || match tuple["position"].as_str().unwrap() {
        "Root" => ValuePosition::Root,
        "ListElement" => ValuePosition::ListElement,
        "ObjectMember" => ValuePosition::ObjectMember,
        _ => panic!("unknown captured position"),
    };
    let site = match tuple["site"].as_str().unwrap() {
        "SelectionName" => GrammarSite::SelectionName(if tuple["context"] == "Alias" {
            SelectionName::Alias
        } else {
            SelectionName::Field
        }),
        "ArgumentName" => GrammarSite::ArgumentName {
            context,
            first: tuple["first"].as_bool().unwrap(),
        },
        "DirectiveArgumentColon" => GrammarSite::DirectiveArgumentColon(context),
        "ObjectMemberName" => GrammarSite::ObjectMemberName(ValueContext::Argument(context)),
        "ObjectMemberColon" => GrammarSite::ObjectMemberColon(ValueContext::Argument(context)),
        "VariableReferenceName" => GrammarSite::VariableReferenceName {
            context: ValueContext::Argument(context),
            position: position(),
        },
        "RequiredValue" => GrammarSite::RequiredValue {
            context: ValueContext::Argument(context),
            position: position(),
        },
        "ExecutableDocument" => GrammarSite::ExecutableDocument,
        _ => panic!("unknown captured site"),
    };
    FailureKind::Grammar {
        site,
        expected: match tuple["expected"].as_str().unwrap() {
            "Name" => Expected::Name,
            "Literal(:)" => Expected::Literal(":"),
            "Value" => Expected::Value,
            "Unexpected" => Expected::Unexpected,
            _ => panic!("unknown captured expectation"),
        },
        found: match tuple["found"].as_str().unwrap() {
            "Name" => Found::Name,
            "Number" => Found::Number,
            "Eof" => Found::Eof,
            "Spread" => Found::Spread,
            token => Found::Punctuation(token.chars().next().unwrap()),
        },
    }
}

#[test]
fn exact_156_residual_captures_keep_typed_owners_coordinates_and_admission_lanes() {
    let contract: Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/graphql-parser-captured-residual-v1.json"
    ))
    .unwrap();
    assert_eq!(
        contract["contract"],
        "native-graphql-parser-captured-residual-v1"
    );
    let rows = contract["cases"].as_array().unwrap();
    assert_eq!(rows.len(), 156);
    assert_eq!(
        rows.iter()
            .filter(|r| r["new_admission_policy"] == true)
            .count(),
        64
    );
    assert_eq!(
        rows.iter()
            .filter(|r| r["earlier_grammar_masked_by_later_token_budget"] == true)
            .count(),
        16
    );
    for row in rows {
        let source = row["document"].as_str().unwrap();
        let diagnostic = &row["reference"]["stdout"]["data"]["errors"][0];
        assert_eq!(*diagnostic, row["frozen_parse"]["stdout"]["formatted"]);
        let expected = Error::Syntax {
            message: diagnostic["message"].as_str().unwrap().into(),
            start: Pos {
                line: diagnostic["locations"][0]["line"].as_u64().unwrap() as usize,
                column: diagnostic["locations"][0]["column"].as_u64().unwrap() as usize,
            },
            end: None,
        };
        let before = row["before_limit_policy"].as_bool().unwrap();
        for maximum in [None, Some(4000)] {
            let failure = first_failure(source, maximum);
            assert_eq!(failure.kind, captured_kind(row), "{}", row["name"]);
            assert_eq!(failure.error, expected, "{}", row["name"]);
            assert!(!failure.owned, "{}", row["name"]);
            assert!(failure.owned(), "{}", row["name"]);
            assert_eq!(failure.before_limit(), before, "{}", row["name"]);
        }
        assert_eq!(
            inspect(source, &mut vec![]),
            Err(expected.clone()),
            "{}",
            row["name"]
        );
        assert_eq!(
            before_token_limit(source, 4000),
            before.then_some(expected),
            "{}",
            row["name"]
        );
    }
}

#[test]
fn exact_75_captured_grammar_rows_preserve_first_failure_and_independent_lanes() {
    let contract: Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/graphql-parser-grammar-provenance-v1.json"
    ))
    .unwrap();
    let rows = contract["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 75);
    assert_eq!(rows.iter().filter(|r| r["promotion"] == true).count(), 64);
    // One parse per original fixture avoids repeatedly allocating the large
    // immutable historical inventories. No candidate process is represented.
    for binding in contract["fixture_bindings"].as_array().unwrap() {
        let fixture = historical(binding["fixture"].as_str().unwrap());
        for row in rows.iter().filter(|r| r["fixture"] == binding["fixture"]) {
            let case = &fixture["cases"][row["index"].as_u64().unwrap() as usize];
            assert_eq!(case["name"], row["name"]);
            assert_eq!(case["expectation"], "baseline");
            assert_eq!(
                case["reference"]["stdout"]["data"]["errors"][0],
                row["diagnostic"]
            );
            let source = case["document"].as_str().unwrap();
            let diagnostic = &row["diagnostic"];
            let expected = Error::Syntax {
                message: diagnostic["message"].as_str().unwrap().into(),
                start: Pos {
                    line: diagnostic["locations"][0]["line"].as_u64().unwrap() as usize,
                    column: diagnostic["locations"][0]["column"].as_u64().unwrap() as usize,
                },
                end: None,
            };
            for maximum in [None, Some(4000)] {
                let failure = first_failure(source, maximum);
                assert_eq!(failure.kind, captured_kind(row), "{}", row["name"]);
                assert_eq!(failure.error, expected, "{}", row["name"]);
                assert_eq!(
                    failure.owned,
                    row["legacy_variable_flag"].as_bool().unwrap()
                );
                assert_eq!(failure.owned(), row["ordinary_policy"].as_bool().unwrap());
                assert_eq!(
                    failure.before_limit(),
                    row["before_limit_policy"].as_bool().unwrap()
                );
            }
            assert_eq!(inspect(source, &mut vec![]), Err(expected.clone()));
            assert_eq!(
                before_token_limit(source, 4000),
                row["before_limit_policy"]
                    .as_bool()
                    .unwrap()
                    .then_some(expected)
            );
        }
    }
}

#[test]
fn exact_name_colon_policy_rejects_other_tokens_sites_and_contexts() {
    // Independent hand-written policy oracle, using a constant irrelevant error
    // message. These are structural assertions, not new captured diagnostics.
    for context in [
        ArgumentContext::Field,
        ArgumentContext::FieldDirective,
        ArgumentContext::VariableDefinitionDirective,
        ArgumentContext::Directive,
    ] {
        for first in [false, true] {
            for (site, expected, found) in [
                (
                    GrammarSite::ArgumentName { context, first },
                    Expected::Name,
                    Found::Punctuation(':'),
                ),
                (
                    GrammarSite::ArgumentName { context, first },
                    Expected::Name,
                    Found::Punctuation(')'),
                ),
                (
                    GrammarSite::ArgumentName { context, first },
                    Expected::Name,
                    Found::Punctuation('!'),
                ),
                (
                    GrammarSite::ArgumentName { context, first },
                    Expected::Name,
                    Found::Eof,
                ),
                (
                    GrammarSite::ArgumentName { context, first },
                    Expected::Value,
                    Found::Punctuation(')'),
                ),
                (
                    GrammarSite::DirectiveArgumentColon(context),
                    Expected::Literal(":"),
                    Found::Name,
                ),
                (
                    GrammarSite::DirectiveArgumentColon(context),
                    Expected::Literal(":"),
                    Found::Number,
                ),
                (
                    GrammarSite::DirectiveArgumentColon(context),
                    Expected::Name,
                    Found::Name,
                ),
            ] {
                let allowed = match site {
                    GrammarSite::ArgumentName {
                        context: ArgumentContext::Field,
                        first: true,
                    } => expected == Expected::Name && found == Found::Punctuation(':'),
                    GrammarSite::ArgumentName {
                        context:
                            ArgumentContext::VariableDefinitionDirective | ArgumentContext::Directive,
                        first: true,
                    } => expected == Expected::Name && found == Found::Punctuation(')'),
                    GrammarSite::ArgumentName {
                        context: ArgumentContext::FieldDirective,
                        first: true,
                    } => {
                        expected == Expected::Name && matches!(found, Found::Punctuation(')' | ':'))
                    }
                    GrammarSite::ArgumentName {
                        context: ArgumentContext::Field,
                        first: false,
                    } => {
                        expected == Expected::Name && matches!(found, Found::Punctuation(':' | '!'))
                    }
                    GrammarSite::DirectiveArgumentColon(
                        ArgumentContext::VariableDefinitionDirective | ArgumentContext::Directive,
                    ) => expected == Expected::Literal(":") && found == Found::Name,
                    GrammarSite::DirectiveArgumentColon(ArgumentContext::FieldDirective) => {
                        expected == Expected::Literal(":")
                            && matches!(found, Found::Name | Found::Number)
                    }
                    _ => false,
                };
                for legacy in [false, true] {
                    let failure = Failure {
                        error: syntax("", 0, "fixed unrelated text".into()),
                        owned: legacy,
                        kind: FailureKind::Grammar {
                            site,
                            expected,
                            found,
                        },
                    };
                    let ordinary_eof = matches!(
                        site,
                        GrammarSite::ArgumentName {
                            context: ArgumentContext::FieldDirective,
                            first: false,
                        }
                    ) && expected == Expected::Name
                        && found == Found::Eof;
                    assert_eq!(failure.owned(), legacy || allowed || ordinary_eof);
                    assert_eq!(failure.before_limit(), allowed);
                }
            }
        }
    }
    for context in [
        ValueContext::Argument(ArgumentContext::Field),
        ValueContext::Argument(ArgumentContext::FieldDirective),
        ValueContext::Argument(ArgumentContext::VariableDefinitionDirective),
        ValueContext::Argument(ArgumentContext::Directive),
        ValueContext::VariableDefault,
    ] {
        for position in [
            ValuePosition::Root,
            ValuePosition::ListElement,
            ValuePosition::ObjectMember,
        ] {
            for (site, expected, found, ordinary) in [
                (
                    GrammarSite::ObjectMemberName(context),
                    Expected::Name,
                    Found::Punctuation(':'),
                    matches!(
                        context,
                        ValueContext::Argument(
                            ArgumentContext::Field | ArgumentContext::FieldDirective
                        )
                    ),
                ),
                (
                    GrammarSite::ObjectMemberColon(context),
                    Expected::Literal(":"),
                    Found::Number,
                    matches!(
                        context,
                        ValueContext::Argument(
                            ArgumentContext::Field | ArgumentContext::FieldDirective
                        )
                    ),
                ),
                (
                    GrammarSite::ObjectMemberColon(context),
                    Expected::Literal(":"),
                    Found::Name,
                    context == ValueContext::Argument(ArgumentContext::Field),
                ),
                (
                    GrammarSite::VariableReferenceName { context, position },
                    Expected::Name,
                    Found::Punctuation(')'),
                    context == ValueContext::Argument(ArgumentContext::Field)
                        && matches!(position, ValuePosition::Root | ValuePosition::ObjectMember)
                        || context == ValueContext::Argument(ArgumentContext::FieldDirective)
                            && position == ValuePosition::Root,
                ),
                (
                    GrammarSite::VariableReferenceName { context, position },
                    Expected::Name,
                    Found::Punctuation(']'),
                    context == ValueContext::Argument(ArgumentContext::Field)
                        && position == ValuePosition::ListElement,
                ),
            ] {
                for legacy in [false, true] {
                    let failure = Failure {
                        error: syntax("", 0, "fixed unrelated text".into()),
                        owned: legacy,
                        kind: FailureKind::Grammar {
                            site,
                            expected,
                            found,
                        },
                    };
                    assert_eq!(failure.owned(), legacy || ordinary);
                    assert_eq!(
                        failure.before_limit(),
                        ordinary
                            && !matches!(
                                site,
                                GrammarSite::VariableReferenceName {
                                    context: ValueContext::Argument(ArgumentContext::Field),
                                    position: ValuePosition::Root,
                                }
                            )
                    );
                }
            }
        }
    }
    for site in [
        GrammarSite::Other,
        GrammarSite::SelectionName(SelectionName::Alias),
    ] {
        for found in [
            Found::Punctuation('}'),
            Found::Punctuation('!'),
            Found::Punctuation(':'),
            Found::Punctuation(')'),
            Found::Eof,
        ] {
            let failure = Failure {
                error: syntax("", 0, "fixed unrelated text".into()),
                owned: false,
                kind: FailureKind::Grammar {
                    site,
                    expected: Expected::Name,
                    found,
                },
            };
            let captured_alias = site == GrammarSite::SelectionName(SelectionName::Alias)
                && found == Found::Punctuation('!');
            assert_eq!(failure.owned(), captured_alias);
            assert_eq!(failure.before_limit(), captured_alias);
        }
    }
}

#[test]
fn captured_callers_stop_before_later_owned_errors_and_keep_variable_exceptions() {
    for source in [
        "{alias:!}",
        "{items(first:1 :2)}",
        "{items(first:1 !)}",
        "query Q @skip(){profile}",
        "fragment F on Query @skip(if true){profile}",
        "{...F @skip()}",
        "{... on Query @skip(if true){profile}}",
        "{profile @skip(if 1)}",
        "{profile @skip(:1)}",
        "{profile @skip(if:{:1})}",
        "{profile @skip(if:{name 1})}",
        "{profile @skip(if:$)}",
        "{items(first:[$])}",
        "{items(first:{name:$)})}",
        "{items(first:{name true})}",
        "{items(first:1 :2) @skip()}",
    ] {
        let first = first_failure(source, None);
        assert!(first.owned(), "{source}");
        assert!(first.before_limit(), "{source}");
        for tail in [
            " %".to_string(),
            " {items(search:\"".into(),
            format!(" {}", "profile ".repeat(4001)),
        ] {
            let extended = format!("{source}{tail}");
            let failure = first_failure(&extended, Some(4000));
            assert_eq!(failure.kind, first.kind, "{source}");
            assert_eq!(failure.error, first.error, "{source}");
            assert_eq!(
                inspect(&extended, &mut vec![]),
                Err(first.error.clone()),
                "{source}"
            );
            assert_eq!(
                before_token_limit(&extended, 4000),
                Some(first.error.clone()),
                "{source}"
            );
        }
    }
    for source in [
        "query Q($x:Int={:1}){profile}",
        "query Q($x:Int={name 1}){profile}",
        "query Q($x:Int @tag(if:{:1})){profile}",
        "query Q($x:Int @tag(if:{name 1})){profile}",
    ] {
        let failure = first_failure(source, None);
        assert!(failure.owned);
        assert!(failure.owned());
        assert!(!failure.before_limit());
    }
}

#[test]
fn caller_tags_preserve_advance_boundaries_original_coordinates_and_valid_neighbors() {
    for source in [
        "{profile}",
        "{alias:profile}",
        "{items(first:1 offset:2){id}}",
        "{items(first:$x){id}}",
        "{profile @skip(if:true)}",
        "query Q($x:Int=1 @tag){profile}",
    ] {
        assert!(inspect(source, &mut vec![]).is_ok());
        assert!(before_token_limit(source, 4000).is_none());
    }
    for newline in ["\n", "\r", "\r\n"] {
        for prefix in [
            format!("# 雪😀{newline}"),
            format!("{{alias:profile}}{newline}"),
            "query Q($x:Int=1 @tag){profile} ".into(),
        ] {
            let source = format!("{prefix}{{items(:1)}}");
            let failure = first_failure(&source, None);
            assert!(!failure.owned);
            assert_eq!(
                failure.error,
                syntax(
                    &source,
                    prefix.len() + 7,
                    "Expected Name, found ':'.".into()
                )
            );
            assert!(failure.owned());
            assert!(failure.before_limit());
        }
    }
    // The unexpected name token must be successfully lexed within the limit;
    // comments retain their existing count and successful token 4001 wins.
    for (prefix, unexpected, tokens) in [
        ("{", "}", 2),
        ("{items(", ":", 4),
        ("{profile @skip(", ")", 6),
    ] {
        for total in [3999, 4000, 4001] {
            let source = format!("{prefix}{}{unexpected}", "#\n".repeat(total - tokens));
            let first = first_failure(&source, Some(4000));
            if total <= 4000 {
                assert!(first.before_limit(), "{source}");
            } else {
                assert_eq!(first.kind, FailureKind::TokenLimit);
                assert!(!first.before_limit());
            }
        }
    }
    let source = format!("{{{}items(:1)}}", "profile ".repeat(4000));
    assert_eq!(
        first_failure(&source, Some(4000)).kind,
        FailureKind::TokenLimit
    );
    assert!(before_token_limit(&source, 4000).is_none());
}
