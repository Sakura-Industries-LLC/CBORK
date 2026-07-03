// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

use pest::{Parser, iterators::Pair};

use crate::{
    abnf,
    ast::{
        AbnfDocument, AbnfElement, AbnfError, AbnfOption, AbnfRule, Alternation, CharVal,
        Concatenation, DefinitionOperator, GroupedAlternation, NumBase, NumRange, NumVal, NumValue,
        ProseVal, Repeat, RepeatRange, Repetition, Rulename, SourceSpan,
    },
};

/// Parse strict RFC 5234 ABNF into an owned document.
pub(crate) fn parse_abnf(input: &str) -> Result<AbnfDocument, AbnfError> {
    let mut pairs = abnf::ABNFParser::parse(abnf::Rule::abnf, input)?;
    let root = pairs
        .next()
        .ok_or_else(|| AbnfError::invalid("parser returned no root pair"))?;
    if pairs.next().is_some() {
        return Err(AbnfError::invalid("parser returned multiple root pairs"));
    }

    build_document(input.to_owned(), root)
}

/// Convert the Pest root pair into an owned document.
fn build_document(
    source: String,
    root: Pair<'_, abnf::Rule>,
) -> Result<AbnfDocument, AbnfError> {
    if root.as_rule() != abnf::Rule::abnf {
        return Err(AbnfError::invalid("root pair was not abnf"));
    }

    let rules = root
        .into_inner()
        .filter(|pair| matches!(pair.as_rule(), abnf::Rule::rule))
        .map(build_rule)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(AbnfDocument::new(source, rules))
}

/// Convert a single ABNF rule pair into an owned rule.
fn build_rule(pair: Pair<'_, abnf::Rule>) -> Result<AbnfRule, AbnfError> {
    let span = span_from_pair(&pair);
    let mut children = pair.into_inner().collect::<Vec<_>>();
    let name = build_rulename(&take_child(
        &mut children,
        abnf::Rule::rulename,
        "rule was missing a rulename",
    )?)?;
    let operator = build_definition_operator(&take_child(
        &mut children,
        abnf::Rule::defined_as,
        "rule was missing a definition operator",
    )?)?;
    let expression = build_elements(take_child(
        &mut children,
        abnf::Rule::elements,
        "rule was missing an expression",
    )?)?;

    Ok(AbnfRule::new(name, operator, expression, span))
}

/// Convert a definition operator pair into its owned form.
fn build_definition_operator(pair: &Pair<'_, abnf::Rule>) -> Result<DefinitionOperator, AbnfError> {
    if pair.as_rule() != abnf::Rule::defined_as {
        return Err(AbnfError::invalid("expected a definition operator"));
    }

    if pair.as_str().contains("=/") {
        Ok(DefinitionOperator::Incremental)
    } else {
        Ok(DefinitionOperator::Assign)
    }
}

/// Convert an elements pair into the contained alternation.
fn build_elements(pair: Pair<'_, abnf::Rule>) -> Result<Alternation, AbnfError> {
    if pair.as_rule() != abnf::Rule::elements {
        return Err(AbnfError::invalid("expected elements"));
    }

    build_single_inner(pair, abnf::Rule::alternation, "elements")
}

/// Convert a rulename pair into an owned rulename.
fn build_rulename(pair: &Pair<'_, abnf::Rule>) -> Result<Rulename, AbnfError> {
    if pair.as_rule() != abnf::Rule::rulename {
        return Err(AbnfError::invalid("expected a rulename"));
    }

    Ok(Rulename::new(
        pair.as_str().to_owned(),
        span_from_pair(pair),
    ))
}

/// Convert an alternation pair into an owned alternation.
fn build_alternation(pair: Pair<'_, abnf::Rule>) -> Result<Alternation, AbnfError> {
    if pair.as_rule() != abnf::Rule::alternation {
        return Err(AbnfError::invalid("expected an alternation"));
    }

    let span = span_from_pair(&pair);
    let concatenations = pair
        .into_inner()
        .filter(|child| {
            matches!(
                child.as_rule(),
                abnf::Rule::concatenation | abnf::Rule::repetition
            )
        })
        .map(build_concatenation)
        .collect::<Result<Vec<_>, _>>()?;

    if concatenations.is_empty() {
        return Err(AbnfError::invalid(
            "alternation must contain at least one concatenation",
        ));
    }

    Ok(Alternation::new(concatenations, span))
}

/// Convert a concatenation pair into an owned concatenation.
fn build_concatenation(pair: Pair<'_, abnf::Rule>) -> Result<Concatenation, AbnfError> {
    if pair.as_rule() == abnf::Rule::repetition {
        let span = span_from_pair(&pair);
        return Ok(Concatenation::new(vec![build_repetition(pair)?], span));
    }

    if pair.as_rule() != abnf::Rule::concatenation {
        return Err(AbnfError::invalid(format!(
            "expected a concatenation, found {:?}: {}",
            pair.as_rule(),
            pair.as_str()
        )));
    }

    let span = span_from_pair(&pair);
    let repetitions = pair
        .into_inner()
        .filter(|child| child.as_rule() == abnf::Rule::repetition)
        .map(build_repetition)
        .collect::<Result<Vec<_>, _>>()?;

    if repetitions.is_empty() {
        return Err(AbnfError::invalid(
            "concatenation must contain at least one repetition",
        ));
    }

    Ok(Concatenation::new(repetitions, span))
}

/// Convert a repetition pair into an owned repetition.
fn build_repetition(pair: Pair<'_, abnf::Rule>) -> Result<Repetition, AbnfError> {
    if pair.as_rule() != abnf::Rule::repetition {
        return Err(AbnfError::invalid("expected a repetition"));
    }

    let span = span_from_pair(&pair);
    let mut inner = pair.into_inner().peekable();
    let repeat = if matches!(inner.peek().map(Pair::as_rule), Some(abnf::Rule::repeat)) {
        let repeat_pair = inner
            .next()
            .ok_or_else(|| AbnfError::invalid("repetition was missing a repeat"))?;
        Some(build_repeat(&repeat_pair)?)
    } else {
        None
    };

    let element = build_element(
        inner
            .next()
            .ok_or_else(|| AbnfError::invalid("repetition was missing an element"))?,
    )?;

    if inner.next().is_some() {
        return Err(AbnfError::invalid(
            "repetition had unexpected extra children",
        ));
    }

    Ok(Repetition::new(repeat, element, span))
}

/// Convert a repeat pair into an owned repeat.
fn build_repeat(pair: &Pair<'_, abnf::Rule>) -> Result<Repeat, AbnfError> {
    if pair.as_rule() != abnf::Rule::repeat {
        return Err(AbnfError::invalid("expected a repeat"));
    }

    let text = pair.as_str();
    if let Some((min, max)) = text.split_once('*') {
        let span = span_from_pair(pair);
        let min = if min.is_empty() {
            None
        } else {
            Some(parse_u64(min, "repeat minimum")?)
        };
        let max = if max.is_empty() {
            None
        } else {
            Some(parse_u64(max, "repeat maximum")?)
        };
        Ok(Repeat::Range(RepeatRange::new(min, max, span)))
    } else {
        Ok(Repeat::Exact(parse_u64(text, "repeat count")?))
    }
}

/// Convert an element pair into an owned element.
fn build_element(pair: Pair<'_, abnf::Rule>) -> Result<AbnfElement, AbnfError> {
    if pair.as_rule() != abnf::Rule::element {
        return Err(AbnfError::invalid("expected an element"));
    }

    let mut inner = pair.into_inner();
    let child = inner
        .next()
        .ok_or_else(|| AbnfError::invalid("element had no child"))?;
    if inner.next().is_some() {
        return Err(AbnfError::invalid("element had unexpected extra children"));
    }

    match child.as_rule() {
        abnf::Rule::rulename => Ok(AbnfElement::RuleRef(build_rulename(&child)?)),
        abnf::Rule::group => Ok(AbnfElement::Group(build_group(child)?)),
        abnf::Rule::option => Ok(AbnfElement::Optional(build_option(child)?)),
        abnf::Rule::char_val => Ok(AbnfElement::CharVal(build_char_val(&child)?)),
        abnf::Rule::num_val => Ok(AbnfElement::NumVal(build_num_val(child)?)),
        abnf::Rule::prose_val => Ok(AbnfElement::ProseVal(build_prose_val(&child)?)),
        _ => Err(AbnfError::invalid("unexpected ABNF element kind")),
    }
}

/// Convert a grouped alternation pair into an owned group.
fn build_group(pair: Pair<'_, abnf::Rule>) -> Result<GroupedAlternation, AbnfError> {
    if pair.as_rule() != abnf::Rule::group {
        return Err(AbnfError::invalid("expected a group"));
    }

    let span = span_from_pair(&pair);
    let alternation = build_single_inner(pair, abnf::Rule::alternation, "group")?;
    Ok(GroupedAlternation::new(alternation, span))
}

/// Convert an option pair into an owned option.
fn build_option(pair: Pair<'_, abnf::Rule>) -> Result<AbnfOption, AbnfError> {
    if pair.as_rule() != abnf::Rule::option {
        return Err(AbnfError::invalid("expected an option"));
    }

    let span = span_from_pair(&pair);
    let alternation = build_single_inner(pair, abnf::Rule::alternation, "option")?;
    Ok(AbnfOption::new(alternation, span))
}

/// Convert a quoted string pair into an owned character value.
fn build_char_val(pair: &Pair<'_, abnf::Rule>) -> Result<CharVal, AbnfError> {
    if pair.as_rule() != abnf::Rule::char_val {
        return Err(AbnfError::invalid("expected a char_val"));
    }

    let span = span_from_pair(pair);
    let text = pair.as_str();
    let value = text
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .ok_or_else(|| AbnfError::invalid("char_val was not quoted"))?
        .to_owned();
    Ok(CharVal::new(value, span))
}

/// Convert a prose value pair into an owned prose value.
fn build_prose_val(pair: &Pair<'_, abnf::Rule>) -> Result<ProseVal, AbnfError> {
    if pair.as_rule() != abnf::Rule::prose_val {
        return Err(AbnfError::invalid("expected a prose_val"));
    }

    let span = span_from_pair(pair);
    let text = pair.as_str();
    let value = text
        .strip_prefix('<')
        .and_then(|text| text.strip_suffix('>'))
        .ok_or_else(|| AbnfError::invalid("prose_val was not bracketed"))?
        .to_owned();
    Ok(ProseVal::new(value, span))
}

/// Convert a numeric value pair into an owned numeric value.
fn build_num_val(pair: Pair<'_, abnf::Rule>) -> Result<NumVal, AbnfError> {
    if pair.as_rule() != abnf::Rule::num_val {
        return Err(AbnfError::invalid("expected a num_val"));
    }

    let mut inner = pair.into_inner();
    let child = inner
        .next()
        .ok_or_else(|| AbnfError::invalid("num_val had no child"))?;
    if inner.next().is_some() {
        return Err(AbnfError::invalid("num_val had unexpected extra children"));
    }

    match child.as_rule() {
        abnf::Rule::bin_val => build_num_val_body(&child, NumBase::Binary, 2),
        abnf::Rule::dec_val => build_num_val_body(&child, NumBase::Decimal, 10),
        abnf::Rule::hex_val => build_num_val_body(&child, NumBase::Hexadecimal, 16),
        _ => Err(AbnfError::invalid("unexpected numeric value kind")),
    }
}

/// Convert the body of a numeric value pair into an owned numeric value.
fn build_num_val_body(
    pair: &Pair<'_, abnf::Rule>,
    base: NumBase,
    radix: u32,
) -> Result<NumVal, AbnfError> {
    let span = span_from_pair(pair);
    let text = pair.as_str();
    let body = text
        .get(1..)
        .ok_or_else(|| AbnfError::invalid("numeric value body was empty"))?;

    let value = if let Some((left, right)) = body.split_once('-') {
        if left.is_empty() || right.is_empty() {
            return Err(AbnfError::invalid(
                "numeric range endpoints must not be empty",
            ));
        }
        if left.contains('.') || right.contains('.') {
            return Err(AbnfError::invalid(
                "numeric ranges cannot contain dotted segments",
            ));
        }
        NumValue::Range(NumRange::new(
            parse_u64_radix(left, radix, "numeric range start")?,
            parse_u64_radix(right, radix, "numeric range end")?,
            span.clone(),
        ))
    } else {
        let parts = body
            .split('.')
            .map(|part| {
                if part.is_empty() {
                    Err(AbnfError::invalid("numeric segment must not be empty"))
                } else {
                    parse_u64_radix(part, radix, "numeric segment")
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        NumValue::Sequence(parts)
    };

    Ok(NumVal::new(base, value, span))
}

/// Convert a wrapper pair that contains exactly one inner item.
fn build_single_inner(
    pair: Pair<'_, abnf::Rule>,
    expected: abnf::Rule,
    context: &str,
) -> Result<Alternation, AbnfError> {
    let mut children = pair.into_inner().collect::<Vec<_>>();
    let child = take_child(&mut children, expected, context)?;
    build_alternation(child)
}

/// Find the next child pair with the requested rule.
fn take_child<'source>(
    children: &mut Vec<Pair<'source, abnf::Rule>>,
    expected: abnf::Rule,
    context: &str,
) -> Result<Pair<'source, abnf::Rule>, AbnfError> {
    let index = children
        .iter()
        .position(|child| child.as_rule() == expected)
        .ok_or_else(|| {
            AbnfError::invalid(format!(
                "{context} did not contain the expected {expected:?}"
            ))
        })?;

    Ok(children.remove(index))
}

/// Build a source span from a Pest pair.
fn span_from_pair(pair: &Pair<'_, abnf::Rule>) -> SourceSpan {
    let span = pair.as_span();
    let (start_line, start_column) = span.start_pos().line_col();
    let (end_line, end_column) = span.end_pos().line_col();
    SourceSpan::new(
        span.start()..span.end(),
        start_line,
        start_column,
        end_line,
        end_column,
    )
}

/// Parse an unsigned decimal integer.
fn parse_u64(
    value: &str,
    context: &str,
) -> Result<u64, AbnfError> {
    value
        .parse::<u64>()
        .map_err(|e| AbnfError::invalid(format!("invalid {context}: {e}")))
}

/// Parse an unsigned integer in a specific radix.
fn parse_u64_radix(
    value: &str,
    radix: u32,
    context: &str,
) -> Result<u64, AbnfError> {
    u64::from_str_radix(value, radix)
        .map_err(|e| AbnfError::invalid(format!("invalid {context}: {e}")))
}

#[cfg(test)]
mod tests {
    use crate::parse_abnf;

    #[test]
    fn parses_simple_abnf_document() {
        let source = "rule = \"abc\"\nsecond = 1*ALPHA\n";
        let document = parse_abnf(source).expect("ABNF should parse");

        assert_eq!(document.source(), source);
        assert_eq!(document.rules().len(), 2);

        let first = &document.rules()[0];
        assert_eq!(first.name().as_str(), "rule");
        assert!(matches!(
            first.operator(),
            crate::DefinitionOperator::Assign
        ));
        assert_eq!(first.expression().concatenations().len(), 1);

        let second = &document.rules()[1];
        assert_eq!(second.name().as_str(), "second");
        assert_eq!(second.expression().concatenations().len(), 1);
    }
}
