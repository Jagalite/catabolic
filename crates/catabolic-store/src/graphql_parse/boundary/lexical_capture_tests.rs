// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Expectations are captured frozen diagnostics; this stage05 test is unexecuted.
use super::*;
use serde_json::Value;

fn contract() -> Value {
    serde_json::from_str(include_str!(
        "../../../../../tests/parity/graphql-parser-lexical-capture-v1.json"
    ))
    .unwrap()
}

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
        Ok(()) => panic!("expected captured first failure"),
    }
}

fn syntax_error(value: &Value) -> Error {
    Error::Syntax {
        message: value["message"].as_str().unwrap().into(),
        start: Pos {
            line: value["locations"][0]["line"].as_u64().unwrap() as usize,
            column: value["locations"][0]["column"].as_u64().unwrap() as usize,
        },
        end: None,
    }
}

#[test]
fn captured_104_lexical_targets_preserve_typed_reason_and_original_diagnostic() {
    let contract = contract();
    assert_eq!(
        contract["contract"],
        "native-graphql-parser-lexical-capture-v1"
    );
    let targets = contract["promotions"].as_array().unwrap();
    assert_eq!(targets.len(), 104);
    let mut reasons = [0; 4];
    for target in targets {
        let case = &contract["cases"][target["index"].as_u64().unwrap() as usize];
        assert_eq!(case["name"], target["name"]);
        assert_eq!(case["reference"], target["expected"]);
        let source = case["document"].as_str().unwrap();
        let diagnostic = &case["frozen_parse"]["stdout"]["formatted"];
        assert_eq!(
            *diagnostic,
            case["reference"]["stdout"]["data"]["errors"][0]
        );
        let expected = syntax_error(diagnostic);
        for maximum in [None, Some(4000)] {
            let failure = first_failure(source, maximum);
            let index = match (target["reason"].as_str().unwrap(), failure.kind) {
                ("MissingDigit", FailureKind::InvalidNumber(NumberReason::MissingDigit)) => 0,
                (
                    "InvalidContinuation",
                    FailureKind::InvalidNumber(NumberReason::InvalidContinuation),
                ) => 1,
                (
                    "SingleQuote",
                    FailureKind::UnexpectedCharacter {
                        reason: CharacterReason::SingleQuote,
                        code_point: '\'',
                        ..
                    },
                ) => 2,
                (
                    "InvalidUnicodeEscape",
                    FailureKind::String(StringReason::InvalidUnicodeEscape),
                ) => 3,
                _ => panic!(
                    "captured reason differs: {} {:?}",
                    case["name"], failure.kind
                ),
            };
            if maximum.is_none() {
                reasons[index] += 1;
            }
            assert_eq!(
                failure.error, expected,
                "{} maximum={maximum:?}",
                case["name"]
            );
            assert!(failure.owned());
            assert!(failure.before_limit());
        }
        assert_eq!(
            inspect(source, &mut vec![]),
            Err(expected.clone()),
            "{}",
            case["name"]
        );
        assert_eq!(
            before_token_limit(source, 4000),
            Some(expected),
            "{}",
            case["name"]
        );
    }
    assert_eq!(reasons, [43, 22, 29, 10]);
}

#[test]
fn captured_48_earlier_grammar_controls_do_not_promote_a_later_lexical_failure() {
    let contract = contract();
    let mut count = 0;
    for case in contract["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["group"] == "unowned first failure must stop")
    {
        count += 1;
        let source = case["document"].as_str().unwrap();
        for maximum in [None, Some(4000)] {
            let failure = first_failure(source, maximum);
            assert!(
                matches!(
                    failure.kind,
                    FailureKind::Grammar {
                        site: GrammarSite::SelectionName(_)
                            | GrammarSite::ArgumentName { .. }
                            | GrammarSite::ObjectMemberColon(_)
                            | GrammarSite::DirectiveArgumentColon(_),
                        ..
                    }
                ),
                "{}",
                case["name"]
            );
            assert!(failure.owned(), "{}", case["name"]);
            assert!(failure.before_limit(), "{}", case["name"]);
        }
        let mut edits = vec![];
        let expected = syntax_error(&case["frozen_parse"]["stdout"]["formatted"]);
        assert_eq!(
            inspect(source, &mut edits),
            Err(expected.clone()),
            "{}",
            case["name"]
        );
        assert!(edits.is_empty());
        assert_eq!(
            before_token_limit(source, 4000),
            Some(expected),
            "{}",
            case["name"]
        );
    }
    assert_eq!(count, 48);
}
