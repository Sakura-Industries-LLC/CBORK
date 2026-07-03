// Copyright (c) 2023 Input Output (IOG).
// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

use pest::Parser;

pub(crate) mod byte_sequences;
pub(crate) mod comments;
pub(crate) mod group_elements;
pub(crate) mod identifiers;
pub(crate) mod literal_values;
pub(crate) mod rules;
pub(crate) mod text_sequences;
pub(crate) mod type_declarations;

/// A Pest test parser for a full CDDL syntax.
#[derive(pest_derive::Parser)]
#[grammar = "grammar/cddl.pest"]
#[grammar = "grammar/cddl_test.pest.frag"]
pub struct CDDLTestParser;

/// # Panics
pub(crate) fn check_tests_rule(
    rule_type: Rule,
    passes: &[impl AsRef<str>],
    fails: &[impl AsRef<str>],
) {
    for test in passes {
        let parse = CDDLTestParser::parse(rule_type, test.as_ref());
        assert!(parse.is_ok());
    }

    for test in fails {
        let parse = CDDLTestParser::parse(rule_type, test.as_ref());
        assert!(parse.is_err());
    }
}
