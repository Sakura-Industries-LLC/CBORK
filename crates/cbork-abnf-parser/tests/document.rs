// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Integration tests for the owned ABNF document model.

use cbork_abnf_parser::{
    AbnfElement, AbnfError, AbnfValidationError, DefinitionOperator, NumBase, NumValue, Repeat,
    parse_abnf,
};

#[test]
fn parses_owned_document_and_preserves_source() {
    let source = concat!(
        "digit = %x30-39\n",
        "token = 1*DIGIT\n",
        "quoted = \"abc\"\n",
    );

    let document = parse_abnf(source).expect("ABNF should parse");

    assert_eq!(document.source(), source);
    assert_eq!(document.rules().len(), 3);

    let digit = &document.rules()[0];
    assert_eq!(digit.name().as_str(), "digit");
    assert_eq!(digit.operator(), DefinitionOperator::Assign);
    assert_eq!(digit.span().start_line(), 1);
    assert_eq!(digit.span().start_column(), 1);
    assert_eq!(digit.expression().concatenations().len(), 1);

    let digit_repetition = &digit.expression().concatenations()[0].repetitions()[0];
    if let AbnfElement::NumVal(num) = digit_repetition.element() {
        assert_eq!(num.base(), NumBase::Hexadecimal);
        if let NumValue::Range(range) = num.value() {
            assert_eq!(range.start(), 0x30);
            assert_eq!(range.end(), 0x39);
        } else {
            panic!("expected numeric range");
        }
    } else {
        panic!("expected numeric value");
    }

    let token = &document.rules()[1];
    assert_eq!(token.name().as_str(), "token");
    let token_repetition = &token.expression().concatenations()[0].repetitions()[0];
    if let Some(Repeat::Range(range)) = token_repetition.repeat() {
        assert_eq!(range.min(), Some(1));
        assert_eq!(range.max(), None);
    } else {
        panic!("expected bounded repeat");
    }
    assert!(matches!(
        token_repetition.element(),
        AbnfElement::RuleRef(rule) if rule.as_str() == "DIGIT"
    ));

    let quoted = &document.rules()[2];
    assert_eq!(quoted.name().as_str(), "quoted");
    let quoted_repetition = &quoted.expression().concatenations()[0].repetitions()[0];
    if let AbnfElement::CharVal(text) = quoted_repetition.element() {
        assert_eq!(text.value(), "abc");
    } else {
        panic!("expected quoted string");
    }
}

#[test]
fn rejects_missing_terminal_newline() {
    let source = "rule = \"abc\"";

    let err = parse_abnf(source).expect_err("ABNF must require a terminal newline");
    assert!(matches!(
        err,
        AbnfError::Parse(_) | AbnfError::InvalidAst(_)
    ));
}

#[test]
fn validates_text_and_bytes_against_the_document() {
    let source = concat!("start = 1*alpha \"d\"\n", "alpha = \"a\" / \"b\" / \"c\"\n",);

    let document = parse_abnf(source).expect("ABNF should parse");

    let text = String::from("abcd");
    assert!(document.validate_text(text).is_ok());
    assert!(document.validate_bytes(b"abcd").is_ok());

    let err = document
        .validate_text(String::from("abce"))
        .expect_err("mismatch should fail validation");
    assert!(matches!(err, AbnfValidationError::Mismatch { .. }));
}

#[test]
fn validates_numeric_literal_bytes() {
    let document = parse_abnf("start = %x41.42.43\n").expect("ABNF should parse");

    assert!(document.validate_text("ABC").is_ok());
    assert!(document.validate_bytes(b"ABC").is_ok());
    assert!(document.validate_bytes(b"ABD").is_err());
}
